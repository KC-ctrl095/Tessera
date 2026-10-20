// Write-only HTTP headers (Cloudflare Access and other proxies): an editor and its check.
import { For, Show } from "solid-js";
import { Icon } from "../icons";

export type HeaderRow = { key: string; value: string };

/**
 * Write-only HTTP headers (proxy / Cloudflare Access): the stored names are
 * listed; replacing them means entering every value again.
 */
export function HeaderEditor(props: {
  label: string;
  names: string[];
  replacing: boolean;
  rows: HeaderRow[];
  setRows: (fn: HeaderRow[] | ((l: HeaderRow[]) => HeaderRow[])) => void;
  onReplace: () => void;
  onKeep: () => void;
}) {
  const set = (i: number, k: keyof HeaderRow, v: string) => props.setRows((l) => l.map((r, j) => (j === i ? { ...r, [k]: v } : r)));
  return (
    <div class="field">
      <span class="field-label">{props.label}</span>
      <Show
        when={props.replacing}
        fallback={
          <div class="row gap wrap">
            <Show when={props.names.length} fallback={<span class="muted small">None</span>}>
              <For each={props.names}>
                {(n) => (
                  <span class="badge mono">
                    <Icon name="lock" size={11} /> {n}
                  </span>
                )}
              </For>
            </Show>
            <button type="button" class="link-btn" onClick={props.onReplace}>
              {props.names.length ? "Replace headers…" : "Add headers…"}
            </button>
          </div>
        }
      >
        <span class="field-help">Headers are saved as a set: enter every value again (they are write-only).</span>
        <For each={props.rows}>
          {(r, i) => (
            <div class="header-row">
              <input class="input mono" placeholder="Header" value={r.key} aria-label="Header name" autocomplete="off" spellcheck={false} onInput={(e) => set(i(), "key", e.currentTarget.value)} />
              <input class="input mono" type="password" placeholder="Value" value={r.value} aria-label={`Value for ${r.key || "header"}`} autocomplete="off" spellcheck={false} onInput={(e) => set(i(), "value", e.currentTarget.value)} />
              <button type="button" class="icon-btn" aria-label="Remove header" onClick={() => props.setRows((l) => l.filter((_, j) => j !== i()))}>
                <Icon name="x" size={15} />
              </button>
            </div>
          )}
        </For>
        <div class="row gap wrap">
          <button type="button" class="link-btn add-row" onClick={() => props.setRows((l) => [...l, { key: "", value: "" }])}>
            <Icon name="plus" size={14} /> Add header
          </button>
          <button
            type="button"
            class="link-btn add-row"
            onClick={() =>
              props.setRows((l) => [
                ...l.filter((x) => x.key.trim() && !/^CF-Access-Client-(Id|Secret)$/i.test(x.key.trim())),
                { key: "CF-Access-Client-Id", value: "" },
                { key: "CF-Access-Client-Secret", value: "" },
              ])
            }
          >
            <Icon name="key" size={14} /> Cloudflare Access
          </button>
          <button type="button" class="link-btn add-row" onClick={() => props.setRows([])}>
            Remove all
          </button>
          <button type="button" class="link-btn add-row" onClick={props.onKeep}>
            Keep current headers
          </button>
        </div>
      </Show>
    </div>
  );
}

/** Validate header rows into a map, or return an error message. */
export function headerMap(rows: HeaderRow[]): Record<string, string> | string {
  const headers: Record<string, string> = {};
  for (const r of rows) {
    const k = r.key.trim();
    if (!k && !r.value) continue;
    if (!/^[A-Za-z0-9-]+$/.test(k)) return `“${k || "(empty)"}” isn't a valid header name.`;
    if (!r.value) return `Enter the value for ${k}. Values are never shown again, so each header needs it re-entered.`;
    headers[k] = r.value;
  }
  return headers;
}
