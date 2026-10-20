import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onMount, type JSX } from "solid-js";
import type { RewindResult } from "../../proto/RewindResult";
import { abortSession } from "../../lib/actions";
import { typedText } from "../../lib/images";
import { rewindBlocked } from "../../lib/rewind";
import { branchTitle, confirmRewind, fileChanges, shortTitle } from "../../lib/rewindAction";
import { api, state } from "../../lib/store";
import { errorMessage, setRewindDialog, type RewindSpec } from "../../lib/ui";
import { plural } from "../../lib/util";
import { Icon, Spinner, type IconName } from "../icons";
import { Modal, Toggle } from "../primitives";
import { FileIcon } from "./Messages";
import { historyTo } from "../../lib/history";
/** Lists longer than this start folded. */
const FOLD = 4;

/**
 * "Rewind to here": a dry run first, shown calmly — what leaves the
 * conversation, which files come back, what can't be undone — then the two
 * choices. A bottom sheet on phones, a centred sheet elsewhere.
 */
export function RewindDialog(props: { spec: RewindSpec }) {
  const session = () => state.sessions[props.spec.sessionId];
  const info = () => session()?.info;
  const toSeq = Math.max(0, props.spec.seq - 1);
  const message = createMemo(() => {
    const e = session()?.entries.find((x) => x.seq === props.spec.seq);
    return e?.kind === "user" ? e : undefined;
  });
  const text = () => {
    const m = message();
    return m ? typedText(m.content).trim() : "";
  };
  const images = () => message()?.content.filter((p) => p.type === "image").length ?? 0;

  const [preview, setPreview] = createSignal<RewindResult | null>(null);
  const [error, setError] = createSignal<{ message: string; busy: boolean } | null>(null);
  const [files, setFiles] = createSignal(false);
  const [conversation, setConversation] = createSignal(true);
  const [working, setWorking] = createSignal(false);
  // Memos, so the checks below only react to real changes (not every status tick of other chats).
  const blocked = createMemo(() => rewindBlocked(info()));
  const lastSeq = createMemo(() => info()?.last_seq);
  const changes = () => (preview() ? fileChanges(preview()!) : 0);
  const canGo = () => !!preview() && !working() && !blocked() && (conversation() || (files() && changes() > 0));
  let go: HTMLButtonElement | undefined;
  let gen = 0;

  const close = () => {
    if (!working()) setRewindDialog(null);
  };

  const load = async () => {
    const g = ++gen;
    setError(null);
    try {
      await historyTo(props.spec.sessionId, props.spec.seq);
      const r = await api.rewind(props.spec.sessionId, toSeq, { files: true, conversation: true, dryRun: true });
      if (g !== gen) return;
      const first = !preview();
      setPreview(r);
      // Files default on when there's something to put back; a fresh preview keeps the reader's choice.
      if (first || !fileChanges(r)) setFiles(fileChanges(r) > 0);
      if (first) requestAnimationFrame(() => go?.focus({ preventScroll: true }));
    } catch (e) {
      if (g !== gen) return;
      setError(failure(e));
    }
  };
  onMount(() => {
    if (!blocked()) void load();
  });

  // The chat moved on underneath (a turn from another device, the agent stopping): preview again.
  createEffect(
    on(
      [lastSeq, blocked],
      ([, b]) => {
        if (!working() && !b) void load();
      },
      { defer: true },
    ),
  );
  // The chat or the message itself is gone (deleted, or rewound on another device): nothing is left to rewind to.
  createEffect(() => {
    const s = session();
    if (!working() && (!s || (s.loaded && !message() && (!s.hasOlderHistory || (s.entries[0]?.seq ?? Infinity) <= props.spec.seq)))) setRewindDialog(null);
  });

  const submit = async () => {
    if (!canGo()) return;
    setWorking(true);
    setError(null);
    try {
      await confirmRewind(props.spec, { files: files() && changes() > 0, conversation: conversation(), fadeFrom: preview()!.to_seq });
    } catch (e) {
      setWorking(false);
      setError(failure(e));
    }
  };
  /** Shown unless it only repeats the "agent is working" notice. */
  const shownError = () => {
    const e = error();
    return e && !(e.busy && blocked()) ? e.message : null;
  };

  return (
    <Modal onClose={close} title="Rewind to here" size="md" class="rewind-dialog">
      <div class="modal-body rewind-body">
        <p class="rewind-lede">Go back to just before this message:</p>
        <blockquote class="rewind-quote">
          <Show when={images()}>
            <span class="rewind-quote-att">
              <Icon name="image" size={13} />
              {plural(images(), "image", "images")}
            </span>
          </Show>
          <Show when={text()} fallback={<Show when={!images()}><span class="rewind-quote-empty">(no text)</span></Show>}>
            <span class="rewind-quote-text">{text()}</span>
          </Show>
        </blockquote>

        <Show when={blocked()}>
          <div class="rewind-blocked" role="status">
            <Icon name="alert" size={15} />
            <span>The agent is working: stop it first, then rewind.</span>
            <button type="button" class="btn btn-sm" onClick={() => void abortSession(props.spec.sessionId)}>
              <Icon name="stop" size={12} /> Stop
            </button>
          </div>
        </Show>

        <Switch>
          <Match when={shownError()}>
            <div class="form-error" role="alert">
              <Icon name="alertCircle" size={15} />
              <div>
                {shownError()}
                <Show when={!working() && !blocked()}>
                  <button type="button" class="link-btn" onClick={() => void load()}>
                    Try again
                  </button>
                </Show>
              </div>
            </div>
          </Match>
          <Match when={!preview() && !blocked()}>
            <div class="rewind-loading" aria-busy="true">
              <div class="rewind-loading-line">
                <Spinner size={14} />
                <span>Checking what would change…</span>
              </div>
              <div class="sk w80" />
              <div class="sk w60" />
            </div>
          </Match>
        </Switch>

        <Show when={preview()}>
          {(p) => (
            <div class="rewind-preview" classList={{ stale: !!error() || blocked() }}>
              <div class="rewind-opts">
                <Option
                  icon="chat"
                  title="Rewind conversation"
                  checked={conversation()}
                  onChange={setConversation}
                  desc={
                    conversation()
                      ? p().removed_entries <= 1
                        ? "Removes just this message."
                        : `Removes this message and everything after it: ${plural(p().removed_entries, "message", "messages")}.`
                      : "Keeps the conversation as it is."
                  }
                >
                  <Show when={conversation()}>
                    <p class="rewind-kept">
                      <Icon name="fork" size={13} />
                      <span>
                        The full chat stays in your chats as <b>“{shortTitle(branchTitle(info()?.title ?? ""))}”</b>, and your message comes back to the message box.
                      </span>
                    </p>
                  </Show>
                </Option>
                <Option
                  icon="file"
                  title="Restore files"
                  checked={files() && changes() > 0}
                  disabled={!changes()}
                  onChange={setFiles}
                  desc={
                    !changes()
                      ? p().skipped.length
                        ? "Nothing to put back safely: see below."
                        : "The agent didn't change any files after this point."
                      : files()
                        ? filesSummary(p())
                        : "Leaves the files as they are now."
                  }
                >
                  <Show when={changes() || p().skipped.length || p().notes.length}>
                    <div class="rewind-files" classList={{ off: !files() || !changes() }}>
                      <FileGroup label="Restored" hint="back as they were" icon="rewind" paths={p().restored} />
                      <FileGroup label="Removed" hint="created after this point" icon="trash" paths={p().deleted} />
                      <FileGroup label="Left as they are" icon="circleMinus" paths={p().skipped.map((s) => s.path)} reasons={p().skipped.map((s) => s.reason)} />
                      <Show when={p().notes.length}>
                        <ul class="rewind-notes">
                          <For each={p().notes}>
                            {(n) => (
                              <li>
                                <Icon name="info" size={13} />
                                <span>{n}</span>
                              </li>
                            )}
                          </For>
                        </ul>
                      </Show>
                    </div>
                  </Show>
                </Option>
              </div>

              <Show when={p().not_undone.length}>
                <div class="rewind-notice" role="note">
                  <Icon name="terminal" size={15} />
                  <div class="rewind-notice-body">
                    <div class="rewind-notice-title">Shell commands aren't undone</div>
                    <p>
                      {p().not_undone.length === 1 ? "The agent ran a command" : `The agent ran ${p().not_undone.length} commands`} after this point. Whatever{" "}
                      {p().not_undone.length === 1 ? "it" : "they"} changed stays as it is now:
                    </p>
                    <Folded items={p().not_undone} what="commands">
                      {(c) => (
                        <li>
                          <code>{c}</code>
                        </li>
                      )}
                    </Folded>
                  </div>
                </div>
              </Show>

              <Show when={p().to_seq < toSeq}>
                <p class="rewind-moved">
                  <Icon name="info" size={13} />
                  <span>This goes back a little further than your message, so no tool call is cut off from its result.</span>
                </p>
              </Show>
            </div>
          )}
        </Show>
      </div>
      <footer class="rewind-foot">
        <button type="button" class="btn btn-ghost" onClick={close} disabled={working()}>
          Cancel
        </button>
        <button ref={go} type="button" class="btn btn-primary rewind-go" disabled={!canGo()} onClick={() => void submit()}>
          <Show when={working()} fallback={<Icon name="rewind" size={15} />}>
            <Spinner size={14} />
          </Show>
          {working() ? "Rewinding…" : "Rewind"}
        </button>
      </footer>
    </Modal>
  );
}

function filesSummary(r: RewindResult): string {
  const out: string[] = [];
  if (r.restored.length) out.push(`Puts ${plural(r.restored.length, "file", "files")} back as ${r.restored.length === 1 ? "it was" : "they were"}`);
  if (r.deleted.length) out.push(`${out.length ? "removes" : "Removes"} ${plural(r.deleted.length, "new file", "new files")} the agent created`);
  return out.join(" and ") + ".";
}

/** One of the two choices: an icon, what it does, a switch; details below. */
function Option(props: {
  icon: IconName;
  title: string;
  desc: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (v: boolean) => void;
  children?: JSX.Element;
}) {
  return (
    <div class="rewind-opt" classList={{ on: props.checked, disabled: !!props.disabled }}>
      <div
        class="rewind-opt-head"
        onClick={(e) => {
          if (!props.disabled && !(e.target as HTMLElement).closest(".toggle")) props.onChange(!props.checked);
        }}
      >
        <span class="rewind-opt-icon">
          <Icon name={props.icon} size={15} />
        </span>
        <span class="rewind-opt-text">
          <span class="rewind-opt-title">{props.title}</span>
          <span class="rewind-opt-desc">{props.desc}</span>
        </span>
        <Toggle checked={props.checked} disabled={props.disabled} onChange={props.onChange} label={props.title} />
      </div>
      {props.children}
    </div>
  );
}

/** A group of files: a small heading with its count, then the paths (folded when long). */
function FileGroup(props: { label: string; hint?: string; icon: IconName; paths: string[]; reasons?: string[] }) {
  return (
    <Show when={props.paths.length}>
      <div class="rewind-group">
        <div class="rewind-group-head">
          <Icon name={props.icon} size={12} />
          <span class="rewind-group-label">{props.label}</span>
          <span class="rewind-group-count">{props.paths.length}</span>
          <Show when={props.hint}>
            <span class="rewind-group-hint">{props.hint}</span>
          </Show>
        </div>
        <Folded items={props.paths.map((p, i) => ({ p, why: props.reasons?.[i] }))} what="files">
          {(f) => (
            <li class="rewind-file" title={f.p}>
              <FileIcon name={f.p} size={13} />
              <span class="rewind-path">
                <span class="rewind-dir">{dirOf(f.p)}</span>
                <span class="rewind-name">{baseOf(f.p)}</span>
              </span>
              <Show when={f.why}>
                <span class="rewind-why">{f.why}</span>
              </Show>
            </li>
          )}
        </Folded>
      </div>
    </Show>
  );
}

/** A list that shows its first few items and folds the rest behind "Show all". */
function Folded<T>(props: { items: T[]; what: string; children: (item: T) => JSX.Element }) {
  const [all, setAll] = createSignal(false);
  const shown = () => (all() || props.items.length <= FOLD + 1 ? props.items : props.items.slice(0, FOLD));
  return (
    <>
      <ul class="rewind-list">
        <For each={shown()}>{(it) => props.children(it)}</For>
      </ul>
      <Show when={props.items.length > FOLD + 1}>
        <button type="button" class="link-btn rewind-more" aria-expanded={all()} onClick={() => setAll(!all())}>
          <Icon name={all() ? "chevronUp" : "chevronDown"} size={12} />
          {all() ? "Show fewer" : `Show all ${props.items.length} ${props.what}`}
        </button>
      </Show>
    </>
  );
}

const failure = (e: unknown) => ({ message: errorMessage(e), busy: (e as { code?: string }).code === "busy" });

const dirOf = (p: string) => {
  const i = p.lastIndexOf("/");
  return i >= 0 ? p.slice(0, i + 1) : "";
};
const baseOf = (p: string) => p.slice(p.lastIndexOf("/") + 1);
