// Settings → Memory & skills → Skills: saved procedures the agent loads when they fit.
import { For, Show, createMemo, createResource, createSignal } from "solid-js";
import type { SkillInfo } from "../../proto/SkillInfo";
import { api, state } from "../../lib/store";
import { confirmDialog, errorMessage, toast, toastError } from "../../lib/ui";
import { plural, relTime, tildify } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { LoadingBlock } from "./kit";

const SKILL_GROUPS: { source: string; title: string; hint: string }[] = [
  { source: "user", title: "Saved on this server", hint: "Available in every chat." },
  { source: "project", title: "Project folder", hint: "From the server's default working folder." },
  { source: "extra", title: "Other folders", hint: "From folders added with skills.extra_dirs in config.toml." },
];

export function SkillsTab() {
  const [skills, { mutate, refetch }] = createResource(() => api.listSkills(null));
  const [busy, setBusy] = createSignal<string | null>(null);
  const folder = (s: SkillInfo) => tildify(s.path.replace(/\/SKILL\.md$/, ""), state.server?.home);
  const groups = createMemo(() => {
    const all = skills() ?? [];
    const known = SKILL_GROUPS.map((g) => g.source);
    const rest = [...new Set(all.map((s) => s.source))].filter((src) => !known.includes(src)).map((source) => ({ source, title: source, hint: "" }));
    return [...SKILL_GROUPS, ...rest]
      .map((g) => ({ ...g, items: all.filter((s) => s.source === g.source) }))
      .filter((g) => g.items.length);
  });

  const remove = async (s: SkillInfo) => {
    const ok = await confirmDialog({
      title: `Delete “${s.name}”?`,
      body: `Its folder, ${folder(s)}, is deleted from the server. Chats that are already open keep it until they start over.${
        s.source === "extra" ? " That folder belongs to another tool's skills, so it is gone there too." : ""
      } This can't be undone.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    setBusy(s.name);
    try {
      await api.deleteSkill(s.name);
      mutate((list) => list?.filter((x) => x.name !== s.name));
      toast({ level: "success", message: `Deleted ${s.name}`, timeout: 2000 });
    } catch (e) {
      toastError(e, "Could not delete the skill");
      // Show what is really there (it may have gone already).
      void refetch();
    } finally {
      setBusy(null);
    }
  };

  return (
    <Show when={skills()} fallback={<LoadingBlock error={skills.error} />}>
      <div class="stack">
        <p class="muted">
          A skill is a saved procedure the agent loads when it fits. The agent only saves one after working out something multi-step worth repeating, and never asks you. Every skill's name and description is
          sent with each new chat, so delete the ones you don't want.
        </p>
        <Show
          when={skills()!.length}
          fallback={
            <div class="empty-state">
              <span class="empty-icon">
                <Icon name="book" size={20} />
              </span>
              <p>No skills yet.</p>
            </div>
          }
        >
          <For each={groups()}>
            {(g) => (
              <section class="skill-group" aria-label={g.title}>
                <div class="skill-group-head">
                  <h3>{g.title}</h3>
                  <span class="muted small">
                    {plural(g.items.length, "skill", "skills")}
                    {g.hint ? ` · ${g.hint}` : ""}
                  </span>
                </div>
                <ul class="skill-list">
                  <For each={g.items}>
                    {(s) => <SkillCard skill={s} folder={folder(s)} busy={busy() === s.name} onDelete={() => remove(s)} />}
                  </For>
                </ul>
              </section>
            )}
          </For>
        </Show>
      </div>
    </Show>
  );
}

function SkillCard(props: { skill: SkillInfo; folder: string; busy: boolean; onDelete: () => void }) {
  const s = () => props.skill;
  const [open, setOpen] = createSignal(false);
  const [text] = createResource(open, (o) => (o ? api.readSkill(s().name).catch((e) => `Could not read it: ${errorMessage(e)}`) : ""));
  return (
    <li class="card skill" data-skill={s().name}>
      <div class="skill-head">
        <Icon name="book" size={15} />
        <span class="strong grow skill-name">{s().name}</span>
        <button type="button" class="icon-btn danger-text" aria-label={`Delete ${s().name}`} data-tip="Delete" disabled={props.busy} onClick={props.onDelete}>
          <Show when={props.busy} fallback={<Icon name="trash" size={16} />}>
            <Spinner size={14} />
          </Show>
        </button>
      </div>
      <p class="skill-desc">{s().description}</p>
      <div class="skill-foot">
        <span class="muted mono tiny skill-where">
          {props.folder}
          <Show when={s().updated_at}> · changed {relTime(s().updated_at)}</Show>
        </span>
        <button type="button" class="btn btn-sm btn-ghost skill-view" aria-expanded={open()} onClick={() => setOpen(!open())}>
          {open() ? "Hide steps" : "View steps"}
        </button>
      </div>
      <Show when={open()}>
        <Show when={!text.loading} fallback={<LoadingBlock />}>
          <pre class="skill-body mono">{text()}</pre>
        </Show>
      </Show>
    </li>
  );
}
