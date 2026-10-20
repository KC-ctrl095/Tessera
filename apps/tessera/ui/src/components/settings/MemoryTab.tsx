// Settings → Memory & skills: the two short notes included in every chat, then the saved skills.
import { Show, createEffect, createResource, createSignal, on } from "solid-js";
import type { MemoryTarget } from "../../proto/MemoryTarget";
import { api } from "../../lib/store";
import { toast, toastError } from "../../lib/ui";
import { Spinner } from "../icons";
import { Card, LoadingBlock } from "./kit";
import { SkillsTab } from "./SkillsTab";

export function MemoryTab() {
  const [mem, { mutate }] = createResource(() => api.getMemory());
  return (
    <Show when={mem()} fallback={<LoadingBlock error={mem.error} />}>
      <div class="stack">
        <p class="muted">Two short notes included in every conversation. The agent keeps them up to date too; keep them brief and factual.</p>
        <MemoryEditor title="About your work" help="Your environment, projects and conventions." target="memory" value={mem()!.memory} limit={mem()!.memory_limit} onSaved={mutate} />
        <MemoryEditor title="About you" help="Who you are and how you like to work." target="user" value={mem()!.user} limit={mem()!.user_limit} onSaved={mutate} />
        <section class="settings-section" id="set-skills">
          <h4 class="section-title">Skills</h4>
          <SkillsTab />
        </section>
      </div>
    </Show>
  );
}

function MemoryEditor(props: {
  title: string;
  help: string;
  target: MemoryTarget;
  value: string;
  limit: number;
  onSaved: (m: import("../../proto/MemoryState").MemoryState) => void;
}) {
  const [text, setText] = createSignal(props.value);
  const [busy, setBusy] = createSignal(false);
  createEffect(on(() => props.value, (v) => setText(v), { defer: true }));
  const over = () => text().length > props.limit;
  const dirty = () => text() !== props.value;
  const save = async () => {
    setBusy(true);
    try {
      props.onSaved(await api.setMemory(props.target, text()));
      toast({ level: "success", message: `${props.title} saved`, timeout: 2000 });
    } catch (e) {
      toastError(e, "Save failed");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card>
      <div class="mem-head">
        <div>
          <div class="setting-title">{props.title}</div>
          <div class="setting-help">{props.help}</div>
        </div>
        <span class="counter" classList={{ over: over(), near: !over() && text().length > props.limit * 0.9 }}>
          {text().length.toLocaleString()} / {props.limit.toLocaleString()}
        </span>
      </div>
      <textarea class="input mem-text mono" value={text()} onInput={(e) => setText(e.currentTarget.value)} rows={8} aria-label={props.title} />
      <div class="form-actions">
        <Show when={dirty()}>
          <button type="button" class="btn btn-ghost" onClick={() => setText(props.value)}>
            Revert
          </button>
        </Show>
        <button type="button" class="btn btn-primary" disabled={!dirty() || over() || busy()} onClick={save}>
          <Show when={busy()}>
            <Spinner size={14} />
          </Show>
          Save
        </button>
      </div>
    </Card>
  );
}
