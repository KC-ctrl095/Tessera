// QR code drawn as SVG: black modules on a white card with a 4-module quiet
// zone, so phone cameras read it in both themes. The value only ever feeds
// the encoder; nothing from it reaches the DOM as markup.
import { Show, createMemo, createResource } from "solid-js";
import { Spinner } from "./icons";

// The encoder is only needed on the pairing screen: load it on demand.
let encoder: Promise<typeof import("uqr")> | null = null;
const loadEncoder = () => (encoder ??= import("uqr"));

const QUIET = 4;

export function QrCode(props: { value: string; size?: number; label?: string }) {
  const [lib] = createResource(loadEncoder);
  const qr = createMemo(() => {
    const encode = lib()?.encode;
    if (!encode) return null;
    // Long links (many addresses and headers) get the lighter error correction to stay scannable.
    const ecc = props.value.length > 600 ? "L" : "M";
    const { data, size } = encode(props.value, { ecc, border: 0 });
    let d = "";
    for (let y = 0; y < size; y++) {
      const row = data[y];
      for (let x = 0; x < size; x++) {
        if (!row[x]) continue;
        // Merge horizontal runs into one rect-shaped subpath.
        let run = 1;
        while (x + run < size && row[x + run]) run++;
        d += `M${x + QUIET} ${y + QUIET}h${run}v1h-${run}z`;
        x += run - 1;
      }
    }
    return { d, dim: size + QUIET * 2 };
  });
  return (
    <div class="qr-card" style={{ width: `${props.size ?? 240}px`, height: `${props.size ?? 240}px` }}>
      <Show when={qr()} fallback={<div class="qr-wait"><Spinner size={18} /></div>}>
        {(q) => (
          <svg viewBox={`0 0 ${q().dim} ${q().dim}`} width="100%" height="100%" shape-rendering="crispEdges" role="img" aria-label={props.label ?? "QR code"}>
            <rect width={q().dim} height={q().dim} fill="#ffffff" />
            <path d={q().d} fill="#000000" />
          </svg>
        )}
      </Show>
    </div>
  );
}
