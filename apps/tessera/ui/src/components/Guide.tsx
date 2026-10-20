// The built-in user guides (docs/*.md, served by `get_guide`), read in
// place: they work offline and need no public website.
import { createResource, createSignal, Show, type JSX } from "solid-js";
import { api } from "../lib/store";
import { errorMessage } from "../lib/ui";
import { Markdown } from "./chat/Markdown";
import { Icon } from "./icons";
import { Modal } from "./primitives";

export type GuideName = "security" | "remote-access" | "claude-code" | "notifications" | "machines" | "windows" | "windows-start" | "terms" | "privacy" | "licences";

/** A link that opens a guide in a reader. */
export function GuideLink(props: { name: GuideName; children: JSX.Element; class?: string }) {
  const [open, setOpen] = createSignal(false);
  return (
    <>
      <button type="button" class={props.class ?? "link-btn"} onClick={() => setOpen(true)}>
        {props.children} <Icon name="book" size={12} />
      </button>
      <Show when={open()}>
        <GuideReader name={props.name} onClose={() => setOpen(false)} />
      </Show>
    </>
  );
}

function GuideReader(props: { name: GuideName; onClose: () => void }) {
  const [guide] = createResource(() => props.name, (name) => api.getGuide(name));
  // Read only once it's there: reading it while it loads would hand the wait to the nearest
  // Suspense (Settings' sections, which would blank out under the guide) and hold this dialog's
  // setup (its back-gesture entry) until then.
  const loaded = () => (guide.state === "ready" ? guide() : undefined);
  return (
    <Modal onClose={props.onClose} title={loaded()?.title ?? "Guide"} size="lg" class="guide-modal">
      <Show
        when={loaded()}
        fallback={
          <p class="muted">{guide.error ? `The guide couldn't be loaded: ${errorMessage(guide.error)}` : "Loading…"}</p>
        }
      >
        {(g) => (
          <Show when={props.name === "licences"} fallback={<Markdown text={g().markdown.replace(/^#[^\n]*\n+/, "")} class="guide-body" />}>
            <div class="guide-body">
              <Markdown text={g().markdown.replace(/^#[^\n]*\n+/, "")} />
              <LicenceTexts />
            </div>
          </Show>
        )}
      </Show>
    </Modal>
  );
}

/**
 * The full licence texts this app carries (lib/licences.ts, in every build: the desktop and
 * Android apps too), shown on demand under the licences page: they're long.
 */
function LicenceTexts() {
  const [open, setOpen] = createSignal(false);
  const [texts] = createResource(open, () => import("../lib/licences").then((m) => m.LICENCE_TEXTS));
  return (
    <div class="licence-texts">
      <button type="button" class="disclosure" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <Icon name="chevronRight" size={14} class="disc-chev" />
        The full licence texts in this app
      </button>
      <Show when={open() && texts.state === "ready" ? texts() : undefined}>
        {(list) => (
          <div class="licence-list">
            {list().map((t) => (
              <section>
                <h3>{t.title}</h3>
                <pre>{t.text}</pre>
              </section>
            ))}
          </div>
        )}
      </Show>
    </div>
  );
}
