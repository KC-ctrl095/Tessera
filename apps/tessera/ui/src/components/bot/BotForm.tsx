// Create / edit a bot: a template to start from, identity, personality,
// knowledge (notes it knows), conversation starters, brain, permissions &
// tools, machines, home & memory, who it may message, and its duties
// (scheduled jobs).
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import "../../styles/settings.css";
import "../../styles/bot-editor.css";
import "../../styles/botkit.css";
import { createStore, unwrap } from "solid-js/store";
import type { Bot } from "../../proto/Bot";
import type { Job } from "../../proto/Job";
import type { PermissionMode } from "../../proto/PermissionMode";
import type { ToolPreset } from "../../proto/ToolPreset";
import { permissionOptions, confirmYolo, deleteBot, permissionInfo, saveBot } from "../../lib/actions";
import { api, botById, defaultModel, findModel, modelLabel, state } from "../../lib/store";
import { clampThinking, genericThinking, shortLabel, thinkingOptions } from "../../lib/thinking";
import { browseDir, errorMessage, setBotEditor, toast } from "../../lib/ui";
import { RpcError } from "../../lib/transport";
import { plural, tildify } from "../../lib/util";
import { openBot } from "../../lib/workspace";
import { BOT_TEMPLATES, BotAvatar, type BotTemplate, botKnowledge, botStarters, type BotX, botSuggestions, botColor } from "../bots";
import { BOT_COLORS, PERSONALITY_PRESETS, PRESET_TOOLS, TOOL_NAMES, TOOL_PRESETS } from "./presets";
import { ALL_AVATARS, AvatarPicker, preloadAvatars, randomAvatar, resolveAvatar } from "../../avatars";
import { Icon, Spinner } from "../icons";
import { JobEditor, JobList, emptyJob } from "../Jobs";
import { Modal, Segmented, Toggle } from "../primitives";
import { KnowledgeEditor } from "./Knowledge";
import { BotCredentials, applyKept } from "./BotCredentials";
import { BotMachines } from "./BotMachines";

const RESERVED = new Set(["user", "you", "all", "everyone", "here", "tessera", "main"]);

function normalizeHandle(s: string): string {
  return s
    .trim()
    .replace(/^@+/, "")
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/\s+/g, "-")
    .replace(/[^a-z0-9_-]/g, "")
    .slice(0, 24);
}

/** Starters: at most this many. */
const MAX_STARTERS = 4;

/**
 * A duty drafted in the form, created once the bot is saved. `job.id` is only a local key until then.
 * `inherit`: never opened in the editor, so it follows the bot's home folder and permissions.
 */
interface Draft {
  job: Job;
  tpl?: boolean;
  inherit?: boolean;
  error?: string;
}

const FINISH_DUTY = "Finish the duty you're editing first";

let draftSeq = 0;
const draftKey = () => `draft-${++draftSeq}`;

/** The draft as it will be created for `bot`. */
function dutyFor(d: Draft, bot: Bot): Job {
  if (!d.inherit) return d.job;
  const { cwd, permission } = emptyJob(bot);
  return { ...d.job, cwd, permission };
}

const templateDraft = (d: NonNullable<BotTemplate["duty"]>): Draft => ({
  job: { ...emptyJob(), id: draftKey(), name: d.name, schedule: d.schedule, prompt: d.prompt },
  tpl: true,
  inherit: true,
});

function blankBot(): BotX {
  return {
    id: "",
    name: "",
    handle: "",
    avatar: randomAvatar(),
    color: BOT_COLORS[Math.floor(Math.random() * BOT_COLORS.length)],
    role: "",
    personality: "",
    instructions: "",
    permission: "default",
    tools: "assistant",
    allowed_tools: [],
    own_memory: true,
    share_user_profile: true,
    can_message: [],
    knowledge: [],
    starters: [],
    // Least privilege: no machine until the user picks one (the server does the same).
    machines: { default: "no_access", each: {} },
    // Likewise none of the passwords open to any chat until switched on.
    secrets: { default: false, each: {} },
    created_at: 0,
    updated_at: 0,
  };
}

/** A handle like `want` that no other bot has (`sage`, `sage-2`, …). */
function freeHandle(want: string, selfId: string): string {
  const taken = (h: string) => state.bots.some((x) => x.id !== selfId && x.handle.toLowerCase() === h);
  if (!taken(want)) return want;
  for (let i = 2; i < 100; i++) if (!taken(`${want}-${i}`)) return `${want}-${i}`;
  return want;
}

export function BotForm(props: { id: string | null; template?: string | null }) {
  const existing = botById(props.id);
  const initial: BotX = existing ? structuredClone(unwrap(existing)) : blankBot();
  if (!Array.isArray(initial.knowledge)) initial.knowledge = [];
  initial.starters = botStarters(initial);
  // Old single letters show as a stable portrait; saving makes that one permanent.
  initial.avatar = resolveAvatar(initial.avatar, initial.id || initial.name);
  const [b, setB] = createStore<BotX>(initial);
  /** The template a new bot started from. */
  const [picked, setPicked] = createSignal<string | null>(null);
  /** Duties added before the bot exists (a template's duty starts here too); the ones that couldn't be created stay. */
  const [drafts, setDrafts] = createSignal<Draft[]>([]);
  const template = () => BOT_TEMPLATES.find((t) => t.id === picked());
  const applyTemplate = (id: string | null) => {
    setPicked(id);
    const t = BOT_TEMPLATES.find((x) => x.id === id);
    const base = blankBot();
    const next: BotX = { ...base, ...(t?.bot ?? {}), avatar: t?.bot.avatar ?? base.avatar, starters: [...(t?.bot.starters ?? [])], knowledge: [...b.knowledge] };
    setB(next);
    setDrafts((l) => [...(t?.duty ? [templateDraft(t.duty)] : []), ...l.filter((d) => !d.tpl)]);
    if (t?.bot.name) {
      setB("handle", freeHandle(normalizeHandle(t.bot.name), ""));
      setHandleTouched(true);
    } else setHandleTouched(false);
  };
  const [handleTouched, setHandleTouched] = createSignal(!!existing);
  onMount(() => preloadAvatars(ALL_AVATARS.map((a) => a.id)));
  if (!existing && props.template) applyTemplate(props.template);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [duty, setDuty] = createSignal<Job | null>(null);
  const [dutyDirty, setDutyDirty] = createSignal(false);
  /** Passwords kept for certain bots switched here, waiting for Save (name → this bot can use it). */
  const [keptChanges, setKeptChanges] = createSignal<Record<string, boolean>>({});
  const [credsReload, setCredsReload] = createSignal(0);
  let dutiesEl: HTMLElement | undefined;
  /** The settings after the name and what it does: shown when editing a bot, behind "More settings" for a new one. */
  const [more, setMore] = createSignal(!!botById(props.id));
  const [tried, setTried] = createSignal(false);
  const botId = () => b.id || props.id || "";
  const close = () => setBotEditor(null);

  const handle = () => (handleTouched() ? normalizeHandle(b.handle) : normalizeHandle(b.name));
  const handleError = () => {
    const h = handle();
    if (!h) return b.name.trim() ? "Letters, digits, - or _" : null;
    if (RESERVED.has(h)) return `@${h} is reserved`;
    if (state.bots.some((x) => x.id !== botId() && x.handle.toLowerCase() === h)) return `@${h} is already taken`;
    return null;
  };
  const nameError = () => (tried() && !b.name.trim() ? "Give your bot a name" : null);
  const others = createMemo(() => state.bots.filter((x) => x.id !== botId()));
  const duties = createMemo(() => state.jobs.filter((j) => botId() && j.bot_id === botId()));
  const anyMessage = () => b.can_message.includes("*");
  const models = createMemo(() => state.providers.filter((p) => p.authenticated).flatMap((p) => p.models.map((m) => ({ p, m }))));
  const preview = (): BotX => ({ ...b, handle: handle(), name: b.name.trim() || "Your bot" });
  const draftJobs = createMemo(() => drafts().map((d) => dutyFor(d, preview())));
  const draftErrors = createMemo(() => Object.fromEntries(drafts().flatMap((d) => (d.error ? [[d.job.id, d.error]] : []))));
  /** The duty being edited is a draft (not yet on the server). */
  const drafting = () => !botId() || drafts().some((d) => d.job.id === duty()?.id);
  const dutyCount = () => duties().length + (botId() ? 0 : drafts().length);
  const newDuty = () => setDuty(emptyJob(botById(botId()) ?? preview()));
  const closeDuty = () => {
    setDuty(null);
    if (error()?.startsWith(FINISH_DUTY)) setError(null);
  };
  const removeDraft = (j: Job) => setDrafts((l) => l.filter((d) => d.job.id !== j.id));
  const toggleDraft = (j: Job, enabled: boolean) => setDrafts((l) => l.map((d) => (d.job.id === j.id ? { ...d, job: { ...d.job, enabled } } : d)));
  /** From the duty editor: kept as a draft until the bot exists; once it does, a duty that failed is saved now. */
  const submitDuty = async (job: Job) => {
    if (botId()) {
      await api.saveJob({ ...job, id: "", bot_id: botId() });
      removeDraft(job);
      toast({ level: "success", message: `${b.name} has a new duty`, timeout: 2000 });
    } else if (drafts().some((d) => d.job.id === job.id)) {
      setDrafts((l) => l.map((d) => (d.job.id === job.id ? { ...d, job, inherit: false, error: undefined } : d)));
    } else setDrafts((l) => [...l, { job: { ...job, id: draftKey() } }]);
  };
  // "Create a folder for this bot": ~/<its name> on the server, made right here (no terminal).
  const homeParent = () => state.server?.home ?? "~";
  const homeFolderName = () => b.name.trim().toLowerCase().replace(/[^\p{L}\p{N}]+/gu, "-").replace(/^-+|-+$/g, "").slice(0, 60) || "bot";
  const homeMaking = () => tildify(`${homeParent()}/${homeFolderName()}`, state.server?.home);
  const [homeBusy, setHomeBusy] = createSignal(false);
  const [homeError, setHomeError] = createSignal<string | null>(null);
  /** A folder of that name that was already there (offered, never taken without a click). */
  const [homeThere, setHomeThere] = createSignal<string | null>(null);
  const makeHome = async () => {
    if (homeBusy()) return;
    const name = homeFolderName();
    setHomeBusy(true);
    setHomeError(null);
    setHomeThere(null);
    try {
      const made = await api.createDir(homeParent(), name);
      setB("cwd", made.path);
      toast({ level: "success", message: `Made ${tildify(made.path, state.server?.home)}`, timeout: 2400 });
    } catch (e) {
      setHomeError(errorMessage(e));
      if (e instanceof RpcError && e.code === "conflict") {
        const same = (await api.listDir(homeParent()).catch(() => null))?.find((x) => x.name === name && x.is_dir);
        if (same) setHomeThere(same.path);
      }
    } finally {
      setHomeBusy(false);
    }
  };
  const setStarter = (i: number, v: string) => setB("starters", (l) => (l ?? []).map((x, j) => (j === i ? v : x)));
  /** The model this bot will actually use, and the thinking modes it offers. */
  const effModel = () => findModel(b.model ?? defaultModel());
  const thinkOpts = createMemo(() => thinkingOptions(effModel()));
  const [thinkHover, setThinkHover] = createSignal<string | null>(null);
  const thinkHint = () => {
    const l = thinkHover() ?? b.thinking ?? "";
    if (!l) return `Uses ${effModel()?.name ?? "the model"}'s default${effModel()?.default_thinking ? ` (${shortLabel(thinkOpts().find((o) => o.level === effModel()!.default_thinking)?.label ?? genericThinking(effModel()!.default_thinking).label)})` : ""}.`;
    return thinkOpts().find((o) => o.level === l)?.hint ?? "";
  };
  const setModel = (m: Bot["model"]) => {
    setB("model", m);
    // Keep a chosen thinking level valid for the new model.
    if (b.thinking) {
      const info = findModel(m ?? defaultModel());
      const opts = thinkingOptions(info);
      setB("thinking", opts.length ? clampThinking(b.thinking, opts, info?.default_thinking) : undefined);
    }
  };

  const setTools = (t: ToolPreset) => {
    if (t === "custom" && b.tools !== "custom" && !b.allowed_tools.length) setB("allowed_tools", [...PRESET_TOOLS[b.tools as Exclude<ToolPreset, "custom">]]);
    setB("tools", t);
  };
  const toggleTool = (name: string) =>
    setB("allowed_tools", (l) => (l.includes(name) ? l.filter((x) => x !== name) : [...l, name]));
  const toggleMsg = (h: string) => setB("can_message", (l) => (l.includes(h) ? l.filter((x) => x !== h) : [...l.filter((x) => x !== "*"), h]));

  const save = async (then: "stay" | "chat" | "close") => {
    setTried(true);
    setError(null);
    if (dutyDirty()) return setError(`${FINISH_DUTY}: save it or cancel.`);
    if (!b.name.trim()) return setError("Give your bot a name.");
    if (handleError()) return setError(handleError());
    setBusy(true);
    try {
      const bot: BotX = {
        ...structuredClone(unwrap(b)),
        name: b.name.trim(),
        handle: handle(),
        avatar: b.avatar,
        role: b.role.trim(),
        personality: b.personality.trim(),
        instructions: b.instructions.trim(),
        allowed_tools: b.tools === "custom" ? [...b.allowed_tools] : [],
        knowledge: [...botKnowledge(b)],
        starters: (b.starters ?? []).map((x) => x.trim()).filter(Boolean).slice(0, MAX_STARTERS),
      };
      if (!bot.cwd?.trim()) delete bot.cwd;
      if (!bot.model) delete bot.model;
      if (!bot.thinking) delete bot.thinking;
      const saved = await saveBot(bot);
      const fresh = !existing && !b.id;
      setB({ ...structuredClone(saved), knowledge: botKnowledge(saved), starters: (saved as BotX).starters ?? bot.starters });
      setHandleTouched(true);
      // Passwords kept for certain bots: what was switched here goes to their lists, now that the bot exists.
      const pending = keptChanges();
      const keptFailed = await applyKept(saved.id, pending).catch(() => Object.keys(pending));
      setKeptChanges({});
      setCredsReload((n) => n + 1);
      // The drafted duties are created with it (once). One that fails stays in the list with its reason.
      let made = 0;
      if (fresh) {
        setDuty(null);
        const left: Draft[] = [];
        for (const d of drafts()) {
          try {
            await api.saveJob({ ...dutyFor(d, saved), id: "", bot_id: saved.id });
            made++;
          } catch (e) {
            left.push({ ...d, error: errorMessage(e) });
          }
        }
        setDrafts(left);
        if (left.length) {
          setError(`${saved.name} is saved, but ${left.length === 1 ? "a duty" : `${left.length} duties`} couldn't be added. They're listed under Duties: fix and save each again.`);
          setMore(true);
          requestAnimationFrame(() => dutiesEl?.scrollIntoView({ block: "nearest", behavior: "smooth" }));
          return;
        }
      }
      if (keptFailed.length) {
        setError(`${saved.name} is saved, but who can use ${keptFailed.join(", ")} couldn't be changed. Switch ${keptFailed.length === 1 ? "it" : "them"} again and save.`);
        return;
      }
      const created = `${saved.name} created${made ? ` with ${plural(made, "duty", "duties")}` : ""}`;
      if (then === "chat") {
        close();
        // A test drive starts fresh, so it shows the bot as it is now.
        openBot(saved.id, { newPane: true, fresh: true });
        if (made) toast({ level: "success", message: created, timeout: 2400 });
      } else if (then === "close") {
        close();
        toast({ level: "success", message: `${saved.name} saved`, timeout: 1800 });
      } else toast({ level: "success", message: created, timeout: 2400 });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    const cur = botById(botId());
    if (cur && (await deleteBot(cur))) close();
  };

  return (
    <Modal onClose={close} title={existing ? `Edit ${existing.name}` : "New bot"} size="lg" class="bot-editor">
      <div class="be" style={{ "--bot": botColor(b.color) }}>
        <div class="be-form">
          <Show when={!existing && !b.id}>
            <section class="be-sec be-templates">
              <h3 class="be-h">
                <Icon name="sparkles" size={16} /> Start from
              </h3>
              <div class="tpl-grid" role="radiogroup" aria-label="Start from a template">
                <For each={BOT_TEMPLATES}>
                  {(t) => (
                    <button type="button" role="radio" class="tpl-card" aria-checked={picked() === t.id} classList={{ on: picked() === t.id }} style={{ "--bot": botColor(t.bot.color) }} onClick={() => applyTemplate(t.id)}>
                      <span class="tpl-icon">
                        <Icon name={t.icon} size={16} />
                      </span>
                      <span class="tpl-text">
                        <span class="tpl-label">{t.label}</span>
                        <span class="tpl-blurb">{t.blurb}</span>
                      </span>
                    </button>
                  )}
                </For>
                <button type="button" role="radio" class="tpl-card blank" aria-checked={picked() === null} classList={{ on: picked() === null }} onClick={() => applyTemplate(null)}>
                  <span class="tpl-icon">
                    <Icon name="plus" size={16} />
                  </span>
                  <span class="tpl-text">
                    <span class="tpl-label">From scratch</span>
                    <span class="tpl-blurb">An empty bot to shape yourself.</span>
                  </span>
                </button>
              </div>
              <Show when={template()?.duty}>
                {(d) => (
                  <label class="tpl-duty">
                    <input
                      type="checkbox"
                      checked={drafts().some((x) => x.tpl)}
                      onChange={(e) => {
                        const on = e.currentTarget.checked;
                        setDrafts((l) => (on ? [templateDraft(d()), ...l] : l.filter((x) => !x.tpl)));
                      }}
                    />
                    <span>
                      Also add the duty <b>{d().name}</b> ({d().when}): “{d().prompt}”
                    </span>
                  </label>
                )}
              </Show>
            </section>
          </Show>

          {/* Identity */}
          <section class="be-sec be-sec-id">
            <h3 class="be-h">
              <Icon name="smile" size={16} /> Identity
            </h3>
            <div class="be-id">
              <div class="be-id-fields">
                <div class="be-identity">
                  <div class="field grow">
                    <label class="field-label" for="be-name">
                      Name
                    </label>
                    <input
                      id="be-name"
                      class="input be-name"
                      classList={{ invalid: !!nameError() }}
                      value={b.name}
                      maxLength={40}
                      placeholder="Nova"
                      autofocus
                      onInput={(e) => setB("name", e.currentTarget.value)}
                    />
                    <Show when={nameError()}>
                      <span class="field-help danger-text">{nameError()}</span>
                    </Show>
                  </div>
                  <div class="field">
                    <label class="field-label" for="be-handle">
                      Handle
                    </label>
                    <div class="be-handle" classList={{ invalid: !!handleError() }}>
                      <span>@</span>
                      <input
                        id="be-handle"
                        value={handle()}
                        placeholder="nova"
                        spellcheck={false}
                        autocapitalize="off"
                        onInput={(e) => {
                          setHandleTouched(true);
                          setB("handle", e.currentTarget.value);
                        }}
                      />
                    </div>
                    <span class="field-help" classList={{ "danger-text": !!handleError() }}>
                      {handleError() ?? "Used for @mentions in channels"}
                    </span>
                  </div>
                </div>
                <label class="field">
                  <span class="field-label">Role</span>
                  <input
                    class="input"
                    value={b.role}
                    maxLength={90}
                    placeholder="Personal assistant — plans your day, email, reminders"
                    onInput={(e) => setB("role", e.currentTarget.value)}
                  />
                </label>
                <div class="field">
                  <span class="field-label">Colour</span>
                  <div class="swatches" role="radiogroup" aria-label="Bot colour">
                    <For each={BOT_COLORS}>
                      {(c) => (
                        <button
                          type="button"
                          role="radio"
                          class="swatch"
                          aria-checked={b.color.toLowerCase() === c}
                          aria-label={c}
                          style={{ "--sw": c }}
                          onClick={() => setB("color", c)}
                        >
                          <Show when={b.color.toLowerCase() === c}>
                            <Icon name="check" size={14} stroke={2.4} />
                          </Show>
                        </button>
                      )}
                    </For>
                    <label class="swatch custom" title="Custom colour" classList={{ on: !BOT_COLORS.includes(b.color.toLowerCase()) }} style={{ "--sw": botColor(b.color) }}>
                      <input type="color" value={b.color} onInput={(e) => setB("color", e.currentTarget.value)} aria-label="Custom colour" />
                      <Icon name="plus" size={14} />
                    </label>
                  </div>
                </div>
              </div>
              <div class="field be-avatar">
                <span class="field-label">Avatar</span>
                <AvatarPicker value={b.avatar} onChange={(v) => setB("avatar", v)} color={b.color} />
              </div>
            </div>
          </section>

          {/* A new bot: a template, a name and what it does first; everything else one step deeper. */}
          <Show
            when={more()}
            fallback={
              <button type="button" class="be-more" aria-expanded="false" onClick={() => setMore(true)}>
                <Icon name="chevronRight" size={14} class="disc-chev" />
                <span class="be-more-text">
                  More settings
                  <span class="be-more-what">personality, knowledge, model, freedom, tools, folder, memory, duties, computers, passwords</span>
                </span>
              </button>
            }
          >
            {/* Personality */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="sparkles" size={16} /> Personality
              </h3>
              <div class="preset-row">
                <For each={PERSONALITY_PRESETS}>
                  {(p) => (
                    <button type="button" class="chip" classList={{ active: b.personality === p.text }} onClick={() => setB("personality", p.text)}>
                      {p.label}
                    </button>
                  )}
                </For>
              </div>
              <textarea
                class="input"
                rows={3}
                value={b.personality}
                placeholder="How does it talk? e.g. Warm, friendly and to the point."
                aria-label="Personality"
                onInput={(e) => setB("personality", e.currentTarget.value)}
              />
              <label class="field">
                <span class="field-label">Standing instructions & duties</span>
                <textarea
                  class="input"
                  rows={4}
                  value={b.instructions}
                  placeholder="What is it responsible for? e.g. Keep my calendar tidy, triage email every morning, never book meetings before 10:00."
                  onInput={(e) => setB("instructions", e.currentTarget.value)}
                />
              </label>
            </section>

            {/* Knowledge */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="note" size={16} /> Knowledge
              </h3>
              <p class="muted small be-note">
                Notes {b.name.trim() || "the bot"} knows: it sees their titles and reads them when they help. Add notes from your library, or a text or Markdown file.
              </p>
              <KnowledgeEditor ids={botKnowledge(b)} onChange={(ids) => setB("knowledge", ids)} botName={b.name.trim() || "Bot"} />
            </section>

            {/* Conversation starters */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="chat" size={16} /> Conversation starters
              </h3>
              <p class="muted small be-note">Shown on a new chat with {b.name.trim() || "the bot"}, one tap to send.</p>
              <div class="starters">
                <For each={b.starters ?? []}>
                  {(st, i) => (
                    <div class="starter-row">
                      <input class="input" value={st} maxLength={120} placeholder="Quiz me on what we covered this week" aria-label={`Starter ${i() + 1}`} onInput={(e) => setStarter(i(), e.currentTarget.value)} />
                      <button type="button" class="icon-btn sm" aria-label={`Remove starter ${i() + 1}`} onClick={() => setB("starters", (l) => (l ?? []).filter((_, j) => j !== i()))}>
                        <Icon name="x" size={14} />
                      </button>
                    </div>
                  )}
                </For>
                <Show when={(b.starters ?? []).length < MAX_STARTERS}>
                  <button type="button" class="btn btn-sm starter-add" onClick={() => setB("starters", (l) => [...(l ?? []), ""])}>
                    <Icon name="plus" size={14} /> Add a starter
                  </button>
                </Show>
              </div>
            </section>

            {/* Brain */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="cpu" size={16} /> Brain
              </h3>
              <div class="stack tight">
                <label class="field">
                  <span class="field-label">Model</span>
                  <select
                    class="input"
                    value={b.model ? `${b.model.provider}/${b.model.model}` : ""}
                    onChange={(e) => {
                      const v = e.currentTarget.value;
                      if (!v) setModel(undefined);
                      else {
                        const i = v.indexOf("/");
                        setModel({ provider: v.slice(0, i), model: v.slice(i + 1) });
                      }
                    }}
                  >
                    <option value="">Inherit default ({modelLabel(state.server?.default_model)})</option>
                    <For each={models()}>{({ p, m }) => <option value={`${m.provider || p.id}/${m.id}`}>{`${p.name} · ${m.name}`}</option>}</For>
                  </select>
                </label>
                <div class="field">
                  <span class="field-label">Thinking</span>
                  <Show
                    when={thinkOpts().length}
                    fallback={<span class="field-help">{effModel() ? `${effModel()!.name} has no thinking modes.` : "Pick a model to see its thinking modes."}</span>}
                  >
                    <div class="think-chips" role="radiogroup" aria-label="Thinking" onPointerLeave={() => setThinkHover(null)}>
                      <button
                        type="button"
                        role="radio"
                        class="think-chip"
                        aria-checked={!b.thinking}
                        classList={{ on: !b.thinking }}
                        onPointerEnter={() => setThinkHover("")}
                        onClick={() => setB("thinking", undefined)}
                      >
                        Model default
                      </button>
                      <For each={thinkOpts()}>
                        {(o) => (
                          <button
                            type="button"
                            role="radio"
                            class="think-chip"
                            aria-checked={b.thinking === o.level}
                            classList={{ on: b.thinking === o.level }}
                            onPointerEnter={() => setThinkHover(o.level)}
                            onClick={() => setB("thinking", o.level)}
                          >
                            {o.label}
                          </button>
                        )}
                      </For>
                    </div>
                    <span class="field-help">{thinkHint()}</span>
                  </Show>
                </div>
              </div>
            </section>

            {/* Permissions & tools */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="shield" size={16} /> Permissions & tools
              </h3>
              <div class="field">
                <span class="field-label">Permissions</span>
                <Segmented<PermissionMode>
                  label="Permissions"
                  value={b.permission}
                  onChange={async (v) => {
                    if (await confirmYolo(v)) setB("permission", v);
                  }}
                  options={permissionOptions()}
                />
                <span class="field-help" classList={{ "yolo-text": b.permission === "auto" }}>
                  {permissionInfo(b.permission).desc}.
                </span>
              </div>
              <div class="field">
                <span class="field-label">Tools</span>
                <div class="tool-presets" role="radiogroup" aria-label="Tool preset">
                  <For each={TOOL_PRESETS}>
                    {(t) => (
                      <button type="button" role="radio" class="tool-preset" aria-checked={b.tools === t.value} classList={{ active: b.tools === t.value }} onClick={() => setTools(t.value)}>
                        <span class="tp-label">{t.label}</span>
                        <span class="tp-desc">{t.desc}</span>
                      </button>
                    )}
                  </For>
                </div>
                <Show when={b.tools === "custom"}>
                  <div class="tool-checks">
                    <For each={TOOL_NAMES}>
                      {(t) => (
                        <label class="tool-check" classList={{ on: b.allowed_tools.includes(t.name) }}>
                          <input type="checkbox" checked={b.allowed_tools.includes(t.name)} onChange={() => toggleTool(t.name)} />
                          <span class="mono">{t.name}</span>
                          <span class="muted small">{t.desc}</span>
                        </label>
                      )}
                    </For>
                  </div>
                </Show>
              </div>
            </section>

            {/* Passwords & keys (SECRETS: bot/BotCredentials.tsx) */}
            <BotCredentials
              botId={botId() || null}
              botName={b.name.trim() || "This bot"}
              value={b.secrets}
              onChange={(v) => setB("secrets", v)}
              kept={keptChanges()}
              onKept={(name, on) => setKeptChanges((k) => ({ ...k, [name]: on }))}
              reload={credsReload()}
            />

            {/* Machines: what it may do on each of the user's machines (bot/BotMachines.tsx) */}
            <BotMachines value={b.machines} onChange={(v) => setB("machines", v)} botId={botId() || null} handle={handle()} name={b.name.trim() || "This bot"} />

            {/* Home & memory */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="folder" size={16} /> Home & memory
              </h3>
              <label class="field">
                <span class="field-label">Home folder</span>
                <div class="row gap">
                  <input class="input mono grow" value={b.cwd ?? ""} placeholder={state.server?.home ?? "~"} onInput={(e) => setB("cwd", e.currentTarget.value || undefined)} />
                  <button
                    type="button"
                    class="btn"
                    onClick={async () => {
                      const p = await browseDir(b.cwd || state.server?.home || "/", `${b.name || "Bot"}'s home folder`);
                      if (p) setB("cwd", p);
                    }}
                  >
                    <Icon name="folder" size={15} /> Browse
                  </button>
                </div>
              </label>
              <Show when={!b.cwd?.trim()}>
                <div class="be-home-new">
                  <Show when={b.name.trim()} fallback={<span class="muted small">Name your bot above and Tessera can make a folder for it.</span>}>
                    <button type="button" class="btn btn-sm be-home-btn" disabled={homeBusy()} onClick={() => void makeHome()}>
                      <Show when={homeBusy()} fallback={<Icon name="plus" size={14} />}>
                        <Spinner size={13} />
                      </Show>
                      <span>Create {homeMaking()}</span>
                    </button>
                    <span class="muted small">A new folder just for this bot.</span>
                  </Show>
                </div>
              </Show>
              <Show when={homeError() && !b.cwd?.trim()}>
                <div class="be-home-note danger-text small" role="alert">
                  {homeError()}
                  <Show when={homeThere()}>
                    {" "}
                    <button
                      type="button"
                      class="link-btn"
                      onClick={() => {
                        setB("cwd", homeThere()!);
                        setHomeError(null);
                        setHomeThere(null);
                      }}
                    >
                      Use {tildify(homeThere()!, state.server?.home)}
                    </button>
                  </Show>
                </div>
              </Show>
              <div class="setting-row">
                <div>
                  <div class="strong">Separate memory</div>
                  <div class="muted small">Keeps its own MEMORY.md — separation of duties.</div>
                </div>
                <Toggle checked={b.own_memory} onChange={(v) => setB("own_memory", v)} label="Separate memory" />
              </div>
              <div class="setting-row">
                <div>
                  <div class="strong">Share my profile</div>
                  <div class="muted small">Let it read what Tessera knows about you (USER.md).</div>
                </div>
                <Toggle checked={b.share_user_profile} onChange={(v) => setB("share_user_profile", v)} label="Share user profile" />
              </div>
            </section>

            {/* Messaging */}
            <section class="be-sec">
              <h3 class="be-h">
                <Icon name="chat" size={16} /> Can message
              </h3>
              <p class="muted small be-note">Private bot-to-bot conversations. You can always open and watch them read-only.</p>
              <div class="preset-row">
                <button type="button" class="chip" classList={{ active: anyMessage() }} onClick={() => setB("can_message", anyMessage() ? [] : ["*"])}>
                  <Icon name="users" size={14} /> Any bot
                </button>
                <For each={others()}>
                  {(o) => (
                    <button
                      type="button"
                      class="chip msg-chip"
                      classList={{ active: anyMessage() || b.can_message.includes(o.handle) }}
                      disabled={anyMessage()}
                      onClick={() => toggleMsg(o.handle)}
                    >
                      <BotAvatar bot={o} size={16} /> {o.name}
                    </button>
                  )}
                </For>
                <Show when={!others().length}>
                  <span class="muted small">No other bots yet.</span>
                </Show>
              </div>
              <div class="setting-row be-askable">
                <div>
                  <div class="strong">Other chats may ask it for help</div>
                  <div class="muted small">
                    {b.askable === false
                      ? "Only you talk to it: no chat or bot can hand it work."
                      : "Your chats and bots can hand it work. It then uses its own access, which may be more than theirs."}
                  </div>
                </div>
                <Toggle checked={b.askable !== false} onChange={(v) => setB("askable", v)} label="Other chats may ask it for help" />
              </div>
            </section>

            {/* Duties */}
            <section class="be-sec be-duties" ref={dutiesEl}>
              <h3 class="be-h">
                <Icon name="clock" size={16} /> Duties
                <span class="spacer" />
                <Show when={!duty()}>
                  <button type="button" class="btn btn-sm" onClick={newDuty}>
                    <Icon name="plus" size={14} /> Add duty
                  </button>
                </Show>
              </h3>
              <Show
                when={!duty()}
                fallback={
                  <JobEditor
                    job={duty()!}
                    onDone={closeDuty}
                    lockBot
                    bot={preview()}
                    error={draftErrors()[duty()!.id]}
                    onDirty={setDutyDirty}
                    onSubmit={drafting() ? submitDuty : undefined}
                    saveLabel={drafting() && !botId() ? (duty()!.id ? "Update duty" : "Add duty") : undefined}
                  />
                }
              >
                <div class="stack">
                  <Show when={drafts().length}>
                    <p class="muted small be-note">{botId() ? "These couldn't be added. Edit one to try again." : `Added when you create ${b.name.trim() || "the bot"}.`}</p>
                    <JobList jobs={draftJobs()} onEdit={setDuty} empty={null} local={{ onToggle: toggleDraft, onRemove: removeDraft }} errors={draftErrors()} />
                  </Show>
                  <JobList
                    jobs={duties()}
                    onEdit={setDuty}
                    empty={
                      <Show when={!drafts().length}>
                        <button type="button" class="duty-empty" onClick={newDuty}>
                          <Icon name="calendar" size={18} />
                          <span>
                            No duties yet. <b>Every weekday at 8:00: brief me on…</b>
                          </span>
                        </button>
                      </Show>
                    }
                  />
                </div>
              </Show>
            </section>
          </Show>

          <Show when={existing}>
            <section class="be-sec be-danger">
              <div class="setting-row">
                <div>
                  <div class="strong">Delete {existing!.name}</div>
                  <div class="muted small">Removes the bot and its duties. Past chats stay in your history.</div>
                </div>
                <button type="button" class="btn btn-danger" onClick={remove}>
                  <Icon name="trash" size={15} /> Delete
                </button>
              </div>
            </section>
          </Show>
        </div>

        <aside class="be-side">
          <div class="be-preview" aria-label="Preview">
            <div class="be-preview-glow" />
            <BotAvatar bot={preview()} size={72} ring />
            <div class="be-preview-name">{preview().name}</div>
            <div class="be-preview-handle">@{handle() || "handle"}</div>
            <p class="be-preview-role">{b.role || "What is this bot for?"}</p>
            <div class="be-preview-badges">
              <span class="badge" classList={{ "badge-yolo": b.permission === "auto" }}>
                <Icon name={permissionInfo(b.permission).icon} size={11} /> {permissionInfo(b.permission).label}
              </span>
              <span class="badge">
                <Icon name="wrench" size={11} /> {TOOL_PRESETS.find((t) => t.value === b.tools)?.label}
              </span>
              <span class="badge">
                <Icon name="sparkles" size={11} /> {b.model ? modelLabel(b.model) : "Default model"}
              </span>
              <Show when={b.own_memory}>
                <span class="badge">
                  <Icon name="bookmark" size={11} /> Own memory
                </span>
              </Show>
              <Show when={dutyCount()}>
                <span class="badge">
                  <Icon name="clock" size={11} /> {plural(dutyCount(), "duty", "duties")}
                </span>
              </Show>
              <Show when={botKnowledge(b).length}>
                <span class="badge">
                  <Icon name="note" size={11} /> {plural(botKnowledge(b).length, "note", "notes")}
                </span>
              </Show>
            </div>
            <div class="be-bubble">
              <BotAvatar bot={preview()} size={20} />
              <p>
                Hi, I'm {preview().name}.{" "}
                {b.personality ? b.personality.split(/(?<=\.)\s/)[0] : "Tell me what you need."}
              </p>
            </div>
            <div class="be-suggest">
              <For each={botSuggestions(preview()).slice(0, 3)}>{(s) => <span class="chip">{s}</span>}</For>
            </div>
            <Show when={b.cwd}>
              <div class="muted small mono be-home">
                <Icon name="folder" size={12} /> {tildify(b.cwd!, state.server?.home)}
              </div>
            </Show>
          </div>
          <Show when={error()}>
            <div class="form-error">{error()}</div>
          </Show>
          <div class="be-actions">
            <div class="be-mini" aria-hidden="true">
              <BotAvatar bot={preview()} size={32} />
              <span class="be-mini-text">
                <span class="be-mini-name">{preview().name}</span>
                <span class="be-mini-handle">@{handle() || "handle"}</span>
              </span>
            </div>
            <Show
              when={existing || b.id}
              fallback={
                <>
                  <button type="button" class="btn btn-primary btn-lg full" disabled={busy()} onClick={() => save("chat")}>
                    <Show when={busy()} fallback={<Icon name="chat" size={16} />}>
                      <Spinner size={14} />
                    </Show>
                    Create & start chatting
                  </button>
                  <button type="button" class="btn full" disabled={busy()} onClick={() => save("stay")}>
                    Create and keep editing
                  </button>
                </>
              }
            >
              <button type="button" class="btn btn-primary btn-lg full" disabled={busy()} onClick={() => save("close")}>
                <Show when={busy()} fallback={<Icon name="check" size={16} />}>
                  <Spinner size={14} />
                </Show>
                Save changes
              </button>
              <button type="button" class="btn full" disabled={busy()} onClick={() => save("chat")} title="Saves, then opens a fresh chat to try it out">
                <Icon name="chat" size={15} /> Save & test in a new chat
              </button>
            </Show>
            <button type="button" class="btn btn-ghost full" onClick={close}>
              Cancel
            </button>
          </div>
        </aside>
      </div>
    </Modal>
  );
}
