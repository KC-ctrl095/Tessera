// Adding a device: a one-time pairing code and its QR code (Devices and the setup wizard).
import { Show, createEffect, createSignal, onCleanup } from "solid-js";
import type { PairingCode } from "../../proto/PairingCode";
import { api } from "../../lib/store";
import { toast, toastError } from "../../lib/ui";
import { copyText } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { QrCode } from "../QrCode";
import { Card } from "./kit";

/** A one-time pairing code, with its QR code (the link only ever appears here). */
export function PairPanel(props: { autostart?: boolean }) {
  const [code, setCode] = createSignal<PairingCode | null>(null);
  const [issuedAt, setIssuedAt] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  const [now, setNow] = createSignal(Date.now());
  // The countdown ticks only while a code is showing and still valid.
  let t: ReturnType<typeof setInterval> | undefined;
  createEffect(() => {
    const c = code();
    clearInterval(t);
    t = undefined;
    setNow(Date.now());
    if (c && c.expires_at > Date.now())
      t = setInterval(() => {
        setNow(Date.now());
        if (Date.now() >= c.expires_at) clearInterval(t);
      }, 1000);
  });
  onCleanup(() => clearInterval(t));
  const remaining = () => (code() ? Math.max(0, code()!.expires_at - now()) : 0);
  const total = () => (code() ? Math.max(60_000, code()!.expires_at - issuedAt()) : 600_000);

  const pair = async () => {
    setBusy(true);
    try {
      const c = await api.createPairing();
      setIssuedAt(Date.now());
      setCode(c);
    } catch (e) {
      toastError(e, "Could not create a pairing code");
    } finally {
      setBusy(false);
    }
  };
  if (props.autostart) void pair();
  const pretty = (c: string) => (/^\d{4}-\d{4}$/.test(c) ? c.replace("-", " ") : c.replace(/^(\d{3})(\d{3})$/, "$1 $2"));

  return (
    <Card class="pair-card">
      <Show
        when={code() && remaining() > 0}
        fallback={
          <div class="pair-cta">
            <div>
              <div class="setting-title">Add a device</div>
              <div class="setting-help">Get a one-time code, then enter it in Tessera on your phone, laptop or browser.</div>
            </div>
            <button type="button" class="btn btn-primary" disabled={busy()} onClick={pair}>
              <Show when={busy()} fallback={<Icon name="plus" size={15} />}>
                <Spinner size={14} />
              </Show>
              {code() ? "New code" : "Get a code"}
            </button>
          </div>
        }
      >
        <div class="pair-layout" classList={{ "has-qr": !!code()!.link }}>
          {/* The link carries proxy headers and a live code: it is only ever shown here, as a QR code. */}
          <Show when={code()!.link}>
            <figure class="pair-qr">
              <QrCode value={code()!.link!} size={232} label="Pairing QR code" />
              <figcaption>
                <Icon name="phone" size={14} /> Scan with the Tessera app on your phone
              </figcaption>
            </figure>
          </Show>
          <div class="pair-code">
            <div class="muted small center-text">{code()!.link ? "Or enter this code on the other device" : "Enter this code on the other device"}</div>
            <div class="code-big mono" aria-live="polite">
              {pretty(code()!.code)}
            </div>
            <div class="pair-expires muted small tabular">
              Expires in {Math.floor(remaining() / 60000)}:{String(Math.floor((remaining() % 60000) / 1000)).padStart(2, "0")}
            </div>
            <div class="pair-expiry">
              <span style={{ transform: `scaleX(${Math.min(1, remaining() / total())})` }} />
            </div>
            <div class="pair-copies">
              <button
                type="button"
                class="link-btn pair-copy-code"
                onClick={async () => {
                  if (await copyText(code()!.code)) toast({ level: "success", message: "Code copied", timeout: 1800 });
                }}
              >
                <Icon name="copy" size={13} /> Copy code
              </button>
              {/* For a computer without a camera: the whole link, to paste in its Tessera app. */}
              <Show when={code()!.link}>
                <button
                  type="button"
                  class="link-btn pair-copy-link"
                  onClick={async () => {
                    if (await copyText(code()!.link!)) toast({ level: "success", message: "Pairing link copied. Keep it private — paste it only into Tessera.", timeout: 3500 });
                  }}
                >
                  <Icon name="link" size={13} /> Copy pairing link
                </button>
              </Show>
            </div>
            <Show when={code()!.fingerprint}>
              <div class="fp-row center">
                <Icon name="lock" size={14} />
                <span>
                  Server key <span class="mono fp">{code()!.fingerprint}</span>
                </span>
              </div>
              <p class="muted small center-text">Your device will show the same key after pairing — that's how you know nobody is in between.</p>
            </Show>
          </div>
        </div>
      </Show>
    </Card>
  );
}
