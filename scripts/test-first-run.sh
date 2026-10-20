#!/bin/sh
# Test the demo without starting Tessera, desktop apps or a service manager.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d)
CHILD=""
trap '[ -z "$CHILD" ] || kill "$CHILD" 2>/dev/null || true; rm -rf "$T"' EXIT
mkdir -p "$T/build" "$T/original-home"
cat > "$T/build/tessera" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = setup ]
[ "$HOME" = "$TESSERA_DEMO_DIR/user" ]
[ "$TESSERA_HOME" = "$TESSERA_DEMO_DIR/home" ]
[ "$XDG_CONFIG_HOME" = "$TESSERA_DEMO_DIR/config" ]
[ "$XDG_RUNTIME_DIR" = "$TESSERA_DEMO_DIR/runtime" ]
[ "$DBUS_SESSION_BUS_ADDRESS" = "unix:path=$TESSERA_DEMO_DIR/runtime/no-session-bus" ]
for c in systemctl loginctl sudo launchctl; do
  [ "$(command -v "$c")" = "$TESSERA_DEMO_DIR/bin/$c" ] || exit 1
  if "$c" --version >/dev/null 2>&1; then exit 1; fi
done
EOF
chmod +x "$T/build/tessera"
export TESSERA_BIN_DIR="$T/build" TESSERA_DEMO_DIR="$T/demo" TESSERA_DEMO_PORT=18780
HOME="$T/original-home" sh "$ROOT/scripts/first-run.sh" terminal
printf 'ok  isolated home, config, runtime and service stand-ins\n'
grep -q '127.0.0.1:18780' "$T/demo/home/config.toml"
printf 'ok  chosen demo port\n'
[ -z "$(ls -A "$T/original-home")" ]
printf 'ok  original home unchanged\n'
if TESSERA_DEMO_PORT=7878 sh "$ROOT/scripts/first-run.sh" terminal >/dev/null 2>&1; then exit 1; fi
printf 'ok  production port refused\n'
mkdir "$T/not-a-demo"
printf 'keep' > "$T/not-a-demo/keep"
if TESSERA_DEMO_DIR="$T/not-a-demo" sh "$ROOT/scripts/first-run.sh" clean >/dev/null 2>&1; then exit 1; fi
[ -f "$T/not-a-demo/keep" ]
printf 'ok  clean refuses unrelated folders\n'
sh "$ROOT/scripts/first-run.sh" clean
[ ! -d "$T/demo" ]
printf 'ok  clean removes its own profile\n'
# Even the same executable is not ours without its demo environment.
mkdir -p "$T/sleeper"
ln -s "$(command -v sleep)" "$T/sleeper/tessera"
ready() {
  n=0
  while [ "$(readlink "/proc/$CHILD/exe" 2>/dev/null || true)" != "$(readlink -f "$T/sleeper/tessera")" ]; do
    n=$((n + 1))
    [ "$n" -lt 40 ] || return 1
    sleep 0.05
  done
}
HOME="$T/original-home" sh "$ROOT/scripts/first-run.sh" terminal >/dev/null
env TESSERA_HOME="$T/unrelated" "$T/sleeper/tessera" 60 &
CHILD=$!
printf '{"pid":%s}\n' "$CHILD" > "$T/demo/home/server.json"
ready
TESSERA_BIN_DIR="$T/sleeper" sh "$ROOT/scripts/first-run.sh" clean >/dev/null
kill -0 "$CHILD"
kill "$CHILD"
wait "$CHILD" 2>/dev/null || true
CHILD=""
printf 'ok  clean leaves an unrelated process alone\n'
HOME="$T/original-home" sh "$ROOT/scripts/first-run.sh" terminal >/dev/null
env TESSERA_HOME="$T/demo/home" "$T/sleeper/tessera" 60 &
CHILD=$!
printf '{"pid":%s}\n' "$CHILD" > "$T/demo/home/server.json"
ready
TESSERA_BIN_DIR="$T/sleeper" sh "$ROOT/scripts/first-run.sh" clean >/dev/null
# Check before wait: waiting for the natural 60-second exit would hide a bug.
if kill -0 "$CHILD" 2>/dev/null; then
  sleep 0.1
  if kill -0 "$CHILD" 2>/dev/null; then exit 1; fi
fi
wait "$CHILD" 2>/dev/null || true
CHILD=""
printf 'ok  clean stops its own process\n'
echo 'ALL OK (8 checks)'
