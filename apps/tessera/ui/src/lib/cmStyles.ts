// CodeMirror's styles where the apps' security policy keeps its <style>
// element out (the notes editor and the file viewer use it).
import type { EditorView } from "@codemirror/view";

/**
 * CodeMirror's own styles go in a <style> element, which the apps' security
 * policy can keep out (their inline styles need a nonce, and with one the
 * policy's 'unsafe-inline' no longer counts). Then the same rules go in
 * through the stylesheet API instead: on each new editor, the ones not added
 * yet (an editor with other parts brings rules of its own).
 */
let works = false;
const added = new WeakSet<object>();
export function ensureStyles(view: EditorView) {
  if (works) return;
  // style-mod keeps them on the document under a symbol of its own.
  const key = Object.getOwnPropertySymbols(document).find((k) => k.description === "styleSet");
  const set = key ? (document as unknown as Record<symbol, { modules?: { getRules(): string }[] } | undefined>)[key] : undefined;
  if (!set?.modules?.some((m) => added.has(m)) && getComputedStyle(view.dom).display === "flex") {
    works = true;
    return;
  }
  const fresh = (set?.modules ?? []).filter((m) => !added.has(m));
  const text = fresh.map((m) => m.getRules()).join("\n");
  if (!text) return;
  try {
    // In a layer of their own: the app's styles (notes.css) still come first, as they do over
    // the <style> element.
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(`@layer codemirror {\n${text}\n}`);
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
    for (const m of fresh) added.add(m);
  } catch (e) {
    console.warn("the editor's styles couldn't be added", e);
  }
}
