// A text file to read (FileTile, FileWindow): CodeMirror, read only, so a
// long log stays quick (only what's on screen is drawn), with find, line
// numbers for code and logs, long lines wrapped or not, and selecting and
// copying as anywhere. Loaded as its own chunk the first time a file opens
// (it shares CodeMirror with the notes editor).
import { defaultKeymap } from "@codemirror/commands";
import { openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { createEffect, on, onCleanup, onMount } from "solid-js";
import { ensureStyles } from "../../lib/cmStyles";
import "../../styles/find.css";

export interface TextViewHandle {
  find: () => void;
  /** What's selected (else everything). */
  text: () => string;
}

/** Prose: no line numbers. */
const PROSE = /\.(txt|md|markdown|rst|text)$/i;

export default function TextView(props: { text: string; name: string; wrap: boolean; touch: boolean; handle?: (h: TextViewHandle) => void }) {
  let host!: HTMLDivElement;
  let view: EditorView | undefined;
  const wrap = new Compartment();
  onMount(() => {
    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: props.text,
        extensions: [
          EditorState.readOnly.of(true),
          // Phones: no keyboard for a file you read (find has its own box).
          EditorView.editable.of(!props.touch),
          wrap.of(props.wrap ? EditorView.lineWrapping : []),
          PROSE.test(props.name) ? [] : lineNumbers(),
          search({ top: true }),
          keymap.of([...searchKeymap, ...defaultKeymap]),
          EditorView.contentAttributes.of({ "aria-label": `Text of ${props.name}`, "data-swipe": "" }),
        ],
      }),
    });
    ensureStyles(view);
    props.handle?.({
      find: () => view && openSearchPanel(view),
      text: () => {
        if (!view) return props.text;
        const r = view.state.selection.main;
        return r.empty ? view.state.doc.toString() : view.state.sliceDoc(r.from, r.to);
      },
    });
  });
  createEffect(on(() => props.wrap, (w) => view?.dispatch({ effects: wrap.reconfigure(w ? EditorView.lineWrapping : []) }), { defer: true }));
  onCleanup(() => view?.destroy());
  return <div ref={host} class="text-view" classList={{ prose: PROSE.test(props.name) }} />;
}
