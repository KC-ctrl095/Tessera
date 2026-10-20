// Colours for code blocks: highlight.js (already used for chat replies,
// loaded on first use) turned into ranges the editor marks.
import type { EditorView } from "@codemirror/view";
import { redraw } from "./hooks";

type HL = typeof import("../../../lib/highlight");
let hl: HL | null = null;
let loading = false;
/** Editors to draw again once highlight.js is here. */
const waiting = new Set<EditorView>();

const cache = new Map<string, [number, number, string][]>();

const ENTITIES: Record<string, string> = { "&lt;": "<", "&gt;": ">", "&amp;": "&", "&quot;": '"', "&#x27;": "'", "&#39;": "'" };
const decode = (s: string) => s.replace(/&(?:lt|gt|amp|quot|#x27|#39);/g, (e) => ENTITIES[e] ?? e);

/** [from, to, classes] (offsets in `code`) to colour `code` written in `lang`; none until highlight.js is loaded. */
export function codeMarks(view: EditorView, lang: string, code: string): [number, number, string][] {
  if (!hl) {
    waiting.add(view);
    if (!loading) {
      loading = true;
      void import("../../../lib/highlight").then((m) => {
        hl = m;
        for (const v of waiting) if (v.dom.isConnected) v.dispatch({ effects: redraw.of(null) });
        waiting.clear();
      });
    }
    return [];
  }
  if (!hl.hasLanguage(lang)) return [];
  const key = `${lang}\u0000${code}`;
  let hit = cache.get(key);
  if (!hit) {
    hit = [];
    let html = "";
    try {
      html = hl.highlight(code, lang);
    } catch {
      return [];
    }
    const stack: string[] = [];
    let pos = 0;
    for (const m of html.matchAll(/<span class="([^"]+)">|<\/span>|([^<]+)/g)) {
      if (m[1] !== undefined) stack.push(m[1]);
      else if (m[2] === undefined) stack.pop();
      else {
        const text = decode(m[2]);
        if (stack.length) hit.push([pos, pos + text.length, stack.join(" ")]);
        pos += text.length;
      }
    }
    cache.set(key, hit);
    if (cache.size > 300) cache.delete(cache.keys().next().value!);
  }
  return hit;
}
