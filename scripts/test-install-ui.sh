#!/bin/sh
# Exercise the installer's UI build in a throwaway copy, without installing
# binaries, starting services, or touching the deployed source.
set -eu
SRC=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/repo" "$T/bin" "$T/home"
(cd "$SRC" && git ls-files -z | xargs -0 tar -cf -) | tar -xf - -C "$T/repo"
cp "$SRC/scripts/install.sh" "$T/repo/scripts/install.sh"
sed '$d' "$T/repo/scripts/install.sh" > "$T/functions.sh"
export HOME="$T/home" XDG_CACHE_HOME="$T/home/cache" XDG_DATA_HOME="$T/home/data" XDG_CONFIG_HOME="$T/home/config"
export CALLS="$T/calls" FAIL_CI="" REAL_NPM=""
if [ "${1:-}" = real ]; then REAL_NPM=$(command -v npm); export REAL_NPM; fi
cat > "$T/bin/npm" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CALLS"
[ -z "$FAIL_CI" ] || [ "$1" != ci ] || exit 1
if [ -n "$REAL_NPM" ]; then exec "$REAL_NPM" "$@"; fi
case "$1" in
  ci) rm -rf node_modules; mkdir node_modules; cp package-lock.json node_modules/installed-lock ;;
  run) cmp package-lock.json node_modules/installed-lock ;;
esac
EOF
chmod +x "$T/bin/npm"
export PATH="$T/bin:$PATH"
. "$T/functions.sh"
setup_output
mkdir -p "$CACHE"
: > "$LOG"
: > "$CALLS"
ROOT="$T/repo"
UI="$ROOT/apps/tessera/ui"
checks=0
check() { "$@"; checks=$((checks + 1)); }
count_ci() { [ "$(grep -c '^ci ' "$CALLS")" -eq "$1" ]; }
build_web_app
check test -n "$WEB_APP"
check count_ci 1
check test -s "$UI/node_modules/.tessera-package-lock.sha256"
build_web_app
check count_ci 1
# A lockfile change, even with node_modules present, must refresh packages.
printf '\n' >> "$UI/package-lock.json"
build_web_app
check count_ci 2
check test "$(cat "$UI/node_modules/.tessera-package-lock.sha256")" = "$(sha256_of "$UI/package-lock.json")"
# An older installer left no marker: refresh once, including desktop fallback.
rm "$UI/node_modules/.tessera-package-lock.sha256"
run "Building the desktop app's interface" build_interface "$ROOT"
check count_ci 3
run "Building the desktop app's interface" build_interface "$ROOT"
check count_ci 3
rm -rf "$UI/node_modules"
run "Building the interface" build_interface "$ROOT"
check count_ci 4
# A failed ci must not mark the new lockfile installed or start a build.
printf '\n' >> "$UI/package-lock.json"
old=$(cat "$UI/node_modules/.tessera-package-lock.sha256")
export FAIL_CI=1
if build_interface "$ROOT" >> "$LOG" 2>&1; then echo 'failed ci was accepted' >&2; exit 1; fi
check test "$(cat "$UI/node_modules/.tessera-package-lock.sha256")" = "$old"
export FAIL_CI=""
run "Retrying the interface" build_interface "$ROOT"
check count_ci 6
check test "$(cat "$UI/node_modules/.tessera-package-lock.sha256")" = "$(sha256_of "$UI/package-lock.json")"
printf 'ALL OK (%s checks)\n' "$checks"
