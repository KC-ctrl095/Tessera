// Central reactive store. Server events are applied incrementally; RPC
// wrappers live in `api`. Everything the UI renders about the server comes
// from here.
import { batch, createSignal } from "solid-js";
import { createStore, produce, reconcile, unwrap } from "solid-js/store";
import { isCheckAck } from "./completion";
import type { ApprovalRequest } from "../proto/ApprovalRequest";
import type { Attachment } from "../proto/Attachment";
import type { CloudflareZone } from "../proto/CloudflareZone";
import type { MosaicLayout } from "../proto/MosaicLayout";
import type { RemoteStatus } from "../proto/RemoteStatus";
import type { SecretKind } from "../proto/SecretKind";
import type { ChatSecretChoice } from "../proto/ChatSecretChoice";
import type { SetupStatus } from "../proto/SetupStatus";
import type { ClaudeCodeInfo } from "../proto/ClaudeCodeInfo";
import type { AccountSettings } from "../proto/AccountSettings";
import type { ProviderKind } from "../proto/ProviderKind";
import type { Guide } from "../proto/Guide";
import type { ApprovalDecision } from "../proto/ApprovalDecision";
import type { AssistantPart } from "../proto/AssistantPart";
import type { Bot } from "../proto/Bot";
import type { ContentPart } from "../proto/ContentPart";
import type { Delivery } from "../proto/Delivery";
import type { DeviceInfo } from "../proto/DeviceInfo";
import type { DirEntry } from "../proto/DirEntry";
import type { Entry } from "../proto/Entry";
import type { Event } from "../proto/Event";
import type { Job } from "../proto/Job";
import type { MemoryState } from "../proto/MemoryState";
import type { MemoryTarget } from "../proto/MemoryTarget";
import type { ModelInfo } from "../proto/ModelInfo";
import type { ModelRef } from "../proto/ModelRef";
import type { HelperLimits } from "../proto/HelperLimits";
import type { HelperSettings } from "../proto/HelperSettings";
import type { ModelRoles } from "../proto/ModelRoles";
import type { BoardContent } from "../proto/BoardContent";
import type { OAuthStart } from "../proto/OAuthStart";
import type { PairingCode } from "../proto/PairingCode";
import type { PermissionMode } from "../proto/PermissionMode";
import type { ProviderInfo } from "../proto/ProviderInfo";
import type { ProviderSpec } from "../proto/ProviderSpec";
import type { QueuedMessage } from "../proto/QueuedMessage";
import type { RemoteAccessInfo } from "../proto/RemoteAccessInfo";
import type { Request } from "../proto/Request";
import type { RewindResult } from "../proto/RewindResult";
import type { RunningTool } from "../proto/RunningTool";
import type { SearchHit } from "../proto/SearchHit";
import type { SecretInfo } from "../proto/SecretInfo";
import type { ServerInfo } from "../proto/ServerInfo";
import type { SessionListPage } from "../proto/SessionListPage";
import type { SessionListCursor } from "../proto/SessionListCursor";
import type { SessionInfo } from "../proto/SessionInfo";
import type { SessionSnapshot } from "../proto/SessionSnapshot";
import type { SkillInfo } from "../proto/SkillInfo";
import type { SmallMode } from "../proto/SmallMode";
import type { ThinkingLevel } from "../proto/ThinkingLevel";
import type { Transcript } from "../proto/Transcript";
import type { VoiceConfig } from "../proto/VoiceConfig";
import type { SpeechAudio } from "../proto/SpeechAudio";
import type { WebSearchInfo } from "../proto/WebSearchInfo";
import type { AskAnswer } from "../proto/AskAnswer";
import type { Whiteboard } from "../proto/Whiteboard";
import type { Note } from "../proto/Note";
import type { NoteInfo } from "../proto/NoteInfo";
import type { NoteVersion } from "../proto/NoteVersion";
import { connectionGeneration } from "./connectionGeneration";
import { backend } from "./backend";
import { machinesChanged } from "./machines";
import type { ClientConfig, ConnectionStatus } from "./transport";
import { toast } from "./ui";
import { reducedMotion, uid } from "./util";

export type ToolResultEntry = Extract<Entry, { kind: "tool_result" }>;
export type AssistantEntry = Extract<Entry, { kind: "assistant" }>;

export interface StreamState {
  /** Tool-call `arguments` are the raw (possibly partial) JSON string while streaming. */
  content: AssistantPart[];
  model: ModelRef;
  startedAt: number;
  /** `stream_ended` received; kept visible until the final entry arrives to avoid flicker. */
  ended: boolean;
  /** Bot id of the speaker (bot chats, channels). */
  author?: string;
  thinkStart?: number;
  thinkEnd?: number;
}

export interface LiveTool extends RunningTool {
  startedAt: number;
}

export interface PendingSend {
  id: string;
  content: ContentPart[];
  delivery: Delivery;
  at: number;
}

export interface SessionState {
  info: SessionInfo;
  entries: Entry[];
  /** Invalidates history-derived views when entries are rewritten without changing the array's length. */
  entriesVersion: number;
  /** Server revision of destructive history edits, independent of sequence numbers. */
  historyRevision: number;
  /** tool_result entries by call_id. */
  results: Record<string, ToolResultEntry>;
  loaded: boolean;
  loading: boolean;
  /** Older entries are fetched only as the user reaches the top. */
  hasOlderHistory: boolean;
  loadingOlderHistory: boolean;
  streaming: StreamState | null;
  running: Record<string, LiveTool>;
  approvals: ApprovalRequest[];
  /** Messages waiting for the busy agent: steers (next step) first, then follow-ups. */
  queued: QueuedMessage[];
  /** Optimistic user messages not yet echoed by the server. */
  pending: PendingSend[];
  /** Locally measured thinking time per assistant entry seq. */
  thinkMs: Record<number, number>;
  /** Locally measured tool run time per call id. */
  toolMs: Record<string, number>;
  /** When the current turn started (client clock), while busy. */
  turnStart: number | null;
  /** When `info.status` last changed (client clock): how long it has been compacting, say. */
  statusSince: number;
  /** History after entry `from` is being rewound: those rows fade out, then go. `local`: this device asked. */
  rewind: { from: number; local: boolean } | null;
}

export interface ProgressStep {
  step: string;
  done: boolean;
  error?: string;
}

export interface AppState {
  conn: ConnectionStatus;
  everConnected: boolean;
  ready: boolean;
  server: ServerInfo | null;
  config: ClientConfig | null;
  sessions: Record<string, SessionState>;
  sessionsLoaded: boolean;
  archived: SessionInfo[] | null;
  providers: ProviderInfo[];
  providersLoaded: boolean;
  jobs: Job[];
  bots: Bot[];
  botsLoaded: boolean;
  /** Live `setup_progress` steps by task (e.g. "cloudflare"). */
  progress: Record<string, ProgressStep[]>;
  /** Whiteboards by id (every chat's). */
  boards: Record<string, Whiteboard>;
  /** The notes library, by id (newest first when listed). */
  notes: Record<string, NoteInfo>;
  notesLoaded: boolean;
  /** Asks answered from this device (id → what was said), shown collapsed until the page reloads. */
  answered: Record<string, { board: string; session: string; prompt: string; label: string }>;
}

export const [state, setState] = createStore<AppState>({
  conn: { state: "disconnected", serverId: null, endpoint: null, error: null },
  everConnected: false,
  ready: false,
  server: null,
  config: null,
  sessions: {},
  sessionsLoaded: false,
  archived: null,
  providers: [],
  providersLoaded: false,
  jobs: [],
  bots: [],
  botsLoaded: false,
  progress: {},
  boards: {},
  notes: {},
  notesLoaded: false,
  answered: {},
});

// ── hooks the workspace registers (avoids an import cycle) ───────────────

let openSessionIds: () => string[] = () => [];
export function setOpenSessionsProvider(fn: () => string[]) {
  openSessionIds = fn;
}
const removedListeners = new Set<(id: string) => void>();
const mosaicListeners = new Set<(layout: MosaicLayout) => void>();
/** Another device changed the shared mosaic layout. */
export function onMosaicsChanged(cb: (layout: MosaicLayout) => void) {
  mosaicListeners.add(cb);
  return () => mosaicListeners.delete(cb);
}
const noteListeners = new Set<(e: { type: "note_updated"; note: NoteInfo } | { type: "note_removed"; id: string }) => void>();
/** A note changed or went away (note tiles follow). */
export function onNoteEvent(cb: (e: { type: "note_updated"; note: NoteInfo } | { type: "note_removed"; id: string }) => void) {
  noteListeners.add(cb);
  return () => noteListeners.delete(cb);
}
const boardRemovedListeners = new Set<(id: string) => void>();
/** A whiteboard was deleted (its tiles close). */
export function onBoardRemoved(cb: (id: string) => void) {
  boardRemovedListeners.add(cb);
  return () => boardRemovedListeners.delete(cb);
}
const bootListeners = new Set<() => void>();
/** Called after every successful (re)connect, once the lists are loaded. */
export function onBootstrapped(cb: () => void) {
  bootListeners.add(cb);
  return () => bootListeners.delete(cb);
}
/** Forget the live progress of a task before starting it again. */
export const resetProgress = (task: string) => setState("progress", task, []);
export function onSessionRemoved(cb: (id: string) => void) {
  removedListeners.add(cb);
  return () => removedListeners.delete(cb);
}

// ── RPC ──────────────────────────────────────────────────────────────────

export function call<T>(req: Request): Promise<T> {
  return backend.call<T>(req);
}

type SessionPatch = {
  title?: string;
  model?: ModelRef;
  thinking?: ThinkingLevel;
  permission?: PermissionMode;
  pinned?: boolean;
  archived?: boolean;
  cwd?: string;
  members?: string[];
  fast?: boolean;
  subagents?: boolean;
};

// Voice is connection-scoped: cancel work without cutting off the chat socket.
function voiceCall<T>(req: Extract<Request, { method: "transcribe" | "speak" }>, signal?: AbortSignal): Promise<T> {
  if (signal?.aborted) return Promise.reject(new DOMException("Voice canceled", "AbortError"));
  const operation = crypto.randomUUID();
  const work = call<T>({ ...req, params: { ...req.params, operation } } as Request);
  if (!signal) return work;
  return new Promise((resolve, reject) => {
    const abort = () => {
      signal.removeEventListener("abort", abort);
      void call({ method: "cancel_voice", params: { operation } }).catch(() => {});
      reject(new DOMException("Voice canceled", "AbortError"));
    };
    signal.addEventListener("abort", abort, { once: true });
    work.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
    if (signal.aborted) abort();
  });
}
// Tuple union keeps the exact method/params pairing checked by Request, while
// sharing the emitted object-building code across the eager API surface.
type RequestArgs<R = Request> = R extends { method: infer M; params: infer P }
  ? [method: M, params: P] : R extends { method: infer M } ? [method: M] : never;
const rpc = <T>(...args: RequestArgs) => call<T>({ method: args[0], params: args[1] } as Request);
const bare = <T>(method: Extract<RequestArgs, [method: string]>[0]) => () => call<T>({ method } as Request);

export const api = {
  hello: () => rpc<ServerInfo>("hello", { client: "tessera-ui", version: __APP_VERSION__ }),
  listSessionPage: (archived = false, before: SessionListCursor | null = null, include_ids: string[] = []) =>
    rpc<SessionListPage>("list_session_page", { archived, before, include_ids, limit: 100 }),
  listSessions: (archived = false) => rpc<SessionInfo[]>("list_sessions", { archived }),
  createSession: (o: { cwd?: string | null; model?: ModelRef | null; title?: string | null; botId?: string | null; private?: boolean } = {}) =>
    rpc<SessionInfo>("create_session", { cwd: o.cwd ?? null, model: o.model ?? null, title: o.title ?? null, bot_id: o.botId ?? null, private: !!o.private }),
  getSession: (session_id: string, since_seq?: number, page?: { before_seq?: number }) =>
    call<SessionSnapshot>(page ? {
      method: "get_session_page",
      params: { session_id, before_seq: page.before_seq, limit: 120 },
    } : {
      method: "get_session",
      params: since_seq === undefined ? { session_id } : { session_id, since_seq },
    }).catch((error) => {
      // Old servers do not know this opt-in request. Keep their full-history path.
      if (page && (error as { code?: string }).code === "bad_request"
        && /unknown (variant|method)|not implemented/i.test(String((error as { message?: string }).message)))
        return rpc<SessionSnapshot>("get_session", { session_id });
      throw error;
    }),
  updateSession: (session_id: string, p: SessionPatch) =>
    rpc<SessionInfo>("update_session", {
        session_id,
        title: p.title ?? null,
        model: p.model ?? null,
        thinking: p.thinking ?? null,
        permission: p.permission ?? null,
        pinned: p.pinned ?? null,
        archived: p.archived ?? null,
        cwd: p.cwd ?? null,
        members: p.members ?? null,
        fast: p.fast ?? null,
        subagents: p.subagents ?? null,
    }),
  setSessionDaybreak: (session_id: string, enabled: boolean) => rpc<SessionInfo>("set_session_daybreak", { session_id, enabled }),
  deleteSession: (session_id: string) => rpc<null>("delete_session", { session_id }),
  /** An empty goal clears it. */
  setGoal: (session_id: string, goal: string) => rpc<SessionInfo>("set_goal", { session_id, goal }),
  /** Withdraw a queued message (index into `queued`). */
  dequeue: (session_id: string, index: number) => rpc<null>("dequeue", { session_id, index }),
  /** Change the text of a queued message. */
  editQueued: (session_id: string, index: number, text: string) => rpc<null>("edit_queued", { session_id, index, text }),
  /** Hand a queued message to the agent now, as steering (read at its next step). */
  steerQueued: (session_id: string, index: number) => rpc<null>("steer_queued", { session_id, index }),
  /** Send a queued message now: the running turn stops and the next starts with it. */
  interruptQueued: (session_id: string, index: number) => rpc<null>("interrupt_queued", { session_id, index }),
  listBoards: (session_id?: string) => rpc<Whiteboard[]>("list_boards", session_id ? { session_id } : {}),
  getBoard: (id: string) => rpc<Whiteboard>("get_board", { id }),
  /**
   * Show something beside a chat yourself (a web page, an image, notes): a board the agent can look at.
   * `placed`: this device already shows it in a tile of its own, so the server adds none.
   */
  createBoard: (session_id: string, title: string, content: BoardContent, placed = false) =>
    rpc<Whiteboard>("create_board", { session_id, title, content, placed }),
  /** Removes the board and its tiles everywhere. */
  deleteBoard: (id: string) => rpc<null>("delete_board", { id }),
  answerBoard: (id: string, ask_id: string, answer: AskAnswer) => rpc<null>("answer_board", { id, ask_id, answer }),
  /** Where the server serves an HTML board as a page (browser UI on the server's machine). */
  boardLink: (id: string) => rpc<{ path: string }>("board_link", { id }),
  forkSession: (session_id: string, at_seq?: number) =>
    rpc<SessionInfo>("fork_session", at_seq === undefined ? { session_id } : { session_id, at_seq }),
  /** Undo what happened after entry `to_seq` (the last one kept): the files, the conversation or both. */
  rewind: (session_id: string, to_seq: number, o: { files: boolean; conversation: boolean; dryRun?: boolean }) =>
    rpc<RewindResult>("rewind", { session_id, to_seq, files: o.files, conversation: o.conversation, dry_run: !!o.dryRun }),
  send: (session_id: string, content: ContentPart[], delivery: Delivery = "auto", attachments: Attachment[] = []) =>
    rpc<null>("send", { session_id, content, delivery, attachments }),
  abort: (session_id: string) => rpc<null>("abort", { session_id }),
  compact: (session_id: string, instructions?: string) =>
    rpc<null>("compact", { session_id, instructions: instructions?.trim() || null }),
  respondApproval: (session_id: string, request_id: string, decision: ApprovalDecision) =>
    rpc<null>("respond_approval", { session_id, request_id, decision }),
  search: (query: string, limit = 30) => rpc<SearchHit[]>("search", { query, limit }),
  listProviders: (refresh = false) => rpc<ProviderInfo[]>("list_providers", { refresh }),
  saveProvider: (spec: ProviderSpec) => rpc<ProviderInfo>("save_provider", { spec }),
  removeProvider: (id: string) => rpc<null>("remove_provider", { id }),
  startOauth: (provider: string) => rpc<OAuthStart>("start_oauth", { provider }),
  completeOauth: (provider: string, input: string) =>
    rpc<ProviderInfo>("complete_oauth", { provider, input }),
  logout: (provider: string) => rpc<null>("logout", { provider }),
  setDefaultModel: (model: ModelRef) => rpc<null>("set_default_model", { model }),
  /** Small model mode for one model ("auto" goes back to Tessera's choice); providers_changed follows. */
  setModelSmall: (provider: string, model: string, mode: SmallMode) =>
    rpc<null>("set_model_small", { provider, model, mode }),
  /** The helper limits of chats that didn't choose their own, and the server's capacity. */
  getHelpers: bare<HelperSettings>("get_helpers"),
  /** Those defaults (`[agent] max_subagents`, `subagent_steps`, `subagent_minutes`); used at once. */
  setHelpers: (at_once: number, steps?: number, minutes?: number, enabled?: boolean) =>
    rpc<HelperSettings>("set_helpers", { at_once, steps, minutes, enabled }),
  /** One chat's own helper limits (left out: the server's default; 0: no limit). */
  setChatHelpers: (session_id: string, limits: HelperLimits) =>
    rpc<SessionInfo>("set_chat_helpers", { session_id, limits }),
  /** Tessera's model roles (default, utility, memory…) and the user's own. */
  getModelRoles: bare<ModelRoles>("get_model_roles"),
  /** `provider/model` or `@other-role`; null or "" clears it (it falls back again). */
  setModelRole: (role: string, model: string | null) =>
    rpc<ModelRoles>("set_model_role", model ? { role, model } : { role }),
  listBots: bare<Bot[]>("list_bots"),
  saveBot: (bot: Bot) => rpc<Bot>("save_bot", { bot }),
  deleteBot: (id: string) => rpc<null>("delete_bot", { id }),
  createChannel: (title: string, members: string[], cwd?: string | null) =>
    rpc<SessionInfo>("create_channel", { title, members, cwd: cwd ?? null }),
  listJobs: bare<Job[]>("list_jobs"),
  saveJob: (job: Job) => rpc<Job>("save_job", { job }),
  deleteJob: (id: string) => rpc<null>("delete_job", { id }),
  runJob: (id: string) => rpc<SessionInfo>("run_job", { id }),
  getMemory: bare<MemoryState>("get_memory"),
  setMemory: (target: MemoryTarget, content: string) =>
    rpc<MemoryState>("set_memory", { target, content }),
  listSkills: (cwd?: string | null) => rpc<SkillInfo[]>("list_skills", { cwd: cwd ?? null }),
  readSkill: (name: string, cwd?: string | null) => rpc<string>("read_skill", { name, cwd: cwd ?? null }),
  deleteSkill: (name: string, cwd?: string | null) => rpc<null>("delete_skill", { name, cwd: cwd ?? null }),
  listDevices: bare<DeviceInfo[]>("list_devices"),
  revokeDevice: (id: string) => rpc<null>("revoke_device", { id }),
  createPairing: bare<PairingCode>("create_pairing"),
  listDir: (path: string) => rpc<DirEntry[]>("list_dir", { path }),
  /** "New folder": one folder, `name`, inside the existing folder `parent` (the server's own rules for names). */
  createDir: (parent: string, name: string) => rpc<DirEntry>("create_dir", { parent, name }),
  listSecrets: bare<SecretInfo[]>("list_secrets"),
  setSecret: (name: string, value: string) => rpc<null>("set_secret", { name, value }),
  deleteSecret: (name: string) => rpc<null>("delete_secret", { name }),
  /** The user's answer about a secret kept for one chat (`SecretInfo.chat`): save it, keep it there, or forget it. */
  decideChatSecret: (session_id: string, name: string, choice: ChatSecretChoice) =>
    rpc<null>("decide_chat_secret", { session_id, name, choice }),
  /** Values are write-only; an empty field value keeps what is stored. `bots` (null: any chat), `on_server`,
   * `on_machines` left out keep what is set; `new_name` renames it; `machines_default` makes its machines follow
   * the setting for all passwords (or its own `on_machines`). */
  saveSecret: (p: {
    name: string;
    kind: SecretKind;
    description: string;
    fields: Record<string, string>;
    remove_fields?: string[];
    ask: boolean;
    hosts: string[];
    bots?: string[] | null;
    on_server?: boolean;
    on_machines?: string[];
    new_name?: string;
    machines_default?: boolean;
  }) => rpc<SecretInfo>("save_secret", { ...p, remove_fields: p.remove_fields ?? [] }),
  setupStatus: bare<SetupStatus>("setup_status"),
  completeSetup: bare<null>("complete_setup"),
  detectLocalModels: bare<ProviderInfo[]>("detect_local_models"),
  /** The user's own Claude Code on the server: installed? signed in? (`refresh`: ask it again). */
  /** One Claude account's Claude Code (default: the usual one). */
  claudeCodeStatus: (refresh = false, provider?: string) => rpc<ClaudeCodeInfo>("claude_code_status", { refresh, ...(provider && provider !== "claude-code" ? { provider } : {}) }),
  /** Another ChatGPT or Claude subscription, named by the user (the server picks its id). */
  addAccount: (kind: ProviderKind, name: string) => rpc<ProviderInfo>("add_account", { kind, name }),
  renameProvider: (id: string, name: string) => rpc<ProviderInfo>("rename_provider", { id, name }),
  getAccounts: bare<AccountSettings>("get_accounts"),
  setAccounts: (continue_on_next: boolean) => rpc<AccountSettings>("set_accounts", { continue_on_next }),
  /** A built-in user guide (`security`, `remote-access`, `claude-code`) as markdown. */
  getGuide: (name: string) => rpc<Guide>("get_guide", { name }),
  cloudflareZones: (token: string) => rpc<CloudflareZone[]>("cloudflare_zones", { token }),
  cloudflareSetup: (token: string, zone_id: string, hostname: string) =>
    rpc<RemoteStatus>("cloudflare_setup", { token, zone_id, hostname }),
  cloudflareRemove: (token: string) => rpc<RemoteStatus>("cloudflare_remove", { token }),
  /** Another year for the devices' Cloudflare key: with an API token, or (none) "I renewed it in Cloudflare". */
  cloudflareRenew: (token?: string) => rpc<RemoteStatus>("cloudflare_renew", token ? { token } : {}),
  remoteStatus: bare<RemoteStatus>("remote_status"),
  /** "Use Tessera in a web browser" on or off; `token`: Cloudflare's, when it must let browsers through first. */
  setWebAccess: (on: boolean, token?: string) => rpc<RemoteStatus>("set_web_access", { on, ...(token ? { token } : {}) }),
  getMosaics: bare<MosaicLayout>("get_mosaics"),
  saveMosaics: (layout: MosaicLayout) => rpc<MosaicLayout>("save_mosaics", { layout }),
  getVoice: bare<VoiceConfig>("get_voice"),
  saveVoice: (config: VoiceConfig, api_key: string | null) => rpc<VoiceConfig>("save_voice", { config, api_key }),
  transcribe: (audio: string, mime: string, engine?: string, partial?: boolean, signal?: AbortSignal, language?: string) => voiceCall<Transcript>({ method: "transcribe", params: { audio, mime, ...(engine ? { engine } : {}), ...(partial ? { partial } : {}), ...(language ? { language } : {}) } }, signal),
  speak: (text: string, engine?: string, signal?: AbortSignal) => voiceCall<SpeechAudio>({ method: "speak", params: { text, ...(engine ? { engine } : {}) } }, signal),
  installVoice: bare<VoiceConfig>("install_voice"),
  deleteVoiceRecordings: bare<VoiceConfig>("delete_voice_recordings"),
  getWebSearch: bare<WebSearchInfo>("get_web_search"),
  /** `headers` omitted keeps the stored ones; `{}` clears them (values are write-only). */
  saveWebSearch: (search: string, searxng_url?: string, headers?: Record<string, string>) =>
    rpc<WebSearchInfo>("save_web_search", { search, ...(searxng_url ? { searxng_url } : {}), ...(headers === undefined ? {} : { headers }) }),
  /** Brave or Tavily API key for web search (write-only; "" removes it). */
  setSearchKey: (provider: "brave" | "tavily", key: string) => rpc<WebSearchInfo>("set_search_key", { provider, key }),
  getRemoteAccess: bare<RemoteAccessInfo>("get_remote_access"),
  /** Headers change one by one (`add` sets or replaces, `remove` drops), so Cloudflare Access ones survive. */
  setRemoteAccess: (urls: string[], change: { add?: Record<string, string>; remove?: string[] } = {}) =>
    rpc<RemoteAccessInfo>("set_remote_access", { urls, ...(change.add && Object.keys(change.add).length ? { add_headers: change.add } : {}), remove_headers: change.remove ?? [] }),
  /** `127.0.0.1:<port>` (this computer only) or `0.0.0.0:<port>` (the home network); applies after a restart. */
  setServerBind: (bind: string) => rpc<SetupStatus>("set_server_bind", { bind }),

  // ── notes (the library; see lib/notes.ts) ──
  listNotes: bare<NoteInfo[]>("list_notes"),
  getNote: (id: string) => rpc<Note>("get_note", { id }),
  /** `id` null creates one; a stale `base_version` is refused (`conflict`). */
  saveNote: (p: { id: string | null; title: string; content: string; base_version: number | null; session_id?: string | null }) =>
    rpc<Note>("save_note", {
        title: p.title,
        content: p.content,
        ...(p.id ? { id: p.id } : {}),
        ...(p.base_version != null ? { base_version: p.base_version } : {}),
        ...(p.session_id ? { session_id: p.session_id } : {}),
    }),
  deleteNote: (id: string) => rpc<null>("delete_note", { id }),
  /** Which bots may read a note: every bot, or only bots it is given to (the default). */
  setNoteBots: (id: string, all_bots: boolean) => rpc<Note>("set_note_bots", { id, all_bots }),
  setNoteAgents: (id: string, keep_from_agents: boolean) => rpc<Note>("set_note_agents", { id, keep_from_agents }),
  /** The versions kept from before the current one, newest first (without their text). */
  noteHistory: (id: string) => rpc<NoteVersion[]>("note_history", { id }),
  getNoteVersion: (id: string, version: number) => rpc<NoteVersion>("get_note_version", { id, version }),
  /** A page on the server showing `html` like an HTML whiteboard (browser UI; the app serves its own). */
  previewPage: (html: string) => rpc<{ path: string }>("preview_page", { html }),
};

// ── helpers ──────────────────────────────────────────────────────────────

function newSessionState(info: SessionInfo): SessionState {
  return {
    info,
    entries: [],
    entriesVersion: 0,
    historyRevision: 0,
    results: {},
    loaded: false,
    loading: false,
    hasOlderHistory: false,
    loadingOlderHistory: false,
    streaming: null,
    running: {},
    approvals: [],
    queued: [],
    pending: [],
    thinkMs: {},
    toolMs: {},
    turnStart: isBusy(info) ? Date.now() : null,
    statusSince: Date.now(),
    rewind: null,
  };
}

/** Older servers omit `members` (and newer fields); the UI relies on them being set. */
const normInfo = (i: SessionInfo): SessionInfo => {
  const info = Array.isArray(i.members) && typeof i.private === "boolean" && typeof i.steps === "number" && typeof i.fast === "boolean" && typeof i.revision === "number"
    ? i
    : { ...i, members: Array.isArray(i.members) ? i.members : [], private: !!i.private, steps: i.steps ?? 0, fast: !!i.fast, revision: i.revision ?? 0 };
  if (!isCheckAck(info.preview)) return info;
  const previous = state.sessions[info.id]?.info.preview ?? "";
  return { ...info, preview: isCheckAck(previous) ? "" : previous };
};

/** Every private chat seen this run (memory only), so nothing about them is ever persisted, even after they vanish from the store. */
export const privateIds = new Set<string>();

// Cached transcripts survive reconnects; revision watermarks do not.
const sessionConnections = new Map<string, number>();
export function upsertSession(raw: SessionInfo, newBaseline = false): boolean {
  const info = normInfo(raw);
  if (info.private) privateIds.add(info.id);
  const s = state.sessions[info.id];
  const connection = connectionGeneration();
  if (!newBaseline && sessionConnections.get(info.id) === connection && (info.revision ?? 0) < (s?.info.revision ?? 0)) return false;
  sessionConnections.set(info.id, connection);
  if (!s) {
    setState("sessions", info.id, newSessionState(info));
    return true;
  }
  const wasBusy = s.info.status !== "idle";
  const wasRunning = isBusy(s.info);
  if (s.info.status !== info.status) setState("sessions", info.id, "statusSince", Date.now());
  setState("sessions", info.id, "info", reconcile(info));
  if (!wasRunning && isBusy(info)) setState("sessions", info.id, "turnStart", Date.now());
  else if (wasRunning && !isBusy(info)) setState("sessions", info.id, "turnStart", null);
  // When a turn is over, no stream or tool can still be live.
  if (info.status === "idle" || info.status === "error" || info.status === "interrupted") {
    if (s.streaming || Object.keys(s.running).length) {
      setState("sessions", info.id, produce((d) => {
        d.streaming = null;
        d.running = {};
      }));
    }
    if (wasBusy && s.pending.length) setState("sessions", info.id, "pending", []);
  }
  return true;
}

function lastSeqOf(s: SessionState): number {
  return s.entries.length ? s.entries[s.entries.length - 1].seq : 0;
}

/** Insert entries (sorted by seq, deduped) into a session draft. */
function mergeEntries(d: SessionState, entries: Entry[]) {
  if (!d.entries.length && !d.pending.length && !Object.keys(d.running).length && entries.every((e, i) => !i || e.seq > entries[i - 1].seq)) {
    // A normal initial snapshot is already sorted/unique. Install its plain data once rather
    // than traversing thousands of entries through the mutable draft's proxies and setters.
    d.entries = entries;
    d.results = Object.fromEntries(entries.filter((e): e is Extract<Entry, { kind: "tool_result" }> => e.kind === "tool_result").map((e) => [e.call_id, e]));
    const last = entries.at(-1)?.seq ?? 0;
    if (d.info.last_seq < last) d.info.last_seq = last;
    return;
  }
  for (const e of entries) {
    const last = d.entries.length ? d.entries[d.entries.length - 1].seq : -1;
    if (e.seq > last) d.entries.push(e);
    else {
      // Rare: out-of-order or duplicate.
      let lo = 0;
      let hi = d.entries.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (d.entries[mid].seq < e.seq) lo = mid + 1;
        else hi = mid;
      }
      if (d.entries[lo]?.seq === e.seq) continue;
      d.entries.splice(lo, 0, e);
      d.entriesVersion++;
    }
    if (e.kind === "tool_result") {
      d.results[e.call_id] = e;
      const live = d.running[e.call_id];
      if (live) d.toolMs[e.call_id] = Math.max(0, Date.now() - live.startedAt);
      delete d.running[e.call_id];
    }
    if (e.kind === "user" && !e.author && d.pending.length) {
      // Drop the matching optimistic message (FIFO by text).
      const text = plainText(e.content);
      const i = d.pending.findIndex((p) => plainText(p.content) === text);
      d.pending.splice(i >= 0 ? i : 0, 1);
    }
  }
  if (d.entries.length && d.info.last_seq < d.entries[d.entries.length - 1].seq) {
    d.info.last_seq = d.entries[d.entries.length - 1].seq;
  }
}

/** Older servers sent plain strings (always follow-ups). */
function normQueue(q: readonly (QueuedMessage | string)[] | undefined): QueuedMessage[] {
  return (q ?? []).map((m) => (typeof m === "string" ? { text: m, steer: false } : m));
}

export function plainText(parts: ContentPart[]): string {
  return parts
    .filter((p): p is Extract<ContentPart, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("\n");
}

function applySnapshot(raw: SessionSnapshot, incremental: boolean) {
  // A snapshot may include hundreds of helpers. Publish one reactive change,
  // not one global catalog scan per child (rewind, helper cards, sidebar).
  batch(() => {
    // Deltas received before the snapshot are already part of it.
    flushDeltas();
    const current = state.sessions[raw.info.id]?.info;
    const connection = connectionGeneration();
    const snap = { ...raw, info: current && sessionConnections.get(raw.info.id) === connection && (raw.info.revision ?? 0) < (current.revision ?? 0) ? current : normInfo(raw.info) };
    sessionConnections.set(raw.info.id, connection);
    const id = snap.info.id;
    if (snap.info.private) privateIds.add(id);
    if (!state.sessions[id]) setState("sessions", id, newSessionState(snap.info));
    setState(
      "sessions",
      id,
      produce((d) => {
        if (d.info.status !== snap.info.status) d.statusSince = Date.now();
        d.info = snap.info;
        d.historyRevision = snap.history_revision ?? 0;
        if (!incremental) {
          historyPages.set(id, { before: snap.history_before ?? null });
          d.hasOlderHistory = snap.history_before != null;
          // Unchanged entries retain their object identity, so a live rewind
          // never re-renders the kept prefix. Changed/reused seqs get fresh data.
          const previous = d.entries;
          const entries = snap.entries.map((e, i) => {
            const old = previous[i];
            return old && JSON.stringify(unwrap(old)) === JSON.stringify(e) ? old : e;
          });
          d.entries = [];
          d.results = {};
          d.entriesVersion++;
          mergeEntries(d, entries);
        } else mergeEntries(d, snap.entries);
        d.loaded = true;
        d.loading = false;
        // Replacing local streaming state with the snapshot's is always correct
        // (a stream we already show keeps its clocks: resyncs don't restart them).
        const st = snap.streaming;
        const was = d.streaming && !d.streaming.ended ? d.streaming : null;
        d.streaming =
          st && !st.content.length && was && !was.content.filter(Boolean).length
            ? was // still waiting for the model's first words, as we showed
            : st && st.content.length
            ? {
                content: st.content.map((p) =>
                  p.type === "tool_call" && typeof p.arguments !== "string"
                    ? { ...p, arguments: JSON.stringify(p.arguments ?? {}) }
                    : p,
                ),
                model: st.model,
                author: st.author ?? d.info.bot_id,
                startedAt: was?.startedAt ?? Date.now(),
                ended: false,
                ...(was?.thinkStart ? { thinkStart: was.thinkStart } : {}),
                ...(was?.thinkEnd ? { thinkEnd: was.thinkEnd } : {}),
              }
            : null;
        const running: Record<string, LiveTool> = {};
        for (const t of st?.running_tools ?? []) running[t.call_id] = { ...t, startedAt: d.running[t.call_id]?.startedAt ?? Date.now() };
        d.running = running;
        d.approvals = snap.approvals;
        d.queued = normQueue(snap.queued);
        if (isBusy(d.info)) d.turnStart ??= Date.now();
        else d.turnStart = null;
      }),
    );
    // Its subagents as the server has them now: every device counts the same ones (subagents aren't in
    // the session list, and one this device last saw running may have been stopped since).
    if (raw.children) {
      for (const c of raw.children) upsertSession(c);
      const known = new Set(raw.children.map((c) => c.id));
      for (const s of Object.values(state.sessions)) {
        if (s.info.parent_id === id && s.info.kind === "subagent" && !s.info.internal && !known.has(s.info.id)) setState("sessions", s.info.id, undefined!);
      }
    }
  });
}

const inflight = new Map<string, { promise: Promise<void>; generation: number; connection: number }>();
export const historyPaging = {
  pages: new Map<string, { before: number | null }>(),
  inflight: new Map<string, Promise<void>>(),
  generation: () => bootGen,
};
const { pages: historyPages, inflight: olderInflight } = historyPaging;

/** Older history is reader-initiated, never part of the first-paint bundle. */
export function loadOlderHistory(id: string): Promise<void> {
  const existing = olderInflight.get(id);
  if (existing) return existing;
  const page = historyPages.get(id);
  if (!state.sessions[id]?.loaded || page?.before == null) return Promise.resolve();
  const generation = bootGen, connection = connectionGeneration();
  setState("sessions", id, "loadingOlderHistory", true);
  const promise = import("./history").then((m) => {
    if (generation === bootGen && connection === connectionGeneration() && historyPages.get(id) === page) return m.prependHistory(id);
  }).finally(() => {
    if (olderInflight.get(id) !== promise) return;
    olderInflight.delete(id);
    if (generation === bootGen && connection === connectionGeneration() && state.sessions[id])
      setState("sessions", id, "loadingOlderHistory", false);
  });
  olderInflight.set(id, promise);
  return promise;
}
/** Full histories explicitly requested by a view, even if its first snapshot failed. */
const requestedHistory = new Set<string>();
/** Sessions to load once more when the load in flight is done (COMPOSER: no stale status line). */
const loadAgain = new Set<string>();
/** Events seen per session, to tell whether some arrived while a snapshot was on its way. */
const sessionEvents = new Map<string, number>();

/** Fetch a session's history if we don't have it yet. */
const peeking = new Map<string, object>();
/**
 * A session's info without its transcript, when nothing told us about it yet
 * (a finished subagent's card after a reload: subagents aren't listed).
 */
export function peekSession(id: string): void {
  if (state.sessions[id] || peeking.has(id)) return;
  const stamp = {};
  const generation = bootGen;
  const connection = connectionGeneration();
  peeking.set(id, stamp);
  api
    .getSession(id, Number.MAX_SAFE_INTEGER)
    .then((snap) => {
      if (generation === bootGen && connection === connectionGeneration() && !state.sessions[id]) upsertSession(snap.info);
    })
    .catch(() => {})
    .finally(() => { if (peeking.get(id) === stamp) peeking.delete(id); });
}

export function ensureLoaded(id: string): Promise<void> {
  const s = state.sessions[id];
  if (s?.loaded) return Promise.resolve();
  return loadSession(id, false);
}

export function loadSession(id: string, incremental: boolean, round = 0): Promise<void> {
  requestedHistory.add(id);
  const existing = inflight.get(id);
  if (existing && existing.generation === bootGen && existing.connection === connectionGeneration()) {
    // Asked again (a resync) while a load is on its way: its answer may
    // predate what we missed, so load once more after it.
    if (incremental) loadAgain.add(id);
    return existing.promise;
  }
  const s = state.sessions[id];
  if (s) setState("sessions", id, "loading", true);
  const since = incremental && s?.loaded ? lastSeqOf(s) : undefined;
  const seen = sessionEvents.get(id) ?? 0;
  const generation = bootGen;
  const connection = connectionGeneration();
  const current = () => generation === bootGen && connection === connectionGeneration();
  const p = api
    .getSession(id, since, since === undefined ? {} : undefined)
    .then((snap) => {
      if (!current()) return;
      if (since !== undefined && (state.sessions[id]?.historyRevision ?? 0) !== (snap.history_revision ?? 0)) {
        // Reused seqs are not a continuation. Replace from a full snapshot,
        // but keep the live view mounted while it arrives (including its draft).
        return api.getSession(id, undefined, {}).then((full) => {
          if (!current()) return;
          applySnapshot(full, false);
          if ((sessionEvents.get(id) ?? 0) !== seen) loadAgain.add(id);
        });
      }
      applySnapshot(snap, since !== undefined);
      // Shorter than what we had: it was rewound while we weren't listening, so
      // what we hold after that point may differ. Reload it all (once this load is done).
      if (since !== undefined && snap.info.last_seq < since) setTimeout(() => { if (current()) void loadSession(id, false).catch(() => {}); });
      // Events for this chat came while the snapshot was on its way. The
      // server may send them before or after it, so the snapshot can be older
      // than what they said (a turn that ended would look busy forever): ask
      // again so the last word is the server's current state.
      else if ((sessionEvents.get(id) ?? 0) !== seen) loadAgain.add(id);
    })
    .catch((e) => {
      if (!current()) return;
      if (state.sessions[id]) setState("sessions", id, "loading", false);
      throw e;
    })
    .finally(() => {
      if (inflight.get(id)?.promise !== p) return;
      inflight.delete(id);
      const again = loadAgain.delete(id);
      const nextRound = generation !== bootGen ? 0 : round + 1;
      // A reconnect can queue a retry before the first, still-pending snapshot rejects.
      // It must not require loaded=true, or that mounted view can stay empty forever.
      if (again && nextRound <= 2 && requestedHistory.has(id))
        setTimeout(() => {
          // Removal/server switch can cancel this queued retry before the timer runs.
          if (requestedHistory.has(id) && connection === connectionGeneration()) void loadSession(id, true, nextRound).catch(() => {});
        }, 300);
    });
  inflight.set(id, { promise: p, generation, connection });
  return p;
}

/** Bumped whenever the providers were fetched again (model roles live beside them). */
const [providersTick, setProvidersTick] = createSignal(0);
export { providersTick };

export async function refreshProviders(refresh = false) {
  const generation = bootGen, connection = connectionGeneration();
  try {
    const providers = await api.listProviders(refresh);
    if (generation !== bootGen || connection !== connectionGeneration()) return;
    setState("providers", reconcile(providers, { key: "id" }));
    setState("providersLoaded", true);
    setProvidersTick((n) => n + 1);
  } catch (e) {
    console.warn("list_providers failed", e);
  }
}

export async function refreshArchived() {
  const generation = bootGen, connection = connectionGeneration();
  const list: SessionInfo[] = [];
  if (state.server?.session_list_pages) {
    let before: SessionListCursor | null = null;
    do {
      const page: SessionListPage = await api.listSessionPage(true, before);
      if (generation !== bootGen || connection !== connectionGeneration()) return;
      list.push(...page.sessions);
      before = page.next_cursor;
      if (before) await afterPaint();
    } while (before);
  } else list.push(...await api.listSessions(true));
  if (generation !== bootGen || connection !== connectionGeneration()) return;
  setState("archived", list.map(normInfo));
}

// ── bootstrap / resync ───────────────────────────────────────────────────

let bootGen = 0;
let bootstrapConnection: number | undefined;

// A task boundary after paint, not a microtask (which can still block input).
const afterPaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => setTimeout(resolve, 0)));

function upsertSummary(info: SessionInfo, baseline: boolean) {
  const existing = state.sessions[info.id];
  // A list is not a snapshot: don't erase a loaded chat's task ledger or
  // replace metadata received in a newer event while this page was in flight.
  if (existing && !baseline && (existing.info.updated_at > info.updated_at || (existing.info.revision ?? 0) > (info.revision ?? 0))) return;
  upsertSession(existing?.loaded ? { ...info, task: existing.info.task } : info, baseline);
}

/** Called on every (re)connect. Critical metadata first, other lists after paint. */
export async function bootstrap() {
  const gen = ++bootGen;
  const eventsBefore = new Map(sessionEvents);
  for (const id of olderInflight.keys()) {
    if (state.sessions[id]) setState("sessions", id, "loadingOlderHistory", false);
  }
  olderInflight.clear();
  const connection = connectionGeneration();
  const info = await api.hello();
  if (gen !== bootGen || connection !== connectionGeneration()) return;
  setState("server", info);
  const [first, bots] = await Promise.all([
    info.session_list_pages
      ? api.listSessionPage(false, null, openSessionIds())
      : api.listSessions(false).then((sessions): SessionListPage => ({ sessions, next_cursor: null })),
    api.listBots().catch(() => [] as Bot[]),
  ]);
  const sessions = first.sessions;
  if (gen !== bootGen || connection !== connectionGeneration()) return;
  const newBaseline = bootstrapConnection !== connection;
  bootstrapConnection = connection;
  const open = new Set(openSessionIds());
  batch(() => {
    const seen = new Set<string>();
    for (const s of sessions) {
      seen.add(s.id);
      if (info.session_list_pages) upsertSummary(s, newBaseline);
      else upsertSession(s, newBaseline);
    }
    // A first page is not a complete catalog. Remove absent rows only when
    // all pages have arrived (open tiles/helpers are independently synced).
    if (!info.session_list_pages) for (const id of Object.keys(state.sessions)) {
      if (!seen.has(id) && !open.has(id)) setState("sessions", produce((d) => void delete d[id]));
    }
    setState("bots", reconcile(bots, { key: "id" }));
    setState("botsLoaded", true);
    setState("sessionsLoaded", true);
    setState("ready", true);
    setState("everConnected", true);
  });
  for (const cb of bootListeners) cb();
  // Restored tiles request their newest history immediately, not behind jobs,
  // boards, notes, providers or the rest of the sidebar catalog.
  const resync = resyncOpen();
  void (async () => {
    await afterPaint();
    const current = () => gen === bootGen && connection === connectionGeneration();
    if (!current()) return;
    void refreshProviders(); // cached only; explicit refresh remains live
    void Promise.all([
      api.listJobs().catch(() => [] as Job[]),
      api.listBoards().catch(() => [] as Whiteboard[]),
      api.listNotes().catch(() => null),
    ]).then(([jobs, boards, notes]) => {
      if (!current()) return;
      batch(() => {
        setState("jobs", reconcile(jobs, { key: "id" }));
        setState("boards", reconcile(Object.fromEntries(boards.map((b) => [b.id, b]))));
        setState("notes", reconcile(Object.fromEntries((notes ?? []).map((n) => [n.id, n]))));
        setState("notesLoaded", notes !== null);
      });
    });
    if (!info.session_list_pages) return;
    const seen = new Set(sessions.map((s) => s.id));
    let before = first.next_cursor;
    while (before && current()) {
      // Finish the lightweight catalog for search/bot profiles/old chats,
      // yielding between pages. Never insert 3,384 historical job sessions.
      const page = await api.listSessionPage(false, before);
      if (!current()) return;
      batch(() => {
        for (const s of page.sessions) { seen.add(s.id); upsertSummary(s, false); }
      });
      before = page.next_cursor;
      await afterPaint();
    }
    if (!current()) return;
    const open = new Set(openSessionIds());
    batch(() => {
      for (const s of Object.values(state.sessions)) {
        if (s.info.kind !== "subagent" && !seen.has(s.info.id) && !open.has(s.info.id) && !s.info.private && (sessionEvents.get(s.info.id) ?? 0) === (eventsBefore.get(s.info.id) ?? 0))
          setState("sessions", produce((d) => void delete d[s.info.id]));
      }
    });
  })().catch((e) => console.warn("session catalog failed", e));
  await resync;
}

export async function resyncOpen() {
  // Unseen restored tiles have no history to resync. Previously requested first snapshots
  // must retry too: a disconnect may have rejected them before loaded became true.
  const ids = [...new Set(openSessionIds())].filter((id) =>
    state.sessions[id]?.loaded || (requestedHistory.has(id) && inflight.get(id)?.generation !== bootGen),
  );
  await Promise.all(
    ids.map((id) =>
      loadSession(id, true).catch((e) => {
        if ((e as { code?: string }).code === "not_found") removeSessionLocal(id);
      }),
    ),
  );
}

function removeSessionLocal(id: string) {
  requestedHistory.delete(id);
  historyPages.delete(id);
  olderInflight.delete(id);
  sessionConnections.delete(id);
  setState("sessions", produce((d) => void delete d[id]));
  for (const cb of removedListeners) cb(id);
}

export function resetServerState() {
  bootGen++;
  bootstrapConnection = undefined;
  sessionConnections.clear();
  requestedHistory.clear();
  inflight.clear();
  historyPages.clear();
  olderInflight.clear();
  peeking.clear();
  loadAgain.clear();
  sessionEvents.clear();
  batch(() => {
    setState("server", null);
    setState("sessions", reconcile({}));
    setState("sessionsLoaded", false);
    setState("ready", false);
    setState("archived", null);
    setState("providers", []);
    setState("providersLoaded", false);
    setState("jobs", []);
    setState("bots", []);
    setState("botsLoaded", false);
    setState("progress", {});
    setState("boards", reconcile({}));
    setState("notes", reconcile({}));
    setState("notesLoaded", false);
    setState("answered", reconcile({}));
  });
}

// ── events ───────────────────────────────────────────────────────────────

type DeltaEvent = Extract<Event, { type: "stream_delta" } | { type: "tool_output" }>;

// Stream deltas (and a running tool's output) arrive dozens of times a second
// per tile, and every one used to re-run everything that reads the stream.
// They are buffered and applied once per frame instead (adjacent deltas of the
// same part, or output of the same call, merged), and any other event flushes
// the buffer first so ordering is preserved.
const deltaBuf: DeltaEvent[] = [];
let deltaRaf = 0;
let deltaTimer: ReturnType<typeof setTimeout> | undefined;

function scheduleDeltas() {
  if (deltaRaf) return;
  deltaRaf = requestAnimationFrame(flushDeltas);
  // rAF does not run in hidden tabs: never let deltas pile up there.
  deltaTimer = setTimeout(flushDeltas, 120);
}

function coalesce(evs: DeltaEvent[]): DeltaEvent[] {
  const out: DeltaEvent[] = [];
  for (const e of evs) {
    const prev = out[out.length - 1];
    if (e.type === "tool_output" || prev?.type === "tool_output") {
      if (prev?.type === "tool_output" && e.type === "tool_output" && prev.session_id === e.session_id && prev.call_id === e.call_id) out[out.length - 1] = { ...prev, chunk: prev.chunk + e.chunk };
      else out.push(e);
      continue;
    }
    const a = prev?.delta;
    const b = e.delta;
    if (prev && prev.session_id === e.session_id && a.part === b.part && a.index === b.index) {
      if (a.part === "tool_call" && b.part === "tool_call") {
        out[out.length - 1] = { ...prev, delta: { ...a, id: b.id || a.id, name: b.name || a.name, arguments: a.arguments + b.arguments } };
        continue;
      }
      if ((a.part === "text" || a.part === "thinking") && a.part === b.part) {
        out[out.length - 1] = { ...prev, delta: { ...a, text: a.text + (b as { text: string }).text } };
        continue;
      }
    }
    out.push(e);
  }
  return out;
}

export function flushDeltas() {
  if (deltaRaf) cancelAnimationFrame(deltaRaf);
  clearTimeout(deltaTimer);
  deltaRaf = 0;
  deltaTimer = undefined;
  if (!deltaBuf.length) return;
  const evs = coalesce(deltaBuf.splice(0));
  batch(() => {
    for (const e of evs) applyEventInner(e);
  });
}

export function applyEvent(e: Event) {
  // The remote browser's events (frames above all) are lib/browser.ts's alone.
  if ((e.type as string).startsWith("browser_")) return;
  // MACHINES-APP: a machine came, went, was paused or changed what it allows (lib/machines.ts).
  if ((e.type as string) === "machines_changed") return machinesChanged();
  const sid = "session_id" in e ? e.session_id : e.type === "session_updated" ? e.session.id : undefined;
  if (sid) sessionEvents.set(sid, (sessionEvents.get(sid) ?? 0) + 1);
  if (e.type === "stream_delta" || e.type === "tool_output") {
    deltaBuf.push(e);
    scheduleDeltas();
    return;
  }
  if (deltaBuf.length) flushDeltas();
  batch(() => applyEventInner(e));
}

function applyEventInner(e: Event) {
  switch (e.type) {
    case "note_updated":
      setState("notes", e.note.id, reconcile(e.note));
      for (const cb of noteListeners) cb({ type: "note_updated", note: e.note });
      return;
    case "note_removed":
      setState("notes", produce((d) => void delete d[e.id]));
      for (const cb of noteListeners) cb({ type: "note_removed", id: e.id });
      return;
    case "board_snapshot_request":
      return; // answered by lib/snapshot.ts
    case "session_updated": {
      const info = normInfo(e.session);
      if (!upsertSession(info)) return;
      if (state.archived) {
        const inArchive = state.archived.some((s) => s.id === info.id);
        if (info.archived && !inArchive) setState("archived", (l) => [info, ...(l ?? [])]);
        else if (!info.archived && inArchive) setState("archived", (l) => (l ?? []).filter((s) => s.id !== info.id));
        else if (inArchive) setState("archived", (s) => s.id === info.id, info);
      }
      return;
    }
    case "session_removed":
      if (state.archived) setState("archived", (l) => (l ?? []).filter((s) => s.id !== e.session_id));
      removeSessionLocal(e.session_id);
      return;
    case "session_rewound": {
      historyPages.delete(e.session_id);
      const s = state.sessions[e.session_id];
      if (s) setState("sessions", e.session_id, "hasOlderHistory", false);
      if (!s) return;
      // This device's own rewind: the caller applies it once the result is in.
      if (localRewinds.has(e.session_id)) return;
      if (!s.loaded || reducedMotion()) return applyRewind(e.session_id, e.to_seq);
      setState("sessions", e.session_id, "rewind", { from: e.to_seq, local: false });
      setTimeout(() => applyRewind(e.session_id, e.to_seq), REWIND_FADE_MS);
      return;
    }
    case "entry_added": {
      const s = state.sessions[e.session_id];
      if (!s) return;
      if (!s.loaded) {
        if (e.entry.seq > s.info.last_seq) setState("sessions", e.session_id, "info", "last_seq", e.entry.seq);
        return;
      }
      setState(
        "sessions",
        e.session_id,
        produce((d) => {
          if (e.entry.kind === "assistant" && d.streaming) {
            if (d.streaming.thinkStart) {
              d.thinkMs[e.entry.seq] = (d.streaming.thinkEnd ?? Date.now()) - d.streaming.thinkStart;
            }
            if (d.streaming.ended) d.streaming = null;
          }
          mergeEntries(d, [e.entry]);
        }),
      );
      return;
    }
    case "entries_changed": {
      // An in-flight older page may still contain the rewritten copy.
      const page = historyPages.get(e.session_id);
      if (page) historyPages.set(e.session_id, { ...page });
      // Stored items were rewritten in place (a secret was moved out of the chat): show the new copies.
      if (!state.sessions[e.session_id]?.loaded) return;
      setState(
        "sessions",
        e.session_id,
        produce((d) => {
          for (const n of e.entries) {
            const i = d.entries.findIndex((x) => x.seq === n.seq);
            if (i < 0) continue;
            d.entries[i] = n;
            if (n.kind === "tool_result") d.results[n.call_id] = n;
          }
          d.entriesVersion++;
        }),
      );
      return;
    }
    case "stream_started": {
      const s = state.sessions[e.session_id];
      if (!s) return;
      setState("sessions", e.session_id, "streaming", {
        content: [],
        model: e.model,
        author: e.author ?? s.info.bot_id,
        startedAt: Date.now(),
        ended: false,
      });
      return;
    }
    case "stream_delta": {
      const s = state.sessions[e.session_id];
      if (!s) return;
      const d = e.delta;
      if (!s.streaming || s.streaming.ended) {
        setState("sessions", e.session_id, "streaming", {
          content: [],
          model: s.streaming?.model ?? s.info.model,
          author: s.streaming?.author ?? s.info.bot_id,
          startedAt: Date.now(),
          ended: false,
        });
      }
      const st = state.sessions[e.session_id].streaming!;
      const now = Date.now();
      if (d.part === "thinking" && !st.thinkStart) setState("sessions", e.session_id, "streaming", "thinkStart", now);
      if (d.part !== "thinking" && st.thinkStart && !st.thinkEnd)
        setState("sessions", e.session_id, "streaming", "thinkEnd", now);
      const cur = st.content[d.index];
      if (d.part === "text" || d.part === "thinking") {
        if (cur && cur.type === d.part) {
          setState(
            "sessions",
            e.session_id,
            "streaming",
            "content",
            d.index,
            produce((p) => {
              (p as { text: string }).text += d.text;
            }),
          );
        } else {
          setState("sessions", e.session_id, "streaming", "content", d.index, { type: d.part, text: d.text });
        }
      } else {
        if (cur && cur.type === "tool_call") {
          setState(
            "sessions",
            e.session_id,
            "streaming",
            "content",
            d.index,
            produce((p) => {
              const tc = p as Extract<AssistantPart, { type: "tool_call" }>;
              tc.arguments = String(tc.arguments ?? "") + d.arguments;
              if (d.id) tc.id = d.id;
              if (d.name) tc.name = d.name;
            }),
          );
        } else {
          setState("sessions", e.session_id, "streaming", "content", d.index, {
            type: "tool_call",
            id: d.id,
            name: d.name,
            arguments: d.arguments,
          });
        }
      }
      return;
    }
    case "stream_ended": {
      const s = state.sessions[e.session_id];
      if (!s?.streaming) return;
      setState("sessions", e.session_id, "streaming", "ended", true);
      // Safety net: if no final entry follows shortly, drop the stream.
      setTimeout(() => {
        const cur = state.sessions[e.session_id]?.streaming;
        if (cur && cur.ended) setState("sessions", e.session_id, "streaming", null);
      }, 4000);
      return;
    }
    case "tool_started": {
      if (!state.sessions[e.session_id]) return;
      setState("sessions", e.session_id, "running", e.call_id, {
        call_id: e.call_id,
        name: e.name,
        output: "",
        startedAt: Date.now(),
      });
      return;
    }
    case "tool_output": {
      const s = state.sessions[e.session_id];
      if (!s) return;
      if (!s.running[e.call_id]) {
        setState("sessions", e.session_id, "running", e.call_id, {
          call_id: e.call_id,
          name: "tool",
          output: "",
          startedAt: Date.now(),
        });
      }
      setState("sessions", e.session_id, "running", e.call_id, "output", (o) => {
        const next = o + e.chunk;
        return next.length > 256_000 ? next.slice(-200_000) : next;
      });
      return;
    }
    case "approval_requested": {
      const s = state.sessions[e.session_id];
      if (!s || s.approvals.some((a) => a.id === e.request.id)) return;
      setState("sessions", e.session_id, "approvals", (l) => [...l, e.request]);
      return;
    }
    case "approval_resolved":
      if (!state.sessions[e.session_id]) return;
      setState("sessions", e.session_id, "approvals", (l) => l.filter((a) => a.id !== e.request_id));
      return;
    case "queue_changed":
      if (!state.sessions[e.session_id]) return;
      setState("sessions", e.session_id, "queued", normQueue(e.queued));
      return;
    case "job_updated": {
      const i = state.jobs.findIndex((j) => j.id === e.job.id);
      if (i >= 0) setState("jobs", i, reconcile(e.job));
      else setState("jobs", (l) => [...l, e.job]);
      return;
    }
    case "job_removed":
      setState("jobs", (l) => l.filter((j) => j.id !== e.id));
      return;
    case "bot_updated": {
      const i = state.bots.findIndex((b) => b.id === e.bot.id);
      if (i >= 0) setState("bots", i, reconcile(e.bot));
      else setState("bots", (l) => [...l, e.bot]);
      return;
    }
    case "bot_removed":
      setState("bots", (l) => l.filter((b) => b.id !== e.id));
      return;
    case "mosaics_changed":
      for (const cb of mosaicListeners) cb(e.layout);
      return;
    case "board_updated":
      setState("boards", e.board.id, reconcile(e.board));
      return;
    case "board_removed":
      setState("boards", produce((d) => void delete d[e.id]));
      for (const cb of boardRemovedListeners) cb(e.id);
      return;
    case "setup_progress": {
      const list = state.progress[e.task] ?? [];
      const i = list.findIndex((x) => x.step === e.step);
      const item = { step: e.step, done: e.done, error: e.error };
      setState("progress", e.task, i >= 0 ? list.map((x, j) => (j === i ? item : x)) : [...list, item]);
      return;
    }
    case "providers_changed":
      void refreshProviders();
      return;
    case "resync":
      // The server dropped events for us: reload what the panes show.
      void resyncOpen();
      return;
    case "notice": {
      // Chat progress, reconnects and automatic retry notices already live
      // in the transcript. Toast only errors and server-wide notices.
      if (e.session_id && e.level !== "error") return;
      const title = e.session_id ? state.sessions[e.session_id]?.info.title || undefined : undefined;
      toast({ key: `notice:${e.session_id ?? "server"}`, level: e.level === "warning" ? "warning" : e.level === "error" ? "error" : "info", title, message: e.message });
      return;
    }
  }
}

// ── rewind ───────────────────────────────────────────────────────────────

/** How long the rewound rows take to fade out before they go. */
export const REWIND_FADE_MS = 200;
/** Sessions this device is rewinding: their `session_rewound` is applied by the caller, with the result. */
const localRewinds = new Set<string>();

/** This device is rewinding a session's history to `from`: fade out what comes after. */
export function beginLocalRewind(id: string, from: number) {
  localRewinds.add(id);
  if (state.sessions[id]) setState("sessions", id, "rewind", { from, local: true });
}

/** The rewind did not happen: everything stays. */
export function cancelLocalRewind(id: string) {
  localRewinds.delete(id);
  if (state.sessions[id]?.rewind) setState("sessions", id, "rewind", null);
}

/**
 * History after entry `toSeq` is gone: drop it here, then catch up with the
 * server (a harness note may already follow the cut). New entries reuse the
 * freed seqs; transcript items are keyed by seq *and* timestamp, so nothing
 * of the old ones can linger.
 */
export function applyRewind(id: string, toSeq: number) {
  localRewinds.delete(id);
  historyPages.delete(id);
  const s = state.sessions[id];
  if (!s) return;
  setState(
    "sessions",
    id,
    produce((d) => {
      const cut = d.entries.findIndex((e) => e.seq > toSeq);
      d.hasOlderHistory = false;
      if (cut >= 0) {
        for (const e of d.entries.slice(cut)) {
          if (e.kind === "tool_result") delete d.results[e.call_id];
          if (e.kind === "assistant") {
            delete d.thinkMs[e.seq];
            for (const p of e.content) if (p.type === "tool_call") delete d.toolMs[p.id];
          }
        }
        d.entries.splice(cut);
      }
      if (d.info.last_seq > toSeq) d.info.last_seq = toSeq;
      d.rewind = null;
    }),
  );
  // A rewind may cut before the installed page. Re-establish its older cursor,
  // rather than marking the retained (possibly empty) tail as complete.
  if (s.loaded) void refetchRewound(id);
}

async function refetchRewound(id: string) {
  // A load that started before the rewind would bring the old entries back:
  // after one, reload everything instead of just what follows the cut.
  const stale = inflight.get(id);
  if (stale) await stale.promise.catch(() => {});
  if (state.sessions[id]) await loadSession(id, false).catch(() => {});
}

// ── derived helpers ──────────────────────────────────────────────────────

export function allModels(): ModelInfo[] {
  return state.providers.flatMap((p) => p.models);
}

export function findModel(ref: ModelRef | null | undefined): ModelInfo | undefined {
  if (!ref) return undefined;
  const p = state.providers.find((x) => x.id === ref.provider);
  return p?.models.find((m) => m.id === ref.model);
}

/** A preset or a saved default whose account signed out is not ready. */
export function modelReady(ref: ModelRef | null | undefined): boolean {
  return !!ref && state.providers.some((p) => p.id === ref.provider && p.authenticated && p.models.some((m) => m.id === ref.model));
}

export function defaultModel(): ModelRef | null {
  if (modelReady(state.server?.default_model)) return state.server!.default_model!;
  const p = state.providers.find((x) => x.authenticated && x.models.length);
  return p ? { provider: p.id, model: p.models[0].id } : null;
}

export function modelLabel(ref: ModelRef | null | undefined): string {
  if (!ref) return "Default model";
  const name = findModel(ref)?.name ?? ref.model;
  // Several ChatGPT or Claude accounts: which one ("GPT-5.5 · Work").
  const tag = accountTag(ref.provider);
  return tag ? `${name} · ${tag}` : name;
}

/** A subscription account's short name, when there are several of its kind ("Work"; the usual one "ChatGPT"). */
export function accountTag(provider: string): string | null {
  const p = state.providers.find((x) => x.id === provider);
  if (!p || (p.kind !== "openai_codex" && p.kind !== "claude_code")) return null;
  if (state.providers.filter((x) => x.kind === p.kind).length < 2) return null;
  return accountShort(p);
}

/** "ChatGPT (Work)" → "Work"; "ChatGPT (subscription)" → "ChatGPT"; anything else as it is. */
export function accountShort(p: { name: string; kind: ProviderKind }): string {
  const m = /^(ChatGPT|Claude) \((.+)\)$/.exec(p.name.trim());
  if (!m) return p.name;
  return /subscription/i.test(m[2]) ? m[1] : m[2];
}

export const botById = (id: string | null | undefined): Bot | undefined => (id ? state.bots.find((b) => b.id === id) : undefined);
export const botByHandle = (h: string): Bot | undefined => {
  const k = h.replace(/^@/, "").toLowerCase();
  return state.bots.find((b) => b.handle.toLowerCase() === k);
};

/** A private bot-to-bot conversation (read-only for the user). */
export const isBotDm = (s: SessionInfo | undefined) => !!s && s.kind === "channel" && !!s.bot_id;
/** A room where bots (and the user) talk. */
export const isRoom = (s: SessionInfo | undefined) => !!s && s.kind === "channel" && !s.bot_id;

/** Boards of a chat that wait for the user's input (a signature, a choice…). */
export const pendingAsks = (sessionId: string | null | undefined): Whiteboard[] =>
  sessionId
    ? Object.values(state.boards).filter((b) => !!b.ask && (b.session_id === sessionId || state.sessions[b.session_id]?.info.parent_id === sessionId))
    : [];

export const isBusy = (s: SessionInfo | undefined) =>
  !!s && (s.status === "running" || s.status === "awaiting_approval" || s.status === "compacting");

// ── actions with optimistic UI ───────────────────────────────────────────

export async function sendMessage(sessionId: string, content: ContentPart[], delivery: Delivery, attachments: Attachment[] = []) {
  const s = state.sessions[sessionId];
  // While the agent works, a message waits in `queued` (shown as a chip)
  // until the server injects it; only an idle send appears right away.
  const optimistic = delivery !== "follow_up" && !isBusy(s?.info);
  const pid = uid("p");
  if (s && optimistic) {
    setState("sessions", sessionId, "pending", (l) => [...l, { id: pid, content, delivery, at: Date.now() }]);
  }
  try {
    await api.send(sessionId, content, delivery, attachments);
  } catch (e) {
    if (state.sessions[sessionId]) {
      setState("sessions", sessionId, "pending", (l) => l.filter((p) => p.id !== pid));
    }
    throw e;
  }
}
