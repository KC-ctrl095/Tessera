#!/bin/sh
# Try the first run with an isolated home. No real settings or services are used.
#
#   sh scripts/first-run.sh            the desktop app's first run
#   sh scripts/first-run.sh terminal   `tessera setup` in this terminal
#   sh scripts/first-run.sh clean      stop the demo server and delete its profile
#
# TESSERA_BIN_DIR chooses a build. TESSERA_DEMO_DIR chooses the profile folder.
# TESSERA_DEMO_PORT chooses the port (default 7979, never the normal port 7878).
set -eu

DIR="${TESSERA_DEMO_DIR:-${TMPDIR:-/tmp}/tessera-first-run}"
BIN="${TESSERA_BIN_DIR:-$HOME/.local/bin}"
BIN=$(cd "$BIN" && pwd)
PORT="${TESSERA_DEMO_PORT:-7979}"
case "$PORT" in '' | *[!0-9]*) echo "Choose a port number with TESSERA_DEMO_PORT." >&2; exit 1 ;; esac
[ "$PORT" -ge 1024 ] && [ "$PORT" -le 65535 ] && [ "$PORT" -ne 7878 ] || {
  echo "Choose a port from 1024 to 65535, other than 7878." >&2; exit 1;
}
case "${1:-desktop}" in desktop | terminal | clean) ;; *) echo "Use desktop, terminal or clean." >&2; exit 1 ;; esac

# A marker prevents an accidental 'clean' from deleting an unrelated folder.
if [ "${1:-}" = clean ]; then
  [ -f "$DIR/.tessera-demo" ] || { echo "Not a demo profile: $DIR" >&2; exit 1; }
  DIR=$(cd "$DIR" && pwd -P)
  if [ -f "$DIR/home/server.json" ]; then
    pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$DIR/home/server.json")
    # The same binary alone is not enough: a recycled PID could be a real server.
    if [ -n "$pid" ] && [ "$(readlink "/proc/$pid/exe" 2>/dev/null || true)" = "$(readlink -f "$BIN/tessera")" ] &&
      tr '\000' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -Fxq "TESSERA_HOME=$DIR/home"; then
      kill "$pid" 2>/dev/null || true
    fi
  fi
  rm -rf -- "$DIR"
  echo "Removed the demo profile ($DIR)."
  exit 0
fi

if [ -e "$DIR" ] && [ ! -f "$DIR/.tessera-demo" ] && [ -n "$(ls -A "$DIR")" ]; then
  echo "That folder is not a demo profile. Choose an empty TESSERA_DEMO_DIR." >&2
  exit 1
fi
umask 077
mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd -P)
[ "$DIR" != / ] && [ "$DIR" != "$HOME" ] || exit 1
: > "$DIR/.tessera-demo"
mkdir -p "$DIR/home" "$DIR/user" "$DIR/data" "$DIR/config" "$DIR/cache" "$DIR/runtime" "$DIR/bin"
chmod 700 "$DIR" "$DIR/runtime"
if [ ! -f "$DIR/home/config.toml" ]; then
  printf '[server]\nbind = "127.0.0.1:%s"\n' "$PORT" > "$DIR/home/config.toml"
fi

# A custom TESSERA_HOME does not isolate the real user service manager.
# Block service and privilege commands as well as isolating every home directory.
for command in systemctl loginctl sudo launchctl; do
  cat > "$DIR/bin/$command" <<'EOF'
#!/bin/sh
echo "Background services and administrator changes are disabled in the demo." >&2
exit 1
EOF
  chmod 700 "$DIR/bin/$command"
done
export HOME="$DIR/user"
export TESSERA_HOME="$DIR/home"
export XDG_DATA_HOME="$DIR/data"
export XDG_CONFIG_HOME="$DIR/config"
export XDG_CACHE_HOME="$DIR/cache"
export XDG_RUNTIME_DIR="$DIR/runtime"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$DIR/runtime/no-session-bus"
export PATH="$DIR/bin:$PATH"

echo "Demo profile: $DIR"
echo "Background services are disabled. In the desktop app, untick start at login."
echo "When you're done: TESSERA_DEMO_DIR='$DIR' sh scripts/first-run.sh clean"
case "${1:-desktop}" in
  terminal) exec "$BIN/tessera" setup ;;
  desktop) exec "$BIN/tessera-desktop" ;;
esac
