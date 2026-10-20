#!/bin/sh
# Try scripts/install.sh the ways people run it, in a throwaway home: your
# real ~/.local/bin, settings, data and background service are never touched
# (`systemctl` and `sudo` are stand-ins that only write down what they were
# asked, and the user service manager can't be reached).
#
#   sh scripts/test-install.sh           every case (builds Tessera twice: slow the first time)
#   sh scripts/test-install.sh quick     only the ready-made-build cases (needs one build)
#   KEEP=1 sh scripts/test-install.sh    keep the throwaway home afterwards
#
# Uses CARGO_TARGET_DIR when set, so a warm build cache makes it quick.
# TEST_INSTALL_BUILD_WRAP can serialize source builds, for example
# 'flock -o /tmp/tessera-build.lock nice -n 19 ionice -c3'. The local download
# server stays outside that wrapper and is stopped by the exit trap.
# TEST_INSTALL_PORT selects the local download server's port.
set -u
SRC="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-all}"
REAL_HOME=$HOME
T=$(mktemp -d "${TMPDIR:-/tmp}/tessera-install-test.XXXXXX")
SERVER_PID=""
fails=0
passes=0

finish() {
  status=$?
  # On GitHub, the end of each installer run as an error note when the test
  # failed: notes can be read without signing in, logs can't.
  if [ "$status" -ne 0 ] && [ "${GITHUB_ACTIONS:-}" = true ]; then
    for f in "$T"/out*.txt; do
      [ -f "$f" ] || continue
      printf '::error title=test-install %s::%s\n' "${f##*/}" \
        "$(tail -n 15 "$f" | sed 's/%/%25/g' | awk '{printf "%s%%0A", $0}')"
    done
  fi
  if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null; fi
  if [ -z "${KEEP:-}" ]; then rm -rf "$T"; else echo "kept $T"; fi
}
trap finish EXIT
trap 'exit 130' INT TERM

# install.sh also supports root/system-wide installs. This test must never
# exercise those paths or modify a writable system unit.
if [ "$(id -u)" -eq 0 ] || [ -w /etc/systemd/system/tessera.service ]; then
  echo "Run this test as an unprivileged user without a writable system Tessera unit." >&2
  exit 1
fi

check() { # CONDITION-STATUS MESSAGE
  if [ "$1" -eq 0 ]; then
    passes=$((passes + 1))
    printf '  ok   %s\n' "$2"
  else
    fails=$((fails + 1))
    printf '  FAIL %s\n' "$2"
    # On GitHub, also as an error note: those can be read without signing in.
    [ "${GITHUB_ACTIONS:-}" = true ] && printf '::error title=test-install::%s\n' "$2"
  fi
}
has() { grep -q -- "$1" "$2"; echo $?; }
hasnt() { if grep -q -- "$1" "$2"; then echo 1; else echo 0; fi; }

# The throwaway home and its stand-ins.
H="$T/home"
mkdir -p "$H/.config/systemd/user" "$H/.local/share/tessera-browser" "$T/fakebin"
cat > "$T/fakebin/systemctl" <<'EOF'
#!/bin/sh
echo "systemctl $*" >> "$FAKE_LOG"
case "$*" in
  "--user is-active --quiet tessera") exit 0 ;;   # a user service "runs"
  *is-active*) exit 3 ;;
esac
exit 0
EOF
cat > "$T/fakebin/sudo" <<'EOF'
#!/bin/sh
echo "sudo $*" >> "$FAKE_LOG"
exit 1
EOF
cp "$T/fakebin/sudo" "$T/fakebin/loginctl"
# A "browser", so no 170 MB download.
printf '#!/bin/sh\nexit 0\n' > "$H/.local/share/tessera-browser/chrome"
# Hide a browser this computer has installed (CI machines come with
# Chromium): these stand-ins look like snap launchers, which install.sh skips.
for b in chromium chromium-browser google-chrome-stable google-chrome brave-browser brave microsoft-edge-stable microsoft-edge; do
  printf '#!/bin/sh\n# stand-in for /snap/bin/%s\nexit 1\n' "$b" > "$T/fakebin/$b"
done
chmod +x "$T/fakebin/"* "$H/.local/share/tessera-browser/chrome"

# A unit written before KillMode=mixed (install.sh adds the line).
old_unit() {
  cat > "$H/.config/systemd/user/tessera.service" <<'EOF'
[Service]
ExecStart=/bin/sh -lc 'exec "$HOME/.local/bin/tessera" serve'
KillSignal=SIGINT
TimeoutStopSec=20
EOF
}

# in_home CMD…: run CMD with the throwaway home and stand-ins.
in_home() {
  env -u DBUS_SESSION_BUS_ADDRESS -u XDG_RUNTIME_DIR -u TESSERA_HOME -u DISPLAY -u WAYLAND_DISPLAY \
    -u TESSERA_BIN_DIR -u TESSERA_TOOLCHAIN -u TESSERA_REPO_URL -u TESSERA_VERSION \
    HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share" XDG_CACHE_HOME="$H/.cache" \
    RUSTUP_HOME="${RUSTUP_HOME:-$REAL_HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$REAL_HOME/.cargo}" \
    PATH="$T/fakebin:${CARGO_HOME:-$REAL_HOME/.cargo}/bin:/usr/local/bin:/usr/bin:/bin" \
    FAKE_LOG="$T/fake.log" NO_COLOR=1 "$@"
}

source_install() {
  # Intentional word splitting for the optional command prefix, not arguments.
  in_home ${TEST_INSTALL_BUILD_WRAP:-} sh scripts/install.sh --yes < /dev/null
}

BIN="$H/.local/bin"

# ── 1. From a copy of the source, as production is updated ──
if [ "$MODE" = all ]; then
  echo "— from a source tree: sh scripts/install.sh --yes </dev/null"
  mkdir -p "$T/tessera"
  (cd "$SRC" && tar --exclude=./target --exclude=node_modules --exclude=./.git --exclude=./out -cf - .) | tar -xf - -C "$T/tessera"
  old_unit
  : > "$T/fake.log"
  (cd "$T/tessera" && source_install) > "$T/out1.txt" 2>&1
  check $? "it finishes"
  "$BIN/tessera" --version > /dev/null 2>&1
  check $? "the installed tessera runs"
  check "$(has 'Building Tessera' "$T/out1.txt")" "it built this copy"
  check "$(has '^KillMode=mixed' "$H/.config/systemd/user/tessera.service")" "an old unit gets KillMode=mixed"
  check "$(has 'systemctl --user daemon-reload' "$T/fake.log")" "…and systemd reloads it"
  check "$(has 'systemctl --user restart tessera' "$T/fake.log")" "the running service is restarted"
  check "$(has 'restarted with the new version' "$T/out1.txt")" "…and it says so"
  check "$(has 'Tessera is installed' "$T/out1.txt")" "it says it's installed"
  check "$(hasnt 'Set it up now' "$T/out1.txt")" "without a terminal, setup isn't started"
  check "$(has 'Next: ' "$T/out1.txt")" "…but named as the next step"
  check "$(hasnt '^sudo' "$T/fake.log")" "no sudo"
  sed 's/^/    | /' "$T/out1.txt"
fi

# ── A release on a local server ──
if [ ! -x "$BIN/tessera" ]; then
  # quick: a build from this checkout (the target dir, else build it).
  TD="${CARGO_TARGET_DIR:-$SRC/target}"
  if [ ! -x "$TD/release/tessera" ]; then
    (cd "$SRC" && ${TEST_INSTALL_BUILD_WRAP:-} cargo build --release -p tessera) || { echo "build failed"; exit 1; }
  fi
  mkdir -p "$BIN"
  cp "$TD/release/tessera" "$BIN/tessera"
fi
REL="$T/release"
mkdir -p "$REL/pkg"
cp "$BIN/tessera" "$REL/pkg/tessera"
tar -C "$REL/pkg" -czf "$REL/tessera-linux-x86_64.tar.gz" tessera
tar -C "$REL/pkg" -czf "$REL/tessera-linux-aarch64.tar.gz" tessera
(cd "$REL" && sha256sum tessera-linux-*.tar.gz > SHA256SUMS)
PORT=${TEST_INSTALL_PORT:-$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')}
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$REL" > "$T/http.log" 2>&1 &
SERVER_PID=$!
i=0
while ! curl -fs -o /dev/null "http://127.0.0.1:$PORT/SHA256SUMS"; do
  i=$((i + 1))
  [ $i -lt 40 ] || { echo "the local release server didn't start"; exit 1; }
  sleep 0.25
done
URL="http://127.0.0.1:$PORT"
want=$(sha256sum "$REL/pkg/tessera" | cut -d' ' -f1)

# ── 2. Piped from curl: the ready-made build ──
if [ "$(uname -m)" = x86_64 ] || [ "$(uname -m)" = aarch64 ]; then
  echo "— piped (curl … | sh): the ready-made build, checksum checked"
  rm -f "$BIN/tessera"
  old_unit
  : > "$T/fake.log"
  in_home env TESSERA_DOWNLOAD_URL="$URL" sh < "$SRC/scripts/install.sh" > "$T/out2.txt" 2>&1
  check $? "it finishes"
  check "$(has 'checksum verified' "$T/out2.txt")" "it says the download matched its checksum"
  check "$(hasnt 'Building Tessera' "$T/out2.txt")" "…and doesn't build anything"
  if [ "$(sha256sum "$BIN/tessera" 2>/dev/null | cut -d' ' -f1)" = "$want" ]; then r=0; else r=1; fi
  check $r "the installed tessera is the released one"
  check "$(has '^KillMode=mixed' "$H/.config/systemd/user/tessera.service")" "an old unit gets KillMode=mixed here too"
  check "$(has 'systemctl --user restart tessera' "$T/fake.log")" "the running service is restarted"
  check "$(hasnt 'Set it up now' "$T/out2.txt")" "no terminal: no setup"
  sed 's/^/    | /' "$T/out2.txt"

  # ── 3. A download that doesn't match its checksum ──
  echo "— a download that doesn't match the checksum"
  cp "$REL/SHA256SUMS" "$T/sums.good"
  sed 's/^[0-9a-f]\{8\}/00000000/' "$T/sums.good" > "$REL/SHA256SUMS"
  printf 'old' > "$BIN/tessera"
  in_home env TESSERA_DOWNLOAD_URL="$URL" sh < "$SRC/scripts/install.sh" > "$T/out3.txt" 2>&1
  st=$?
  if [ $st -eq 1 ]; then r=0; else r=1; fi
  check $r "it stops (exit $st)"
  check "$(has "doesn't match" "$T/out3.txt")" "…saying the download doesn't match"
  if [ "$(cat "$BIN/tessera")" = old ]; then r=0; else r=1; fi
  check $r "…and installs nothing"
  cp "$T/sums.good" "$REL/SHA256SUMS"
  sed 's/^/    | /' "$T/out3.txt"

  # ── 4. In a terminal: asks, from the terminal, whether to set it up ──
  if command -v script >/dev/null 2>&1; then
    echo "— piped in a terminal: questions come from the terminal"
    # A pipe into sh, as with curl; the answer comes from the terminal.
    printf 'n\n' | in_home env TESSERA_DOWNLOAD_URL="$URL" TERM=xterm script -qec "cat '$SRC/scripts/install.sh' | sh" /dev/null > "$T/out4.txt" 2>&1
    check $? "it finishes"
    check "$(has 'Set it up now' "$T/out4.txt")" "it asks whether to set it up"
    check "$(has 'Next: ' "$T/out4.txt")" "answered no: it names the next step"
    tr -d '\r' < "$T/out4.txt" | sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' | grep -v '^\s*$' | tail -n 6 | sed 's/^/    | /'
  fi
fi

# ── 5. No ready-made build for this computer: the release's source ──
if [ "$MODE" = all ]; then
  echo "— no ready-made build here: it builds the source instead"
  grep -v "linux-$(uname -m | sed 's/amd64/x86_64/;s/arm64/aarch64/')" "$T/sums.good" > "$REL/SHA256SUMS"
  mkdir -p "$T/repo"
  (cd "$SRC" && git ls-files -z --cached --others --exclude-standard | xargs -0 tar -cf -) | tar -xf - -C "$T/repo"
  cp "$SRC/scripts/install.sh" "$T/repo/scripts/install.sh"
  git -C "$T/repo" init -q && git -C "$T/repo" add -A && git -C "$T/repo" -c user.name=t -c user.email=t@example.com commit -qm source
  in_home env TESSERA_DOWNLOAD_URL="$URL" TESSERA_REPO_URL="file://$T/repo" ${TEST_INSTALL_BUILD_WRAP:-} sh < "$SRC/scripts/install.sh" > "$T/out5.txt" 2>&1
  check $? "it finishes"
  check "$(has 'No ready-made build for' "$T/out5.txt")" "it says there is no ready-made build for this computer"
  check "$(has 'Downloading the source' "$T/out5.txt")" "…fetches the source"
  check "$(has 'Building Tessera' "$T/out5.txt")" "…and builds it"
  "$BIN/tessera" --version > /dev/null 2>&1
  check $? "the installed tessera runs"
  cp "$T/sums.good" "$REL/SHA256SUMS"
  sed 's/^/    | /' "$T/out5.txt"
fi

# ── 6. Chrome for Testing: a newer one replaces the old; the one before stays until the restart ──
case "$(uname -m)" in aarch64 | arm64) CFT=linux-arm64 ;; *) CFT=linux64 ;; esac
echo "— Chrome for Testing: updated when a newer one is out"
REAL_CURL=$(command -v curl)
# Google's two addresses are answered here (FAKE_CHROME is the "latest"
# version, FAKE_ZIP the download); everything else goes to the real curl.
cat > "$T/fakebin/curl" <<EOF
#!/bin/sh
out="" url="" prev=""
for a in "\$@"; do
  if [ "\$prev" = "-o" ]; then out=\$a; fi
  case "\$a" in https://googlechromelabs.github.io/* | https://storage.googleapis.com/*) url=\$a ;; esac
  prev=\$a
done
[ -n "\$url" ] || exec "$REAL_CURL" "\$@"
echo "curl \$*" >> "\$FAKE_LOG"
[ -n "\${FAKE_CHROME:-}" ] || exit 22
case "\$url" in
  */LATEST_RELEASE_STABLE) printf '%s' "\$FAKE_CHROME" ;;
  *) cp "\$FAKE_ZIP" "\$out" ;;
esac
EOF
chmod +x "$T/fakebin/curl"
python3 - "$T/chrome.zip" "$CFT" <<'PY'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    i = zipfile.ZipInfo(f"chrome-{sys.argv[2]}/chrome")
    i.external_attr = 0o755 << 16
    z.writestr(i, "#!/bin/sh\nexit 0\n")
PY
CB="$H/.local/share/tessera-browser"
rm -rf "${CB:?}"/*
for v in 99.0.0.1 100.0.0.1; do
  mkdir -p "$CB/$v/chrome-$CFT"
  printf '#!/bin/sh\nexit 0\n' > "$CB/$v/chrome-$CFT/chrome"
  chmod +x "$CB/$v/chrome-$CFT/chrome"
done
ln -sfn "100.0.0.1/chrome-$CFT/chrome" "$CB/chrome"
: > "$T/fake.log"
in_home env TESSERA_DOWNLOAD_URL="$URL" FAKE_CHROME=101.0.0.2 FAKE_ZIP="$T/chrome.zip" sh < "$SRC/scripts/install.sh" > "$T/out6.txt" 2>&1
check $? "it finishes"
if [ "$(readlink "$CB/chrome")" = "101.0.0.2/chrome-$CFT/chrome" ]; then r=0; else r=1; fi
check $r "the newer Chrome for Testing is the one used now"
check "$(has 'updated from 100.0.0.1' "$T/out6.txt")" "…and it says what it replaced"
if [ -d "$CB/100.0.0.1" ] && [ ! -d "$CB/99.0.0.1" ]; then r=0; else r=1; fi
check $r "the one it replaced stays (a running server may use it), older ones go"
check "$(has "storage.googleapis.com/chrome-for-testing-public/101.0.0.2/$CFT/chrome-$CFT.zip" "$T/fake.log")" "it downloaded the build for this computer"
check "$(has "proto =https" "$T/fake.log")" "…over HTTPS only"
sed 's/^/    | /' "$T/out6.txt"
: > "$T/fake.log"
in_home env TESSERA_DOWNLOAD_URL="$URL" FAKE_CHROME=101.0.0.2 FAKE_ZIP="$T/chrome.zip" sh < "$SRC/scripts/install.sh" > "$T/out7.txt" 2>&1
check $? "run again: it finishes"
check "$(has 'Chrome for Testing 101.0.0.2 · up to date' "$T/out7.txt")" "…and says the browser is up to date"
check "$(hasnt 'storage.googleapis.com' "$T/fake.log")" "…without downloading anything"
: > "$T/fake.log"
in_home env TESSERA_DOWNLOAD_URL="$URL" FAKE_CHROME="" sh < "$SRC/scripts/install.sh" > "$T/out8.txt" 2>&1
check $? "no answer about the latest one: it finishes"
check "$(has "Couldn't check for a newer one" "$T/out8.txt")" "…and says it couldn't check"
rm -f "$T/fakebin/curl"

echo
if [ $fails -eq 0 ]; then echo "ALL OK ($passes checks)"; else echo "$fails FAILURE(S), $passes ok"; fi
[ $fails -eq 0 ]
