import { For, Show, createMemo, createSignal, onMount, type JSX } from "solid-js";
import type { ModelRef } from "../../proto/ModelRef";
import type { ThinkingLevel } from "../../proto/ThinkingLevel";
import type { ThinkingOption } from "../../proto/ThinkingOption";
import { prefs, setPrefs } from "../../lib/prefs";
import { api, state } from "../../lib/store";
import { genericThinking, shortLabel } from "../../lib/thinking";
import { openSettings } from "../../lib/ui";
import { fmtTokens, touchUi } from "../../lib/util";
import { Icon } from "../icons";

/** A provider with more models than this starts folded (unless it holds the chat's model): OpenRouter lists hundreds. */
const LONG_GROUP = 16;

/**
 * What a model server of the user's own that isn't answering is marked with:
 * "Offline — 192.168.1.20:8080 isn't answering".
 */
export function offlineText(baseUrl: string): string {
  let place = baseUrl;
  try {
    const u = new URL(baseUrl);
    place = /^(localhost|127\.0\.0\.1|\[::1\])$/i.test(u.hostname) ? "" : u.host;
  } catch {
    // Not a URL: name it as it is.
  }
  return place ? `Offline — ${place} isn't answering` : "Offline — the server on this computer isn't answering";
}

/** The chat's thinking level, offered at the top of the model menu. */
export interface ThinkingControl {
  level: ThinkingLevel;
  options: ThinkingOption[];
  /** The model's own default (its hint says so). */
  fallback?: ThinkingLevel;
  onSelect: (level: ThinkingLevel) => void;
}

/** One row of chips: how hard the model thinks. The line above names what the level under the pointer means. */
function ThinkingRow(props: { think: ThinkingControl }) {
  const [hover, setHover] = createSignal<ThinkingLevel | null>(null);
  const hint = () => {
    const l = hover() ?? props.think.level;
    const o = props.think.options.find((x) => x.level === l);
    const h = o?.hint ?? genericThinking(l).hint;
    // The chip is short; the line says which level is the model's default.
    return (l !== "auto" && props.think.fallback === l) || /model default/i.test(o?.label ?? "") ? `${h} · model default` : h;
  };
  return (
    <section class="menu-think" aria-label="Thinking">
      <div class="opt-head">
        <Icon name="brain" size={13} />
        <span>Thinking</span>
        <span class="opt-head-hint" title={hint()}>
          {hint()}
        </span>
      </div>
      <div class="think-chips" role="radiogroup" aria-label="Thinking" onPointerLeave={() => setHover(null)}>
        <For each={props.think.options}>
          {(o) => (
            <button
              type="button"
              role="radio"
              class="think-chip"
              aria-checked={props.think.level === o.level}
              classList={{ on: props.think.level === o.level }}
              onPointerEnter={() => setHover(o.level)}
              onFocus={() => setHover(o.level)}
              onBlur={() => setHover(null)}
              onClick={() => props.think.onSelect(o.level)}
            >
              {shortLabel(o.label)}
            </button>
          )}
        </For>
      </div>
    </section>
  );
}

/**
 * Model list grouped by provider, with search; a provider folds away (and
 * stays folded). `thinking` adds the chat's thinking level on top, `before`
 * goes above the list (e.g. "Same as …").
 */
export function ModelMenu(props: {
  current: ModelRef | null;
  onSelect: (m: ModelRef) => void;
  onClose: () => void;
  before?: JSX.Element;
  manage?: boolean;
  thinking?: ThinkingControl;
  /** Start on the thinking level instead of the list. */
  focusThinking?: boolean;
  /**
   * Models of a server that isn't answering can be chosen (a setting, not a
   * turn). Otherwise they show why they can't be picked right now.
   */
  allowOffline?: boolean;
}) {
  const [q, setQ] = createSignal("");
  let root!: HTMLDivElement;
  let input!: HTMLInputElement;
  const searching = () => q().trim().length > 0;
  const groups = createMemo(() => {
    const needle = q().trim().toLowerCase();
    return state.providers
      .map((p) => ({
        p,
        models: p.models.filter(
          (m) => !needle || m.name.toLowerCase().includes(needle) || m.id.toLowerCase().includes(needle) || p.name.toLowerCase().includes(needle),
        ),
        // The chat's own model lives here: a marker when this group is folded.
        holds: p.id === props.current?.provider && p.models.some((m) => m.id === props.current?.model),
      }))
      // A server that is off stays in sight even when it never listed a model.
      .filter((g) => g.models.length || (!needle && (!g.p.authenticated || g.p.offline)));
  });
  /** Folded by choice; without one, only long lists that don't hold the chat's model. A search opens everything it finds. */
  const folded = (g: { p: { id: string; models: unknown[] }; holds: boolean }) => !searching() && (prefs.modelGroups[g.p.id] ?? (g.p.models.length > LONG_GROUP && !g.holds));
  const total = () => state.providers.reduce((n, p) => n + p.models.length, 0);
  // A server that was off may be back: asking for the list makes the server
  // look again, and it tells every device when one answers (the list
  // updates while open). Nothing here changes until then.
  onMount(() => {
    if (state.providers.some((p) => p.offline)) void api.listProviders(false).catch(() => {});
  });
  onMount(() =>
    requestAnimationFrame(() =>
      (props.focusThinking && props.thinking
        ? root.querySelector<HTMLButtonElement>(".think-chip.on") ?? root.querySelector<HTMLButtonElement>(".think-chip")
        : // On a touch screen a focused search would raise the keyboard over the list: it waits for a tap.
          total() > 6 && !touchUi()
          ? input
          : root.querySelector<HTMLButtonElement>(".model-item.selected") ?? root.querySelector<HTMLButtonElement>(".model-item")
      )?.focus(),
    ),
  );

  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const list = Array.from(root.querySelectorAll<HTMLButtonElement>(".model-group-toggle, .model-item:not(:disabled)"));
    const i = list.indexOf(document.activeElement as HTMLButtonElement);
    const next = e.key === "ArrowDown" ? (i + 1) % list.length : i <= 0 ? list.length - 1 : i - 1;
    list[next]?.focus();
  };

  return (
    <div class="model-menu" ref={root} onKeyDown={onKey}>
      <Show when={props.thinking?.options.length}>
        <ThinkingRow think={props.thinking!} />
      </Show>
      <Show when={total() > 6}>
        <div class="menu-search">
          <Icon name="search" size={15} />
          <input
            ref={input}
            placeholder="Search models"
            value={q()}
            onInput={(e) => setQ(e.currentTarget.value)}
            aria-label="Search models"
            onKeyDown={(e) => {
              if (e.key === "Enter") root.querySelector<HTMLButtonElement>(".model-item:not(:disabled)")?.click();
            }}
          />
        </div>
      </Show>
      <div class="model-groups">
        {props.before}
        <Show when={!state.providersLoaded}>
          <div class="menu-empty">Loading models…</div>
        </Show>
        <Show when={state.providersLoaded && !q() && !state.providers.some((p) => p.authenticated && p.models.length)}>
          <div class="menu-empty">Connect an AI account or add an API key to start chatting.</div>
        </Show>
        <Show when={state.providersLoaded && !groups().length}>
          <div class="menu-empty">{q() ? "No matching models" : "No providers configured"}</div>
        </Show>
        <For each={groups()}>
          {(g) => (
            <div class="model-group">
              <div class="menu-heading model-group-head">
                <button
                  type="button"
                  class="model-group-toggle"
                  aria-expanded={!folded(g)}
                  aria-label={`${g.p.name}, ${g.models.length} models`}
                  onClick={() => setPrefs("modelGroups", g.p.id, !folded(g))}
                >
                  <Icon name="chevronDown" size={12} class="grp-chev" />
                  <span class="model-group-name">{g.p.name}</span>
                  <Show when={g.models.length}>
                    <span class="model-group-count">{g.models.length}</span>
                  </Show>
                  <Show when={folded(g) && g.holds}>
                    <i class="model-group-dot" data-tip="Your model is in here" aria-label="Holds the current model" />
                  </Show>
                  <Show when={g.p.offline}>
                    <span class="model-group-off" data-tip={offlineText(g.p.base_url)}>
                      Offline
                    </span>
                  </Show>
                </button>
                <Show when={!g.p.authenticated}>
                  <button
                    type="button"
                    class="link-btn"
                    onClick={() => {
                      props.onClose();
                      openSettings("models");
                    }}
                  >
                    {g.p.auth === "oauth" ? "Sign in" : "Add API key"}
                  </button>
                </Show>
              </div>
              <Show when={!folded(g)}>
                <Show when={g.p.offline && !g.models.length}>
                  <div class="model-offline-empty">{offlineText(g.p.base_url)}</div>
                </Show>
                <For each={g.models}>
                  {(m) => {
                    const selected = () => g.p.authenticated && props.current?.provider === m.provider && props.current?.model === m.id;
                    const offline = () => !!g.p.offline;
                    return (
                      <button
                        type="button"
                        class="menu-item model-item"
                        classList={{ selected: selected(), offline: offline() }}
                        disabled={!g.p.authenticated || (offline() && !props.allowOffline)}
                        role="menuitemradio"
                        aria-checked={selected()}
                        title={offline() ? offlineText(g.p.base_url) : undefined}
                        onClick={() => {
                          props.onSelect({ provider: m.provider || g.p.id, model: m.id });
                          props.onClose();
                        }}
                      >
                        <span class="menu-text">
                          <span class="menu-label">{m.name}</span>
                          <Show when={offline()} fallback={<span class="menu-desc">{m.id}</span>}>
                            <span class="menu-desc model-offline">{offlineText(g.p.base_url)}</span>
                          </Show>
                        </span>
                        <span class="model-caps">
                          <Show when={(m.thinking ?? []).length}>
                            <span class="cap" data-tip={`Thinking: ${(m.thinking ?? []).map((t) => shortLabel(t.label)).join(", ")}`}>
                              <Icon name="brain" size={13} aria-label="Thinking modes" />
                              {(m.thinking ?? []).length}
                            </span>
                          </Show>
                          <Show when={m.vision}>
                            <span class="cap" data-tip="Understands images">
                              <Icon name="image" size={13} aria-label="Vision" />
                            </span>
                          </Show>
                          <Show when={m.fast}>
                            <span class="cap" data-tip="Offers fast mode (/fast)">
                              <Icon name="fast" size={13} aria-label="Fast mode" />
                            </span>
                          </Show>
                          <span class="ctx-badge" data-tip="Context window">
                            {fmtTokens(m.context_window)}
                          </span>
                        </span>
                        <Show when={selected()}>
                          <Icon name="check" size={15} class="menu-check" />
                        </Show>
                      </button>
                    );
                  }}
                </For>
              </Show>
            </div>
          )}
        </For>
      </div>
      <Show when={props.manage !== false}>
      <div class="menu-foot">
        <button
          type="button"
          class="menu-item"
          onClick={() => {
            props.onClose();
            openSettings("models");
          }}
        >
          <span class="menu-icon">
            <Icon name="gear" size={15} />
          </span>
          <span class="menu-label">Manage models…</span>
        </button>
      </div>
      </Show>
    </div>
  );
}

/** Thin ring showing context usage (tokens / window). */
export function ContextRing(props: { tokens: number; window: number; size?: number }) {
  const pct = () => (props.window > 0 ? Math.min(1, props.tokens / props.window) : 0);
  const r = 6.5;
  const c = 2 * Math.PI * r;
  const level = () => (pct() > 0.9 ? "crit" : pct() > 0.7 ? "warn" : "ok");
  return (
    <svg class={`ctx-ring ctx-${level()}`} viewBox="0 0 18 18" width={props.size ?? 18} height={props.size ?? 18} aria-hidden="true">
      <circle cx="9" cy="9" r={r} fill="none" stroke="currentColor" stroke-opacity="0.2" stroke-width="2.4" />
      <circle
        class="ctx-ring-val"
        cx="9"
        cy="9"
        r={r}
        fill="none"
        stroke-width="2.4"
        stroke-linecap="round"
        stroke-dasharray={`${Math.max(0.001, pct() * c)} ${c}`}
        transform="rotate(-90 9 9)"
      />
    </svg>
  );
}
