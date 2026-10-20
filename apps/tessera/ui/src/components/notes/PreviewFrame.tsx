// HTML from a note, run like an HTML whiteboard: on another origin, with no
// network, in a sandboxed frame (lib/notes.ts previewSrc). Shown under an
// HTML code block when its Preview is on; paused while the tile is off
// screen.
import { Show, createEffect, createResource, createSignal, on, onCleanup } from "solid-js";
import { previewSrc } from "../../lib/notes";
import { resolvedTheme } from "../../lib/prefs";
import { Icon, Tessellate } from "../icons";

export function PreviewFrame(props: { html: string; visible: () => boolean }) {
  const [tall, setTall] = createSignal(false);
  const [live, setLive] = createSignal(props.visible());
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    clearTimeout(timer);
    if (props.visible()) setLive(true);
    else timer = setTimeout(() => setLive(false), 900);
  });
  onCleanup(() => clearTimeout(timer));
  const [tick, setTick] = createSignal(0);
  const [src] = createResource(
    () => (live() ? { html: props.html, theme: resolvedTheme(), tick: tick() } : null),
    (k) => previewSrc(k.html),
  );
  const [loaded, setLoaded] = createSignal(false);
  createEffect(on(() => src(), () => setLoaded(false)));
  return (
    <div class="nb-frame" classList={{ tall: tall(), ready: loaded() }}>
      <div class="nb-frame-bar">
        <span class="nb-frame-label">
          <Icon name="play" size={11} /> Preview
          <span class="nb-frame-note">· runs sandboxed, no network</span>
        </span>
        <button type="button" class="icon-btn sm" aria-label="Run again" data-tip="Run again" onClick={() => setTick((n) => n + 1)}>
          <Icon name="refresh" size={13} />
        </button>
        <button type="button" class="icon-btn sm" aria-label={tall() ? "Smaller" : "Taller"} data-tip={tall() ? "Smaller" : "Taller"} onClick={() => setTall(!tall())}>
          <Icon name={tall() ? "minimize" : "maximize"} size={13} />
        </button>
      </div>
      <Show
        when={!src.error}
        fallback={
          <p class="nb-frame-msg">
            <Icon name="alertCircle" size={14} /> {String((src.error as Error)?.message ?? src.error)}
          </p>
        }
      >
        <Show when={live() && src()} keyed fallback={<div class="nb-frame-msg">{live() ? <Tessellate size={8} /> : "Paused while hidden"}</div>}>
          {(url) => <iframe src={url} title="Preview" sandbox="allow-scripts" referrerPolicy="no-referrer" allow="" onLoad={() => setLoaded(true)} />}
        </Show>
      </Show>
    </div>
  );
}
