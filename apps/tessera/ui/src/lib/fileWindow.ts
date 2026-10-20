// A text file from a chat in a window of its own ("Open in a new window").
// That window is the app's own page, started as a file window instead of the
// app (main.tsx): it never connects to the server or touches the app's saved
// state, it only shows the file it was handed.
//
// * The desktop app makes the window (`file_window`, a label `file-…`) and
//   keeps the file for it until it closes (`file_window_take`).
// * A browser opens the app's own address with `?view=<token>`; the window
//   asks the window that opened it for the file. Nothing leaves the browser.
//
// Kept free of other modules: the file window loads only this, the viewer and
// the styles.

/** What a file window shows. */
export interface WindowFile {
  name: string;
  mime: string;
  text: string;
}

/** Files handed to browser windows, by token (on the window that opened them). */
const handed = new Map<string, WindowFile>();

declare global {
  interface Window {
    /** A file window asks its opener for its file (same origin only). */
    __tesseraFile?: (token: string) => WindowFile | null;
  }
}

/** This page is a file window: its token (browser) or label (desktop app); else null. */
export function fileWindowToken(): string | null {
  const label = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: unknown } } } }).__TAURI_INTERNALS__?.metadata?.currentWindow?.label;
  if (typeof label === "string") return label.startsWith("file-") ? label : null;
  const t = new URLSearchParams(location.search).get("view");
  return t && /^[a-z0-9]{8,40}$/.test(t) ? t : null;
}

/** A browser: open `file` in a window of this app. False when the browser refused (a popup blocker). */
export function openBrowserWindow(file: WindowFile): boolean {
  const token = Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, "0")).join("");
  handed.set(token, file);
  window.__tesseraFile ??= (t) => handed.get(t) ?? null;
  const url = new URL(location.href);
  url.search = `?view=${token}`;
  url.hash = "";
  const w = window.open(url.href, `tessera-file-${token}`, "popup,width=900,height=720");
  if (!w) {
    handed.delete(token);
    return false;
  }
  // Forgotten here once that window closes.
  const watch = setInterval(() => {
    if (w.closed) {
      clearInterval(watch);
      handed.delete(token);
    }
  }, 5000);
  return true;
}

/** The file for this window (desktop app: from the shell; browser: from the window that opened it). */
export async function takeWindowFile(token: string): Promise<WindowFile | null> {
  if (token.startsWith("file-") && "__TAURI_INTERNALS__" in window) {
    const { invoke } = await import("@tauri-apps/api/core");
    return (await invoke<WindowFile | null>("file_window_take").catch(() => null)) ?? null;
  }
  try {
    return window.opener?.__tesseraFile?.(token) ?? null;
  } catch {
    // The opener went elsewhere (another origin): nothing to take.
    return null;
  }
}
