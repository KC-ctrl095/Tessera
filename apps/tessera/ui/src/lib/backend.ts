import { TauriTransport, WebTransport, WsTransport, type Backend } from "./transport";
import { isTauri } from "./util";

// Browser mode normally talks to the server that served the page (only a
// pairing code is needed). `?servers` (or localStorage `tessera.multiServer=1`)
// enables the full multi-server onboarding with direct endpoints — useful when
// the UI is hosted separately and for testing the app's onboarding in a browser.
function multiServerMode(): boolean {
  try {
    const q = new URLSearchParams(location.search);
    if (q.has("servers")) {
      localStorage.setItem("tessera.multiServer", q.get("servers") === "0" ? "0" : "1");
    }
    return localStorage.getItem("tessera.multiServer") === "1";
  } catch {
    return false;
  }
}

/** This page was given by "Use Tessera in a web browser" (`/web/` on the server's address). */
export const webAccess = !isTauri && /^\/web(\/|$)/.test(location.pathname);

export const backend: Backend = isTauri ? new TauriTransport() : webAccess ? new WebTransport() : new WsTransport(!multiServerMode());
