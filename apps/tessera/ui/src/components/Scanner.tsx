// Scanning a pairing QR code (phone app). The barcode plugin runs the camera
// behind the webview ("windowed": the webview turns transparent), and this
// draws the viewfinder over it: the app hidden, a dimmed surround with a
// clear square to aim with, corner brackets, a hint, and Cancel. The plugin
// has no torch control, so there is no torch button.
import { Show, createSignal } from "solid-js";
import { render } from "solid-js/web";
import "../styles/setup.css";
import { Icon, Spinner } from "./icons";

type Phase = "starting" | "live" | "found";

function Viewfinder(props: { phase: () => Phase; onCancel: () => void }) {
  return (
    <div class="scanner" classList={{ live: props.phase() !== "starting", found: props.phase() === "found" }} role="dialog" aria-modal="true" aria-label="Scan the pairing QR code">
      <div class="scanner-hint">
        <div class="scanner-title">Point at the QR code</div>
        <div class="scanner-sub">
          from <code>tessera pair</code> or Settings → Devices
        </div>
      </div>
      <div class="scanner-frame" aria-hidden="true">
        <i class="tl" />
        <i class="tr" />
        <i class="bl" />
        <i class="br" />
        <span class="scanner-line" />
        <Show when={props.phase() === "found"}>
          <span class="scanner-found">
            <Spinner size={22} />
          </span>
        </Show>
      </div>
      <button type="button" class="scanner-cancel" disabled={props.phase() === "found"} onClick={props.onCancel}>
        <Icon name="x" size={18} />
        Cancel
      </button>
    </div>
  );
}

/**
 * Scan one QR code with the viewfinder up. Resolves with its text, or null
 * when cancelled (the button or Android's Back). Camera permission must be
 * granted already.
 */
export async function scanQr(): Promise<string | null> {
  const bs = await import("@tauri-apps/plugin-barcode-scanner");
  const [phase, setPhase] = createSignal<Phase>("starting");
  // The plugin's cancel never settles the pending scan, so race it.
  let stop = () => {};
  const cancelled = new Promise<null>((resolve) => (stop = () => resolve(null)));
  const cancel = () => {
    stop();
    void bs.cancel().catch(() => {});
  };
  const host = document.createElement("div");
  host.className = "scanner-host";
  document.body.appendChild(host);
  const dispose = render(() => <Viewfinder phase={phase} onCancel={cancel} />, host);
  const root = document.documentElement;
  // Android's Back button would otherwise close the app while the camera is up.
  let back: { unregister: () => Promise<void> } | null = null;
  try {
    const { onBackButtonPress } = await import("@tauri-apps/api/app");
    back = await onBackButtonPress(cancel);
  } catch {
    /* not on Android */
  }
  try {
    const scan = bs.scan({ formats: [bs.Format.QRCode], windowed: true });
    // The page turns see-through once the camera is asked for; the viewfinder
    // stays dark a moment longer, until the first frames are in.
    root.classList.add("scanning");
    setTimeout(() => phase() === "starting" && setPhase("live"), 350);
    const res = await Promise.race([scan, cancelled]);
    if (!res) return null;
    // Cover the camera at once (the plugin puts the webview's background back
    // as it stops), then let the app show through again.
    setPhase("found");
    return (res.content ?? "").trim();
  } finally {
    void back?.unregister().catch(() => {});
    root.classList.remove("scanning");
    // A short fade (the CSS) before the viewfinder goes.
    host.classList.add("closing");
    setTimeout(() => {
      dispose();
      host.remove();
    }, 160);
  }
}
