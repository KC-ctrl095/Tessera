// A chat that looks live (working, just started, streaming) is checked
// against the server when it has heard nothing for a while, and at once when
// the app comes back to the front. Events can go missing without the
// connection dropping: the server skips events for a client that falls behind,
// a phone's web view can miss them while in the background, and a snapshot can
// cross paths with a turn's last events. Nothing else would correct the
// screen then; the activity line would stay up with its clock stopped.
import { backend } from "./backend";
import { isBusy, loadSession, state, type SessionState } from "./store";

/** Silence after which a live chat is checked (a model may legitimately take longer to answer). */
export const QUIET_MS = 15_000;
const TICK_MS = 5_000;

const heard = new Map<string, number>();
const checked = new Map<string, number>();

backend.onEvent((e) => {
  const id = "session_id" in e ? e.session_id : e.type === "session_updated" ? e.session.id : undefined;
  if (id) heard.set(id, Date.now());
});

const looksLive = (s: SessionState) => isBusy(s.info) || (!!s.streaming && !s.streaming.ended);

/** Check live chats that went quiet (`now`: all of them, e.g. back from the background). */
export function checkLive(now = false) {
  if (state.conn.state !== "connected" || !state.ready) return;
  const t = Date.now();
  for (const s of Object.values(state.sessions)) {
    if (!s?.loaded || !looksLive(s)) continue;
    const id = s.info.id;
    const last = Math.max(heard.get(id) ?? 0, checked.get(id) ?? 0, s.turnStart ?? 0, s.streaming?.startedAt ?? 0);
    if (!now && t - last < QUIET_MS) continue;
    checked.set(id, t);
    void loadSession(id, true).catch(() => {});
  }
}

let started = false;
/** Start watching (once; the activity line does it). */
export function watchLiveness() {
  if (started) return;
  started = true;
  setInterval(() => {
    if (document.visibilityState === "visible") checkLive();
  }, TICK_MS);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") checkLive(true);
  });
  window.addEventListener("pageshow", (e) => {
    if (e.persisted) checkLive(true);
  });
}
