// Choose a bot's avatar: a grid of mosaic portraits in three sections, a shuffle button, and
// (tucked away at the bottom) an emoji or short text instead.
import { createSignal, For, Show } from "solid-js";
import "../styles/avatars.css";
import { ALL_AVATARS, CATEGORY_LABEL, designFor, isMosaicAvatar, mosaicId, randomAvatar, type AvatarCategory, type AvatarInfo } from "./catalog";
import { MosaicAvatar } from "./MosaicAvatar";

export interface AvatarPickerProps {
  value: string;
  onChange: (v: string) => void;
  /** Bot accent colour, used to tint an emoji preview. */
  color?: string;
  class?: string;
}

const EMOJI = ["✨", "🦉", "🐬", "🦁", "🐙", "🦊", "🐝", "🐢", "🌿", "🌙", "☀️", "⚡", "🔥", "🌊", "🏛️", "📜", "🎭", "🧭", "🔭", "🛠️", "🧪", "📚", "🎵", "🍇"];
const SECTIONS: AvatarCategory[] = ["figure", "creature", "emblem"];
/** Keep what the server keeps: the first 4 characters (code points). */
const clip = (s: string) => Array.from(s.trim()).slice(0, 4).join("");

export function AvatarPicker(props: AvatarPickerProps) {
  let root!: HTMLDivElement;
  const selected = () => {
    const id = mosaicId(props.value);
    return id && designFor(id) ? id : null;
  };
  const usingText = () => !isMosaicAvatar(props.value) && props.value.trim() !== "";
  const [emojiOpen, setEmojiOpen] = createSignal(usingText());
  const [draft, setDraft] = createSignal(usingText() ? props.value.trim() : "");
  const [focused, setFocused] = createSignal<string | null>(null);
  const [popped, setPopped] = createSignal<string | null>(null);
  /** The one option in the tab order (roving tabindex). */
  const tabStop = () => focused() ?? selected() ?? ALL_AVATARS[0].id;

  const options = () => Array.from(root.querySelectorAll<HTMLButtonElement>(".mavp-opt"));
  const reveal = (id: string, focus: boolean) => {
    const b = root.querySelector<HTMLButtonElement>(`.mavp-opt[data-id="${id}"]`);
    if (!b) return;
    if (focus) b.focus({ preventScroll: true });
    b.scrollIntoView({ block: "nearest", inline: "nearest" });
  };
  const pick = (id: string) => {
    props.onChange("m:" + id);
    setPopped(id);
  };

  /** The option visually above/below: nearest row in that direction, closest column. */
  const vertical = (list: HTMLButtonElement[], i: number, dir: 1 | -1) => {
    const r = list[i].getBoundingClientRect();
    const cx = r.left + r.width / 2;
    let best = -1,
      bestRow = Infinity,
      bestDx = Infinity;
    list.forEach((b, j) => {
      const q = b.getBoundingClientRect();
      const dy = (q.top - r.top) * dir;
      if (dy < r.height / 2) return;
      const dx = Math.abs(q.left + q.width / 2 - cx);
      if (dy < bestRow - 1 || (Math.abs(dy - bestRow) <= 1 && dx < bestDx)) {
        best = j;
        bestRow = dy;
        bestDx = dx;
      }
    });
    return best;
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const t = e.target as HTMLElement;
    if (!t.classList.contains("mavp-opt")) return;
    const list = options();
    const i = list.indexOf(t as HTMLButtonElement);
    let j = -1;
    switch (e.key) {
      case "ArrowRight":
        j = Math.min(list.length - 1, i + 1);
        break;
      case "ArrowLeft":
        j = Math.max(0, i - 1);
        break;
      case "ArrowDown":
        j = vertical(list, i, 1);
        break;
      case "ArrowUp":
        j = vertical(list, i, -1);
        break;
      case "Home":
        j = 0;
        break;
      case "End":
        j = list.length - 1;
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        pick(t.dataset.id!);
        return;
      default:
        return;
    }
    e.preventDefault();
    if (j >= 0 && j !== i) reveal(list[j].dataset.id!, true);
  };

  const shuffle = () => {
    let v = randomAvatar();
    for (let n = 0; n < 8 && v === props.value; n++) v = randomAvatar();
    const id = v.slice(2);
    pick(id);
    setFocused(null);
    requestAnimationFrame(() => reveal(id, false));
  };

  const setText = (raw: string, input?: HTMLInputElement) => {
    // Show exactly what will be kept (the server stores at most 4 characters).
    const kept = Array.from(raw).slice(0, 4).join("");
    if (input && kept !== raw) input.value = kept;
    setDraft(kept);
    const v = clip(kept);
    if (v) props.onChange(v);
  };

  const Option = (p: { a: AvatarInfo; n: number }) => (
    <button
      type="button"
      role="option"
      class="mavp-opt"
      classList={{ popped: popped() === p.a.id }}
      data-id={p.a.id}
      aria-selected={selected() === p.a.id}
      aria-label={`${CATEGORY_LABEL[p.a.category].replace(/s$/, "")} ${p.n}`}
      tabIndex={tabStop() === p.a.id ? 0 : -1}
      onClick={() => pick(p.a.id)}
      onFocus={() => setFocused(p.a.id)}
      onAnimationEnd={() => setPopped(null)}
    >
      <MosaicAvatar avatar={"m:" + p.a.id} size={40} label="" />
    </button>
  );

  return (
    <div class={"mavp" + (props.class ? " " + props.class : "")} ref={root}>
      <div class="mavp-head">
        <span class="mavp-title">Mosaic portrait</span>
        <button type="button" class="mavp-shuffle" onClick={shuffle} title="Pick one at random">
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <rect x="1.5" y="1.5" width="13" height="13" rx="2.5" fill="none" stroke="currentColor" stroke-width="1.4" />
            <circle cx="5" cy="5" r="1.25" fill="currentColor" />
            <circle cx="11" cy="5" r="1.25" fill="currentColor" />
            <circle cx="8" cy="8" r="1.25" fill="currentColor" />
            <circle cx="5" cy="11" r="1.25" fill="currentColor" />
            <circle cx="11" cy="11" r="1.25" fill="currentColor" />
          </svg>
          Shuffle
        </button>
      </div>
      <div class="mavp-scroll" role="listbox" aria-label="Mosaic portrait" onKeyDown={onKeyDown}>
        <For each={SECTIONS}>
          {(cat) => {
            const items = ALL_AVATARS.filter((a) => a.category === cat);
            return (
              <div class="mavp-section" role="group" aria-label={CATEGORY_LABEL[cat]}>
                <div class="mavp-label" aria-hidden="true">
                  {CATEGORY_LABEL[cat]}
                </div>
                <div class="mavp-grid">
                  <For each={items}>{(a, i) => <Option a={a} n={i() + 1} />}</For>
                </div>
              </div>
            );
          }}
        </For>
      </div>
      <div class="mavp-foot">
        <Show
          when={emojiOpen()}
          fallback={
            <button type="button" class="mavp-link" onClick={() => setEmojiOpen(true)}>
              Use an emoji instead
            </button>
          }
        >
          <div class="mavp-emoji">
            <div class="mavp-emoji-row">
              <Show when={usingText()}>
                <MosaicAvatar avatar={props.value} size={36} color={props.color} label="" ring={false} />
              </Show>
              <input
                class="mavp-input"
                value={draft()}
                maxLength={16}
                placeholder="Type or paste an emoji (or up to 4 letters)"
                aria-label="Emoji or short text avatar"
                onInput={(e) => setText(e.currentTarget.value, e.currentTarget)}
              />
            </div>
            <div class="mavp-emoji-grid" role="group" aria-label="Emoji">
              <For each={EMOJI}>
                {(em) => (
                  <button type="button" class="mavp-em" aria-pressed={props.value.trim() === em} onClick={() => setText(em)}>
                    {em}
                  </button>
                )}
              </For>
            </div>
            <button type="button" class="mavp-link" onClick={() => setEmojiOpen(false)}>
              Hide emoji
            </button>
          </div>
        </Show>
      </div>
    </div>
  );
}
