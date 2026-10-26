#!/usr/bin/env bash
# Opt-in, synthetic-only real-server benchmark. Never point this at an existing home.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
: "${TESSERA_PERF_BIN:?Set an absolute path to the tessera binary being measured}"
: "${PERF_WORK:?Set a NEW output directory outside the checkout}"
[ ! -e "$PERF_WORK" ] || { echo 'PERF_WORK must not exist' >&2; exit 1; }
mkdir -m 700 -p "$PERF_WORK"/{home,user,config,data,cache,runtime}
touch "$PERF_WORK/home/LAUNCH-PERF-THROWAWAY"
PORT=${PERF_PORT:-18101}
case "$PORT" in 1810[1-9]) ;; *) echo 'Use a throwaway port in 18101-18109' >&2; exit 1 ;; esac
cat >"$PERF_WORK/home/config.toml" <<EOF
default_model = "synthetic/synthetic"
[server]
name = "Synthetic launch"
bind = "127.0.0.1:$PORT"
web_access = true
[[provider]]
id = "synthetic"
name = "Synthetic"
kind = "openai_chat"
base_url = "http://127.0.0.1:18109/v1"
discover = false
[[provider.models]]
id = "synthetic"
context_window = 128000
EOF
server=''
cleanup() {
  if [ -n "$server" ]; then kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; fi
  if [ -f "$PERF_WORK/browser.json" ]; then
    node --input-type=module - "$PERF_WORK/browser.json" <<'JS'
import fs from 'node:fs';
try {
  const {pid,start}=JSON.parse(fs.readFileSync(process.argv[2],'utf8'));
  if(Number.isInteger(pid)&&pid>1&&start&&fs.readFileSync(`/proc/${pid}/stat`,'utf8').split(') ').pop().split(' ')[19]===start) process.kill(pid,'SIGTERM');
} catch {}
JS
  fi
}
trap cleanup EXIT INT TERM
start() {
  env HOME="$PERF_WORK/user" TESSERA_HOME="$PERF_WORK/home" XDG_CONFIG_HOME="$PERF_WORK/config" \
    XDG_DATA_HOME="$PERF_WORK/data" XDG_CACHE_HOME="$PERF_WORK/cache" XDG_RUNTIME_DIR="$PERF_WORK/runtime" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=$PERF_WORK/runtime/no-bus" \
    "$TESSERA_PERF_BIN" serve --no-scheduler "$@" >"$PERF_WORK/server.log" 2>&1 &
  server=$!
  for _ in {1..160}; do
    if curl -fsS "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then return; fi
    kill -0 "$server" || { cat "$PERF_WORK/server.log"; return 1; }; sleep .25
  done
  echo 'Throwaway server did not become healthy' >&2; return 1
}
start
kill "$server"; wait "$server" || true; server=''
full=(); [ "${LAUNCH_FULL:-1}" = 0 ] || full=(--full)
python3 "$ROOT/scripts/seed-launch.py" "$PERF_WORK/home" "${full[@]}"
(
  cd "$ROOT/apps/tessera/ui"
  PERF_DIST="$PERF_WORK/dist" node dev/build-launch-perf.mjs >"$PERF_WORK/build.log" 2>&1
)
start --web "$PERF_WORK/dist"
APP="http://localhost:$PORT/web/" LAUNCH_HOME="$PERF_WORK/home" E2E_OUT="$PERF_WORK/metrics" \
  CDP_PORT="${CDP_PORT:-9441}" E2E_BROWSER_PID_FILE="$PERF_WORK/browser.json" \
  node "$ROOT/apps/tessera/ui/tests/e2e/launch-real.mjs" | tee "$PERF_WORK/results.log"
