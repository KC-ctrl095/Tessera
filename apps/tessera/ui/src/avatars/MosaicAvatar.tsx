// A bot's avatar: a mosaic portrait (`m:<id>`), or an emoji / short text set on a small tile field.
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import "../styles/avatars.css";
import { designFor, mosaicId } from "./catalog";
import { designBackground, fieldDesign, FALLBACK_DESIGN, requestDesignUrl } from "./render";

const EMOJI_RE = /\p{Extended_Pictographic}/u;
const HEX_RE = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i;

export interface MosaicAvatarProps {
  /** Stored avatar: `m:<id>`, or an emoji / up to 4 characters of text. */
  avatar: string;
  /** Size in CSS pixels (default 32). */
  size?: number;
  /** The bot's accent colour: tints emoji/text fields and draws a thin ring. */
  color?: string;
  class?: string;
  /** Draw the accent ring (default: whenever `color` is given). */
  ring?: boolean;
  /** Accessible name (default "Mosaic portrait", or the emoji/text). "" hides it from assistive tech. */
  label?: string;
  title?: string;
}

export function MosaicAvatar(props: MosaicAvatarProps) {
  const size = () => props.size ?? 32;
  const value = () => (props.avatar ?? "").trim();
  const id = () => mosaicId(value());
  /** Mosaic design for `m:` values (a neutral tile for unknown ids); null for emoji/text. */
  const design = () => {
    const i = id();
    return i === null ? null : (designFor(i) ?? FALLBACK_DESIGN);
  };
  const glyph = () => (id() === null ? Array.from(value()).slice(0, 4).join("") : "");
  const emoji = () => EMOJI_RE.test(glyph());
  const color = () => (props.color && HEX_RE.test(props.color) ? props.color : undefined);

  const [url, setUrl] = createSignal<string | null>(null);
  createEffect(() => {
    const d = design() ?? fieldDesign(color());
    const small = size() <= 40;
    let live = true;
    setUrl(requestDesignUrl(d, small, (u) => live && setUrl(u)));
    onCleanup(() => (live = false));
  });

  const label = () => (props.label !== undefined ? props.label : id() !== null ? "Mosaic portrait" : glyph() || "Avatar");
  const fontSize = () => {
    const n = Array.from(glyph()).length;
    if (emoji()) return size() * (n > 1 ? 0.44 : 0.56);
    return size() * (n <= 1 ? 0.5 : n === 2 ? 0.4 : 0.3);
  };

  return (
    <span
      class={"mav" + (props.class ? " " + props.class : "")}
      classList={{ "mav-ring": (props.ring ?? true) && !!color() }}
      style={{
        width: `${size()}px`,
        height: `${size()}px`,
        "--mav-c": color(),
        "--mav-bg": designBackground(design() ?? fieldDesign(color())),
        "--mav-rw": size() >= 32 ? "1.5px" : "1px",
      }}
      role={label() ? "img" : undefined}
      aria-label={label() || undefined}
      aria-hidden={label() ? undefined : "true"}
      title={props.title}
    >
      <Show when={url()}>{(u) => <img class="mav-img" src={u()} alt="" draggable={false} decoding="async" />}</Show>
      <Show when={glyph()}>
        <span class="mav-text" classList={{ "is-emoji": emoji() }} style={{ "font-size": `${fontSize()}px` }} aria-hidden="true">
          {glyph()}
        </span>
      </Show>
    </span>
  );
}
