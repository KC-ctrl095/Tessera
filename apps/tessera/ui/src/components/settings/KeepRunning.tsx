// Settings → Devices → this computer (the desktop app, connected to the server it runs here):
// "Keep Tessera running", what setup's tick box chose, changed later. On Windows it's a startup
// program of the user's own (`tessera service install`), on Linux a user service.
import { Show, createResource, createSignal } from "solid-js";
import { activeServer } from "../../lib/app";
import { localServerDetect, localServerKeepRunning, localServerSetKeepRunning } from "../../lib/native";
import { errorMessage, toast } from "../../lib/ui";
import { Spinner } from "../icons";
import { Toggle } from "../primitives";

/** This app runs on Windows (the server it started here does too). */
const onWindows = () => /win/i.test(navigator.platform);

/** `localhost`, `127.x.x.x`, `[::1]`: the server on this very computer. */
function loopback(url: string): boolean {
  try {
    const h = new URL(url).hostname.replace(/^\[|\]$/g, "");
    return h === "localhost" || h === "::1" || /^127\./.test(h);
  } catch {
    return false;
  }
}

/** Same words as setup's tick box (Setup.tsx), so the choice reads the same in both places. */
function help(on: boolean): string {
  if (onWindows())
    return on
      ? "It keeps running with this window closed, and this PC stays awake while it's plugged in, so your phone can reach it any time. The screen still turns off."
      : "It runs until you restart or shut down this PC. Your phone can't reach it while the PC is asleep or off.";
  return on
    ? "It starts whenever you log in to this computer, and keeps running with this window closed."
    : "It runs until you restart this computer or log out. Your phone can't reach it while it's off.";
}

export function KeepRunning() {
  const [info, { mutate, refetch }] = createResource(async () => {
    const here = await localServerDetect().catch(() => null);
    if (!here?.running || !here.saved || !activeServer()?.endpoints.some(loopback)) return null;
    return (await localServerKeepRunning().catch(() => null)) ?? null;
  });
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const set = async (on: boolean) => {
    setError(null);
    setBusy(true);
    try {
      const s = await localServerSetKeepRunning(on);
      if (s) mutate(s);
      toast({
        level: "success",
        message: on ? "Tessera now starts by itself when you sign in" : "Tessera no longer starts by itself; it runs until this computer restarts",
        timeout: 3200,
      });
    } catch (e) {
      setError(errorMessage(e));
      void refetch();
    } finally {
      setBusy(false);
    }
  };
  const title = () => (onWindows() ? "Keep Tessera running: start it when I sign in to Windows" : "Start Tessera when I log in");
  return (
    <Show when={info()?.supported ? info() : undefined}>
      {(k) => (
        <div class="dev-sec keep-running">
          <div class="dev-sec-title">Tessera on this computer</div>
          <div class="setting-row">
            <div class="setting-text">
              <div class="setting-title">{title()}</div>
              <div class="setting-help">{help(k().installed)}</div>
            </div>
            <Toggle checked={k().installed} disabled={busy() || k().system} onChange={(v) => void set(v)} label={title()} />
          </div>
          <Show when={busy()}>
            <p class="muted small keep-busy">
              <Spinner size={12} /> Tessera restarts once; chats that were working carry on.
            </p>
          </Show>
          <Show when={k().system}>
            <p class="muted small">
              It runs as a system service here. Change that in a terminal: <code>sudo tessera service uninstall</code>.
            </p>
          </Show>
          <Show when={error()}>
            <div class="form-error">{error()}</div>
          </Show>
        </div>
      )}
    </Show>
  );
}
