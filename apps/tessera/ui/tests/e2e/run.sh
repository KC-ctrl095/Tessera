#!/bin/sh
# Browser end-to-end suites: the app (served by Vite from src/) against the
# mock server (dev/mock-server.mjs), driven in headless Chromium over CDP.
#
#   sh tests/e2e/run.sh              every suite
#   sh tests/e2e/run.sh smoke paste  only these
#
# Each suite gets a fresh mock. Ports: VITE_PORT (5191), MOCK_PORT (7891),
# CDP_PORT (9347). Screenshots, logs and the browser profile go to E2E_OUT
# (default: $TMPDIR/tessera-e2e). Needs Node 22+ and /usr/bin/chromium.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
UI="$(cd "$HERE/../.." && pwd)"
VITE_PORT="${VITE_PORT:-5191}"
MOCK_PORT="${MOCK_PORT:-7891}"
export CDP_PORT="${CDP_PORT:-9347}"
export E2E_OUT="${E2E_OUT:-${TMPDIR:-/tmp}/tessera-e2e}"
export APP="http://localhost:$VITE_PORT/" MOCK="http://127.0.0.1:$MOCK_PORT"
mkdir -p "$E2E_OUT/logs"
# Unique to this runner: never kill a browser from another test or a reused PID.
export E2E_BROWSER_PID_FILE="$E2E_OUT/browser-$$.json"
# sync-delete also runs the fresh-device cases from sync-fresh.mjs.
SUITES="${*:-smoke approvals composer composer-polish queue security bot extras completion learned skills sync-delete stale-rpc typing-sync stale-chat board-place drag wb-all rewind rewind-lazy sync-lock features mobile menu-frame preview tiles tiles-touch swipe tile-header notes notes-edit app-races note-drafts files-open webview webview-files webview-attach webview-mosaic webview-settings webview-host panels settings-security credentials secret-saving setup claude claude-tools presentation machines machines-platform devices paste stuck chat-ui chat-agents helpers stop-helpers helper-display helper-history ui-busy transcript-perf bot-history handoff-history freedom bot-access notify-files camera harness stream phone-lag phone-restore agent-tiles settings-nav viewer phone-fixes visual sidebar-open webview-grab toasts usage offline-models newfolder lazy-styles options-lazy recall-lazy voice-lazy voice-intent reload-safety small-mode passwords-machines follow follow-resize follow-layout note-access web-access webview-screen web-browser terminal terminal-races terminal-input terminal-appearance simple homes settings-homes bot-work help-legal layers usage-days private-device accounts app-updates remote-cf claude-native notification-inbox notification-policy webview-controls notes-list-lazy notes-startup desktop-ui polish polish-safety polish-targets voice context-window provider-presets daybreak launch-smoke fresh-install history-paging assistant session-list}"

mock_pid=""
# The browser is shared across suites. A later native-scrollbar test cannot undo the launch flag.
case " $SUITES " in *" follow "*) export E2E_SCROLLBARS=1 ;; esac

stop_mock() { [ -n "$mock_pid" ] && kill "$mock_pid" 2>/dev/null; mock_pid=""; }
suite_pid=""
cleanup() {
  [ -n "$suite_pid" ] && kill "$suite_pid" 2>/dev/null
  stop_mock
  [ -n "${vite_pid:-}" ] && kill "$vite_pid" 2>/dev/null
  # Only the browser spawned by this runner's suites, if still the same PID.
  # JavaScript template literals, not shell interpolation.
  # shellcheck disable=SC2016
  node --input-type=module -e '
    import fs from "node:fs";
    try {
      const { pid, start } = JSON.parse(fs.readFileSync(process.env.E2E_BROWSER_PID_FILE, "utf8"));
      if (!Number.isInteger(pid) || pid <= 1 || !start) process.exit(0);
      const now = fs.readFileSync(`/proc/${pid}/stat`, "utf8").split(") ").pop().split(" ")[19];
      if (now === start) process.kill(pid, "SIGTERM");
    } catch {}
  '
  rm -f "$E2E_BROWSER_PID_FILE"
}
trap cleanup EXIT
# Stopped (Ctrl+C, kill): stop the suite running now too, and don't go on to the next one.
trap 'exit 130' INT TERM
# wait_port URL [tries]: every 0.25 s (default 20 s; Vite's first start after npm ci prepares its
# dependencies, which takes longer on a busy machine).
wait_port() { i=0; while [ $i -lt "${2:-80}" ]; do curl -s -o /dev/null "$1" && return 0; sleep 0.25; i=$((i + 1)); done; return 1; }

# Polling: file-change notifications don't arrive on network mounts, and a
# stale module would make a suite test old code.
(cd "$UI" && CHOKIDAR_USEPOLLING=1 TESSERA_BACKEND="$MOCK" exec node node_modules/vite/bin/vite.js --port "$VITE_PORT" --strictPort) > "$E2E_OUT/logs/vite.log" 2>&1 &
vite_pid=$!
wait_port "$APP" 360 || { echo "vite didn't start (see $E2E_OUT/logs/vite.log)"; exit 1; }

failed=0
for t in $SUITES; do
  if [ -n "${E2E_BEFORE_SUITE:-}" ]; then
    bash "$E2E_BEFORE_SUITE" || { failed=1; break; }
  fi
  stop_mock
  (cd "$UI" && PORT="$MOCK_PORT" QUIET=1 exec node dev/mock-server.mjs) > "$E2E_OUT/logs/mock-$t.log" 2>&1 &
  mock_pid=$!
  wait_port "$MOCK/api/health" || { echo "mock didn't start for $t"; failed=1; continue; }
  log="$E2E_OUT/logs/$t.log"
  # In the background and waited for, so a stop signal is handled at once.
  # The 4x CPU baseline deliberately visits a whole multi-thousand-entry chat.
  timeout_s=900
  [ "$t" = transcript-perf ] && timeout_s=1800
  (cd "$HERE" && exec timeout "$timeout_s" node "$t.mjs") > "$log" 2>&1 &
  suite_pid=$!
  status=0
  wait "$suite_pid" || status=$?
  suite_pid=""
  ok=$(grep -cE '^  ok' "$log")
  verdict=$(grep -E 'ALL OK|FAILURE|SCRIPT ERROR' "$log" | head -1 | cut -c1-140)
  case "$verdict" in "ALL OK"*) [ "$status" -eq 0 ] || failed=1 ;; *) failed=1 ;; esac
  printf "%-18s %3s ok  exit=%s  %s\n" "$t" "$ok" "$status" "${verdict:-(no verdict: see $log)}"
done
exit $failed
