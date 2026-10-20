//! Tessera wire protocol.
//!
//! Every client (CLI/TUI, desktop, Android) talks to the harness through these
//! types. In-process clients use them directly; remote clients exchange them as
//! JSON over a WebSocket (`ClientMessage` up, `ServerMessage` down).
//!
//! Timestamps are unix milliseconds. Sequence numbers are per-session and
//! strictly increasing, so a reconnecting client can ask for "everything after
//! seq N" and never miss or duplicate persisted history.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod completion;
pub use completion::{
    CHECK_AUTHOR, check_ack_parts, is_check_ack, is_protocol_body, is_protocol_entry,
    is_protocol_notification,
};
pub mod presentation;

pub const PROTOCOL_VERSION: u32 = 1;

/// Supported attachment payload, before base64/JSON encoding.
pub const MAX_MESSAGE_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_FILE_ATTACHMENT_BYTES: usize = MAX_MESSAGE_ATTACHMENT_BYTES / 2;
/// Base64 expansion plus 1 MiB for request fields and attachment metadata.
/// Shared by client admission and server plaintext/Noise reassembly.
pub const MAX_SERIALIZED_REQUEST_BYTES: usize =
    MAX_MESSAGE_ATTACHMENT_BYTES.div_ceil(3) * 4 + 1024 * 1024;
/// A maximum admitted request echoed as an entry, with response/session metadata.
pub const RESPONSE_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SERIALIZED_RESPONSE_BYTES: usize =
    MAX_SERIALIZED_REQUEST_BYTES + RESPONSE_ENVELOPE_BYTES;

macro_rules! wire {
    ($($item:item)*) => {$(
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
        $item
    )*};
}

wire! {
    /// `provider/model`, e.g. `openai-codex/gpt-5.5` or `local/qwen3.8-27b`.
    pub struct ModelRef {
        pub provider: String,
        pub model: String,
    }

    /// Reasoning effort. Each model advertises the subset it really supports
    /// (`ModelInfo::thinking`); `Auto` sends nothing and lets the provider or
    /// server decide (e.g. llama.cpp's `--reasoning-effort`).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, PartialOrd, Ord)]
    pub enum ThinkingLevel {
        Auto,
        Off,
        Minimal,
        Low,
        Medium,
        High,
        #[serde(rename = "xhigh")]
        XHigh,
        Max,
    }

    /// One reasoning mode a model supports, with its provider-native name.
    pub struct ThinkingOption {
        pub level: ThinkingLevel,
        /// e.g. "Extra high", "Thinking off".
        pub label: String,
        /// One line explaining the trade-off.
        pub hint: String,
    }

    /// How tool calls are authorised for a session.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum PermissionMode {
        /// Read-only and ordinary edits run; destructive or unusual actions ask.
        #[default]
        Default,
        /// "YOLO": nothing asks. Catastrophic commands are still refused.
        #[serde(alias = "yolo")]
        Auto,
        /// Every mutating action asks.
        Ask,
        /// Mutating tools are unavailable (planning / research).
        ReadOnly,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum SessionStatus { Idle, Running, AwaitingApproval, Compacting, Interrupted, Error }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum SessionKind {
        Chat,
        Job,
        Subagent,
        /// A room where bots (and the user) talk; also private bot-to-bot DMs.
        Channel,
    }

    /// Which runner owns a helper's lifecycle, independent of its model.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum HelperOwner { Tessera, ClaudeCode }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ContentPart {
        Text { text: String },
        /// Base64 image data.
        Image { mime: String, data: String },
    }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum AssistantPart {
        Text { text: String },
        Thinking {
            text: String,
            /// Opaque provider data that must be replayed verbatim (signatures,
            /// encrypted reasoning). Never shown to users.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
            opaque: Option<Value>,
        },
        ToolCall {
            id: String,
            name: String,
            #[cfg_attr(feature = "ts", ts(type = "unknown"))]
            arguments: Value,
        },
    }

    #[derive(Default)]
    pub struct Usage {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub input: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub output: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub cache_read: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub cache_write: u64,
        /// Estimated cost in US dollars, when the model's price is known
        /// (absent = unknown, so show nothing).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cost_usd: Option<f64>,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum StopReason { Stop, ToolUse, Length, Aborted, Error }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum NoticeLevel { Info, Warning, Error }

    /// One persisted item of a session's history.
    pub struct Entry {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub ts: u64,
        #[serde(flatten)]
        pub body: EntryBody,
    }

    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum EntryBody {
        User {
            content: Vec<ContentPart>,
            /// Who wrote it, when not the user: a bot id (channels, DMs),
            /// `chat:<session id>` (that chat's agent: a task for a subagent,
            /// a message to a bot), `job:<job id>` (a scheduled job's
            /// instructions), `subagent:<id>` (its report), `goal` or
            /// `harness`. Absent: the user (older servers: also messages a
            /// plain chat's agent sent to a bot, in bot DMs).
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            author: Option<String>,
        },
        Assistant {
            content: Vec<AssistantPart>,
            model: ModelRef,
            usage: Usage,
            stop: StopReason,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            error: Option<String>,
            /// Bot id of the speaker (bot chats and channels).
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            author: Option<String>,
        },
        ToolResult {
            call_id: String,
            tool: String,
            content: Vec<ContentPart>,
            is_error: bool,
            /// Full output id when the visible content was truncated.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            artifact: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
            details: Option<Value>,
        },
        /// Everything before `first_kept_seq` is represented by `summary`
        /// in the model context. The raw history stays in the store.
        Compaction {
            /// Externally reported boundary, not a Tessera-written summary.
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            external: bool,
            summary: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            first_kept_seq: u64,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            tokens_before: u64,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            tokens_after: u64,
        },
        Notice {
            level: NoticeLevel,
            text: String,
            /// For "learned from this conversation": what was saved.
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            #[cfg_attr(feature = "ts", ts(as = "Option<Vec<LearnedItem>>", optional))]
            learned: Vec<LearnedItem>,
        },
    }

    pub struct ContextStats {
        /// Tokens currently in context (reported or estimated; see source).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub tokens: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub window: u64,
        pub compactions: u32,
        /// `reported` = exact engine context; `estimated` = Tessera estimate.
        /// Never cumulative billed token usage. Older servers omit this.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub source: Option<String>,
    }

    pub struct SessionInfo {
        pub id: String,
        /// Monotonic whole-state revision; older servers omit this (0).
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<f64>", optional))]
        pub revision: u64,
        pub title: String,
        pub kind: SessionKind,
        pub cwd: String,
        pub model: ModelRef,
        pub thinking: ThinkingLevel,
        pub permission: PermissionMode,
        pub status: SessionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub parent_id: Option<String>,
        /// The bot this chat is with (bot chats, bot DMs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bot_id: Option<String>,
        /// Bot ids taking part (channels).
        #[serde(default)]
        pub members: Vec<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub last_seq: u64,
        pub usage: Usage,
        pub context: ContextStats,
        pub pinned: bool,
        pub archived: bool,
        /// Short plain-text preview of the latest message.
        pub preview: String,
        /// Structured task state (goal, plan, decisions...). Empty for quick chats.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub task: Option<TaskLedger>,
        /// Private chat: held only in the server's memory, never written to
        /// disk, no memory or skills, web search only; gone when deleted or
        /// when the server restarts.
        #[serde(default)]
        pub private: bool,
        /// Tool calls made so far (shown as progress for subagents).
        #[serde(default)]
        pub steps: u32,
        /// A harness helper (e.g. the background memory review), not a
        /// subagent the user started: clients never list it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub internal: bool,
        /// Fast mode: priority processing where the model offers it
        /// (`ModelInfo::fast`); costs more. Ignored elsewhere.
        #[serde(default)]
        pub fast: bool,
        /// Daybreak Blue for approved defensive work on supported OpenAI models.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub daybreak: bool,
        /// The agent may start subagents (the `agent` tool). Off: it works
        /// alone.
        #[serde(default = "crate::yes", skip_serializing_if = "crate::is_true")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub subagents: bool,
        /// This chat's own helper limits (`set_chat_helpers`); none: the
        /// server's defaults (`HelperSettings`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub helper_limits: Option<HelperLimits>,
        /// A helper waiting for its turn (its chat's limit, a model that
        /// answers one request at a time, or the server's room). Its status
        /// stays `running`, so older apps show it busy and can stop it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub helper_queued: bool,
        /// The parent's tool call that started this helper. Not its owner:
        /// native and bridged Tessera helpers can be continued normally.
        /// Older sessions with a call but no owner stay watch-only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub parent_call: Option<String>,
        /// Who can continue this helper. CLI-owned mirrors are watch-only;
        /// abort redirects to their owner's documented task-stop control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub helper_owner: Option<HelperOwner>,
        /// What a subagent is for: the agent definition it was started with,
        /// or Claude Code's subagent type (`Explore`, `Plan`, a custom agent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub role: Option<String>,
        /// A terminal chat: the lean coding tool set (read, write, edit,
        /// bash, job, grep, glob, task and the web tools) and a short
        /// prompt. Off (chats started in the apps): every tool.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub lean: bool,
        /// A bot's conversation doing work another chat handed it with
        /// `message_bot` in the background: that chat and its call. The bot's
        /// reply goes back there when it is done (then this is cleared);
        /// stopping that chat stops this work too.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub working_for: Option<WorkingFor>,
    }

    /// Whom a bot is working for (`SessionInfo::working_for`).
    pub struct WorkingFor {
        /// The chat that handed the work over.
        pub session_id: String,
        /// Its `message_bot` call.
        pub call_id: String,
        /// When the work was handed over (ms).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub since: u64,
        /// The bot conversation's last entry before the hand-off: its reply
        /// is what comes after.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub from_seq: u64,
    }

    /// Durable task state that lives outside the conversation and survives
    /// compaction verbatim. Maintained by the model through the `task` tool.
    #[derive(Default)]
    pub struct TaskLedger {
        pub goal: String,
        pub constraints: Vec<String>,
        pub plan: Vec<PlanItem>,
        pub decisions: Vec<String>,
        pub open_questions: Vec<String>,
        pub artifacts: Vec<String>,
        pub notes: Vec<String>,
        /// Evidence required before the task counts as done.
        pub done_criteria: Vec<String>,
        /// Verification state of each `done_criteria` item, by index.
        #[serde(default)]
        pub criteria: Vec<CriterionState>,
        /// Older/pruned items are losslessly stored as session-owned recall artifacts.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<std::collections::BTreeMap<String, LedgerArchive>>", optional))]
        pub archived: std::collections::BTreeMap<String, LedgerArchive>,
    }

    pub struct LedgerArchive {
        pub count: u64,
        /// Latest bundle; bundles link back to earlier ones.
        pub artifact: String,
    }

    /// Whether one "done when" criterion was checked, and the proof.
    pub struct CriterionState {
        pub status: CriterionStatus,
        pub evidence: String,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum CriterionStatus {
        #[default]
        Unchecked,
        Met,
        Unmet,
        Unverifiable,
    }

    pub struct PlanItem {
        pub text: String,
        pub status: PlanStatus,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum PlanStatus { Pending, InProgress, Done, Blocked, Skipped }

    /// A model response that is still streaming.
    pub struct StreamingMessage {
        pub content: Vec<AssistantPart>,
        pub model: ModelRef,
        /// Bot id of the speaker, when a bot is talking.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub author: Option<String>,
        /// Tool calls currently executing, with their live output.
        pub running_tools: Vec<RunningTool>,
    }

    pub struct RunningTool {
        pub call_id: String,
        pub name: String,
        pub output: String,
    }

    /// What a `rewind` did (or, for a dry run, would do). Paths are
    /// relative to the session's working directory when inside it.
    pub struct RewindResult {
        /// The point actually rewound to: moved back when `to_seq` fell
        /// between a tool call and its result.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub to_seq: u64,
        pub dry_run: bool,
        /// Files put back as they were at `to_seq`.
        pub restored: Vec<String>,
        /// Files created after `to_seq`, now removed.
        pub deleted: Vec<String>,
        /// Files left alone, and why.
        pub skipped: Vec<RewindSkip>,
        /// Shell commands run after `to_seq`: whatever they changed is not undone.
        pub not_undone: Vec<String>,
        /// Anything else worth telling the user (e.g. overwritten edits).
        pub notes: Vec<String>,
        /// History items removed from the session.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub removed_entries: u64,
        /// The rewound session.
        pub session: SessionInfo,
        /// The new session holding the full history from before the rewind
        /// (conversation rewinds; its `parent_id` is `session.id`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub branch: Option<SessionInfo>,
    }

    pub struct RewindSkip {
        pub path: String,
        pub reason: String,
    }

    pub struct ApprovalRequest {
        pub id: String,
        pub call_id: String,
        pub tool: String,
        /// One-line human description, e.g. `rm -rf build/`.
        pub summary: String,
        pub reason: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub arguments: Value,
        /// Asked every time (a machine that asks before every action, or
        /// before risky ones): no "allow for this session" (sent anyway,
        /// it counts once).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub once: bool,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum ApprovalDecision { Allow, AllowSession, Deny }

    /// Full state needed to render (or resynchronise) one session.
    pub struct SessionSnapshot {
        /// Changes when history is rewound, even after seq numbers are reused.
        /// Older servers omit it; older clients ignore it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<f64>", optional))]
        pub history_revision: u64,
        pub info: SessionInfo,
        pub entries: Vec<Entry>,
        /// Exclusive cursor for older display history. Absent on full snapshots
        /// and at the beginning of a paged history. Never a model-context cut.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub history_before: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub streaming: Option<StreamingMessage>,
        pub approvals: Vec<ApprovalRequest>,
        /// Messages the user queued while the agent was busy.
        pub queued: Vec<QueuedMessage>,
        /// The subagents this chat started, as they are now (subagents aren't
        /// in the session list: every device counts the same ones from here).
        /// Older servers: absent.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<SessionInfo>>", optional))]
        pub children: Vec<SessionInfo>,
    }

    /// A message waiting for a busy agent.
    pub struct QueuedMessage {
        /// What the user typed, in full (older servers: a one-line preview
        /// that included attachment notes).
        pub text: String,
        /// Read at the next step of the running turn (otherwise after it ends).
        pub steer: bool,
        /// Sent now: the running turn is stopping and this starts the next.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub interrupt: bool,
        /// Images and files attached to it (editing keeps them).
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
        pub attachments: u32,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum Delivery {
        /// Start a turn if idle; otherwise queue a follow-up.
        Auto,
        /// Inject into the running turn after the current tool batch.
        Steer,
        /// Run after the current turn finishes.
        FollowUp,
        /// Stop the running turn (its partial reply is kept, as with
        /// `abort`) and start the next with this message; other queued
        /// messages stay queued. Idle: like `auto`. A subagent is steered.
        Interrupt,
    }

    pub struct ModelInfo {
        pub provider: String,
        pub id: String,
        pub name: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub context_window: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub max_output: u64,
        pub reasoning: bool,
        pub vision: bool,
        /// The reasoning modes this model really supports (empty = none).
        #[serde(default)]
        pub thinking: Vec<ThinkingOption>,
        #[serde(default = "crate::auto_level")]
        pub default_thinking: ThinkingLevel,
        /// The level the user last picked for this model: new chats on it
        /// (and chats switched to it) start there.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub last_thinking: Option<ThinkingLevel>,
        /// Offers a fast tier (priority processing) for sessions in fast mode.
        #[serde(default)]
        pub fast: bool,
        /// Caller-specific cyber programs from OpenAI's live model catalog.
        /// Missing or empty metadata does not establish access.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
        pub cyber_access_programs: Vec<String>,
        /// The user's small model mode setting for this model.
        #[serde(default, skip_serializing_if = "SmallMode::is_auto")]
        #[cfg_attr(feature = "ts", ts(as = "Option<SmallMode>", optional))]
        pub small_mode: SmallMode,
        /// New chats on it start in small model mode (the setting, with
        /// `Auto` worked out for this model).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub small: bool,
        /// A model on the user's own server whose context window is too
        /// small for Tessera (or can't be told yet), in plain words with how
        /// to raise it. Absent: nothing to say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub context_warning: Option<String>,
    }

    /// Small model mode: simpler tools and shorter instructions. `Auto` turns
    /// it on for models on the user's own computer or network (unless known
    /// to be large) and for known small models; never for Claude Code.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum SmallMode {
        #[default]
        Auto,
        On,
        Off,
    }

    /// `ClaudeCode`: the user's own Claude Code CLI, run headless as a chat's
    /// engine (its Pro/Max plan); `base_url` is the `claude` command.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum ProviderKind { OpenaiChat, OpenaiResponses, OpenaiCodex, Anthropic, ClaudeCode }

    pub struct ProviderInfo {
        pub id: String,
        pub name: String,
        pub kind: ProviderKind,
        pub base_url: String,
        /// Credential present (API key or OAuth tokens).
        pub authenticated: bool,
        pub auth: AuthMethod,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub account: Option<String>,
        pub models: Vec<ModelInfo>,
        /// A model server on this computer or the local network that isn't
        /// answering (switched off, asleep): `models` are the ones it listed
        /// last. Cleared as soon as it answers again.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub offline: bool,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum AuthMethod { None, ApiKey, Oauth }

    /// Provider definition as supplied by a client when adding/editing one.
    pub struct ProviderSpec {
        pub id: String,
        pub name: String,
        pub kind: ProviderKind,
        pub base_url: String,
        /// Write-only. Omit to keep the stored key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub api_key: Option<String>,
        /// Discover models from the endpoint (`/models`).
        pub discover: bool,
        pub models: Vec<ModelInfo>,
    }

    pub struct OAuthStart {
        pub url: String,
        pub instructions: String,
        /// When set, the flow completes by itself (local callback server or
        /// device code); otherwise the client must call `complete_oauth`.
        pub auto_complete: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub user_code: Option<String>,
    }

    #[derive(Default)]
    pub struct Job {
        pub id: String,
        pub name: String,
        /// Cron expression (5 or 6 fields) or `@every 30m`.
        pub schedule: String,
        pub prompt: String,
        pub cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub model: Option<ModelRef>,
        pub enabled: bool,
        pub permission: PermissionMode,
        /// Run as this bot (a bot "duty").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bot_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub last_run: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub next_run: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub last_session: Option<String>,
        /// How the last run went: `running`, `ok`, `error`, `stopped` (by
        /// the user), `paused` (it stopped short and waits for a message) or
        /// `interrupted` (the server stopped and it didn't continue).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub last_status: Option<String>,
        /// When the last run ended (unix ms).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub last_end: Option<u64>,
        /// No run starts at or after this time (unix ms): an exclusive end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub until: Option<u64>,
        /// At most this many runs; then the schedule ends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub max_runs: Option<u32>,
        /// Runs started so far.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
        pub runs: u32,
        /// Why the schedule ended (its `until` passed, `max_runs` reached,
        /// or a run said its goal was met); it is off from then on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub ended: Option<String>,
    }

    /// One run of a scheduled job (`list_job_runs`), newest first.
    pub struct JobRun {
        pub job_id: String,
        /// The chat it ran in.
        pub session_id: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub started_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub ended_at: Option<u64>,
        /// As `Job::last_status`.
        pub outcome: String,
    }

    /// Which tools a bot may use.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum ToolPreset {
        /// Everything, including MCP tools.
        Full,
        /// Files, shell (here and on machines), search, web, subagents.
        Coding,
        /// Read-only files, web and history.
        Research,
        /// Web, notes, memory, scheduling and messaging other bots; no shell.
        Assistant,
        /// Read-only files only.
        ReadOnly,
        /// Exactly `allowed_tools`.
        Custom,
    }

    /// A named agent with its own persona, tools, permissions and memory.
    pub struct Bot {
        pub id: String,
        /// Display name, e.g. "Nova".
        pub name: String,
        /// Unique lowercase handle used for @mentions.
        pub handle: String,
        /// Emoji or 1–2 characters.
        pub avatar: String,
        /// Accent colour, `#rrggbb`.
        pub color: String,
        /// One line: what this bot is for.
        pub role: String,
        /// Voice and personality.
        pub personality: String,
        /// Standing instructions and responsibilities.
        pub instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub model: Option<ModelRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub thinking: Option<ThinkingLevel>,
        pub permission: PermissionMode,
        pub tools: ToolPreset,
        #[serde(default)]
        pub allowed_tools: Vec<String>,
        /// Home working directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cwd: Option<String>,
        /// Keep a separate MEMORY.md for this bot (separation of duties).
        #[serde(default = "crate::yes")]
        pub own_memory: bool,
        /// Let this bot read the shared user profile.
        #[serde(default = "crate::yes")]
        pub share_user_profile: bool,
        /// Bots this one may message privately (handles; "*" = any).
        #[serde(default)]
        pub can_message: Vec<String>,
        /// Other chats and bots may hand it work (`message_bot`); `false`:
        /// only the user talks to it. Absent: yes. A save that leaves it
        /// out keeps the stored one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub askable: Option<bool>,
        /// Notes this bot knows (ids): it sees their titles and previews and
        /// reads them with the `note` tool.
        #[serde(default)]
        pub knowledge: Vec<String>,
        /// Conversation starters the apps offer in a new chat (UI only;
        /// never part of the bot's instructions). At most 4.
        #[serde(default)]
        pub starters: Vec<String>,
        /// What it may do on each of the user's machines, never more than
        /// the machine itself allows. Absent: as each machine allows (bots
        /// made before this setting). A save that leaves it out keeps the
        /// stored one; a new bot saved without it gets no access.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub machines: Option<BotMachines>,
        /// Which saved passwords open to any chat it may use. Ones kept for
        /// certain bots follow their own list (`SecretInfo::bots`). Absent:
        /// all of them (bots made before this setting). A save that leaves
        /// it out keeps the stored one; a new bot saved without it gets none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub secrets: Option<BotSecrets>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
    }

    /// What a bot may do on one machine, within what that machine allows
    /// (`Bot::machines`). The server checks it before anything is sent.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum MachineAccess {
        /// It can't use the machine, nor open web views there.
        #[default]
        NoAccess,
        /// Read files and list folders the machine allows; no commands,
        /// changes or web views there.
        ReadOnly,
        /// Whatever the machine allows.
        Allowed,
    }

    /// A bot's access to the user's machines.
    #[derive(Default)]
    pub struct BotMachines {
        /// For machines not in `each`, including ones turned on later.
        #[serde(default)]
        pub default: MachineAccess,
        /// By machine id.
        #[serde(default)]
        pub each: std::collections::BTreeMap<String, MachineAccess>,
    }

    /// A bot's use of the saved passwords every chat may use (`Bot::secrets`).
    #[derive(Default, Eq)]
    pub struct BotSecrets {
        /// For passwords not in `each`, including ones saved later.
        #[serde(default)]
        pub default: bool,
        /// By the password's name.
        #[serde(default)]
        pub each: std::collections::BTreeMap<String, bool>,
    }

    pub struct SkillInfo {
        pub name: String,
        pub description: String,
        pub path: String,
        pub source: String,
        /// When its `SKILL.md` last changed (ms since the epoch; 0 unknown).
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum MemoryTarget { Memory, User }

    pub struct MemoryState {
        pub memory: String,
        pub user: String,
        pub memory_limit: u32,
        pub user_limit: u32,
    }

    pub struct DeviceInfo {
        pub id: String,
        pub name: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub last_seen: Option<u64>,
        pub current: bool,
        /// It gets agents' notifications (`notify`). On unless turned off.
        #[serde(default = "yes")]
        pub notify: bool,
        /// Connected to the server right now.
        #[serde(default)]
        pub online: bool,
        /// Devices on the same computer (its desktop app, its terminal, its
        /// machine) share this value, worked out by the server from what each
        /// reported (`device_hello`, `node_hello`); clients show them as one
        /// computer. Absent from older servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub computer: Option<String>,
        /// What it is, as it said or as its name suggests.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<DeviceClient>", optional))]
        pub client: DeviceClient,
        /// Its computer's own name (e.g. `DESKTOP-EXAMPLE`), as it reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub host: Option<String>,
        /// A phone's or tablet's model in plain words (e.g. "Galaxy Z Fold").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub model: Option<String>,
        /// The user chose this name (`rename_device`); nothing replaces it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub named: bool,
        /// A web browser ("Use Tessera in a web browser"): it connects only
        /// while that is on.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub browser: bool,
    }

    /// What a paired device is.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum DeviceClient {
        /// The desktop app.
        App,
        /// The phone or tablet app.
        Phone,
        /// The web app in a browser.
        Browser,
        /// The `tessera` command in a terminal.
        Terminal,
        /// Paired only as a machine (`tessera node enable <link>`).
        Machine,
        /// Not said, or something newer.
        #[default]
        #[serde(other)]
        Unknown,
    }

    /// A message for the user from an agent (`notify`) or a test from
    /// Settings. Devices that take notifications show it as a system
    /// notification; tapping it opens `session_id`.
    pub struct UserNotification {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub id: u64,
        pub title: String,
        pub body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub session_id: Option<String>,
        /// Who sent it: a bot's name or the chat's title (plain text).
        #[serde(default)]
        pub from: String,
        /// Shown like an approval (heads-up, sound) instead of a task update.
        #[serde(default)]
        pub urgent: bool,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        /// When it is (or was) delivered.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub due_at: u64,
        /// Delivered; false while it waits for `due_at`.
        pub sent: bool,
        /// Opened (or its chat was) on some device.
        pub seen: bool,
        /// What it is about: empty for an agent's message (`notify`);
        /// `finished` or `error` (long work in a chat), `job` (a scheduled
        /// task's run), `approval`, `helper`, `machine`, `system`. Clients
        /// treat values they don't know as a message.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<String>", optional))]
        pub kind: String,
        /// Kept for the notification center only: never shown as a system
        /// notification (a routine scheduled run, an approval answered…).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub quiet: bool,
        /// Private chat: OS consumers must redact title, body and sender,
        /// regardless of previews, kind or local session hydration.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub private: bool,
        /// Stable notification replacement key for a scheduled job across runs.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<String>", optional))]
        pub source_key: String,
    }

    /// What one device missed (`pending_notifications`).
    pub struct PendingNotifications {
        /// Sent in the last day, not seen, not yet shown here: show these.
        pub new: Vec<UserNotification>,
        /// Shown here earlier and since seen on some device: remove these.
        #[cfg_attr(feature = "ts", ts(type = "Array<number>"))]
        pub seen: Vec<u64>,
        /// Stable chat/job tags to withdraw after a read on another device.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
        pub withdraw: Vec<String>,
        /// When the next scheduled one is due, if any (to wake up then).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub next_due: Option<u64>,
        /// Chats waiting for an approval or an answer right now.
        #[serde(default)]
        pub waiting: Vec<WaitingChat>,
    }

    pub struct WaitingChat {
        pub session_id: String,
        pub title: String,
        pub private: bool,
    }

    /// A file the user can download from the chat: shared by an agent
    /// (`share_file`), downloaded in a web view, or attached by the user.
    pub struct SharedFile {
        pub id: String,
        pub name: String,
        pub mime: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub size: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub session_id: Option<String>,
        /// The web view it was downloaded in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub board_id: Option<String>,
        /// `agent`, `web_view` or `upload`.
        pub source: String,
        /// Where it is on the server (shown, never needed to download).
        pub path: String,
    }

    /// A build of the app the server offers a device (`get_app_update`):
    /// download it with `read_app_update`, check its SHA-256, then install.
    pub struct AppUpdate {
        pub id: String,
        /// `android-arm64`, `linux-x86_64-desktop`, `linux-x86_64-cli`…
        pub platform: String,
        pub version: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub size: u64,
        /// Lowercase hex.
        pub sha256: String,
        /// When the owner offered it (unix ms).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub added_at: u64,
        /// What changed, in the owner's words.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub notes: Option<String>,
    }

    /// Part of a shared file (`read_shared_file`), base64.
    pub struct FileChunk {
        pub data: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub offset: u64,
        /// The whole file's size.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub size: u64,
        pub eof: bool,
    }

    pub struct WebSearchInfo {
        pub search: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub searxng_url: Option<String>,
        pub header_names: Vec<String>,
        /// Search providers with a stored API key (`brave`, `tavily`).
        #[serde(default)]
        pub keys: Vec<String>,
    }

    pub struct ServerKeyInfo {
        /// Base64 X25519 public key.
        pub key: String,
        pub fingerprint: String,
    }

    pub struct RemoteAccessInfo {
        pub urls: Vec<String>,
        pub header_names: Vec<String>,
    }

    pub struct PairingCode {
        pub code: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub expires_at: u64,
        /// Fingerprint of the server's key, which the device shows after
        /// pairing (compare them to rule out a man in the middle).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub fingerprint: Option<String>,
        /// Everything a new device needs in one `tessera://pair?…` link, for a
        /// QR code: addresses, proxy headers, this code and the key fingerprint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub link: Option<String>,
    }

    pub struct DirEntry {
        pub name: String,
        pub path: String,
        pub is_dir: bool,
    }

    pub struct SearchHit {
        pub session_id: String,
        pub session_title: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: u64,
        pub snippet: String,
    }

    pub struct ServerInfo {
        pub name: String,
        pub version: String,
        pub protocol: u32,
        pub home: String,
        pub default_model: Option<ModelRef>,
        /// Speech-to-text is configured.
        #[serde(default)]
        pub voice: bool,
        /// Where speech is handled (`VoiceConfig::engine`). Absent from
        /// older servers: only `voice` (a transcription service) is there.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub voice_engine: Option<String>,
    }

    /// A stored secret. Values never leave the server and are never shown to
    /// models: they reach tools as environment variables and are scrubbed
    /// from tool output.
    pub struct SecretInfo {
        pub name: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
        #[serde(default)]
        pub kind: SecretKind,
        #[serde(default)]
        pub description: String,
        /// Names of the stored fields (values are never returned).
        #[serde(default)]
        pub fields: Vec<String>,
        /// Ask before the first command in a session that uses it.
        #[serde(default = "crate::yes")]
        pub ask: bool,
        /// Hosts it is meant for (shown in approvals; empty = any).
        #[serde(default)]
        pub hosts: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub last_used: Option<u64>,
        /// Who can use it: `None` any chat (secrets stored before scoping
        /// too), else only these bots (ids; empty: nobody). See [`bot_allowed`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bots: Option<Vec<String>>,
        /// Commands on this server (`bash`, the browser) may use it.
        #[serde(default = "crate::yes")]
        pub on_server: bool,
        /// Machine ids whose commands (`machines` run) may use it, when
        /// `machines_default` is false.
        #[serde(default)]
        pub on_machines: Vec<String>,
        /// Which machines may use it follows the setting for all saved
        /// passwords (`SecurityInfo::secret_machines`) instead of
        /// `on_machines`. Older servers don't send it (then: `on_machines`).
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub machines_default: bool,
        /// Kept for one chat only (its id): typed there while saving was
        /// "Ask me first" or "Don't save". Only that chat (and its
        /// subagents) can use it; it goes when the chat is deleted, or
        /// becomes an ordinary secret when the user saves it
        /// (`decide_chat_secret`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub chat: Option<String>,
    }

    /// What happens to a password or key the user types in a chat (it never
    /// reaches the model either way): `[security] secret_saving`.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum SecretSaving {
        /// Saved to Passwords & keys at once; agents save logins the user
        /// typed without asking.
        #[default]
        Auto,
        /// Kept for that chat only until the user picks Save or Don't save
        /// on the card in the chat; an agent's `save_secret` asks first.
        Ask,
        /// Kept for that chat only, never added to Passwords & keys.
        ChatOnly,
    }

    /// Where saved passwords and keys may be used on the user's machines,
    /// for those that don't choose for themselves (`[security]
    /// secret_machines`). Each machine still decides what it runs.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum SecretMachines {
        /// Nowhere but where a password itself allows a machine.
        #[default]
        Off,
        /// On the machines picked (`secret_machines_picked`).
        Picked,
        /// On every machine, also ones added later.
        All,
    }

    /// Whether ordinary chats (not a bot's) may hand work to bots with
    /// `message_bot` (`[security] bot_handoff`). Each bot can also refuse
    /// work from others (`Bot::askable`).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum BotHandoff {
        #[default]
        On,
        /// Every hand-off asks the user first, whatever the chat's mode.
        Ask,
        Off,
    }

    /// A suggested title for a saved password or key whose title says
    /// little (`SuggestSecretTitles`).
    pub struct SecretTitle {
        pub name: String,
        /// What it is titled now.
        pub title: String,
        pub suggested: String,
    }

    /// The user's answer about a secret kept for one chat.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum ChatSecretChoice {
        /// Move it to Passwords & keys (any chat, asking before first use).
        Save,
        /// Keep it for this chat only (the card stops asking).
        ChatOnly,
        /// Delete it now.
        Forget,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum SecretKind {
        /// One value: `$NAME`.
        #[default]
        Token,
        /// username / email / password / url / totp (+ custom fields):
        /// `$NAME_USERNAME`, `$NAME_PASSWORD`, … and `$NAME_OTP` (the current
        /// one-time code, when a TOTP seed is stored).
        Login,
        /// Any named fields: `$NAME_<FIELD>`.
        Custom,
    }

    /// How the agent's commands are confined (`[security] sandbox`).
    #[serde(rename_all = "lowercase")]
    #[derive(Copy, Eq, Default)]
    pub enum SandboxMode {
        /// Sandbox wherever the system supports it; otherwise run unconfined.
        #[default]
        Auto,
        /// Refuse to run commands without the complete sandbox.
        Strict,
        /// Never sandbox (commands can then read Tessera's credentials).
        Off,
    }

    /// A folder beyond a chat's own: the agent may change files there
    /// without asking (`write`), or only read it (`write: false`: file tools
    /// refuse changes and commands see it read-only).
    pub struct FolderAccess {
        pub path: String,
        pub write: bool,
    }

    /// Settings → Privacy & safety. Only the user changes these, from a paired
    /// device (`set_security`); agents have no tool or request for them.
    pub struct SecurityInfo {
        pub sandbox: SandboxMode,
        /// One line: "on (…)", "partial (…)" or "unavailable (…)".
        pub sandbox_status: String,
        /// Commands can't reach Tessera's data by any route the system leaves open.
        pub sandbox_complete: bool,
        /// What to do about a partial or missing sandbox (commands in backticks).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub sandbox_hint: Option<String>,
        /// Tessera's own data and settings folders: hidden from commands and
        /// refused to file tools, whatever the settings.
        pub protected: Vec<String>,
        pub folders: Vec<FolderAccess>,
        /// Ask before the agent writes files outside the chat's folder and `folders`.
        pub ask_outside: bool,
        /// Scrub credential-looking values from tool output before models see it.
        pub redact_patterns: bool,
        /// Network clients must connect end-to-end encrypted (config.toml only).
        pub require_e2ee: bool,
        /// Take API keys, logins and two-factor codes typed in a chat out of
        /// it before the model sees them (off: they stay in the chat).
        pub save_secrets_from_chat: bool,
        /// What happens to them then. Older servers don't send it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<SecretSaving>", optional))]
        pub secret_saving: SecretSaving,
        /// Where saved passwords may be used on machines by default. Older
        /// servers don't send it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<SecretMachines>", optional))]
        pub secret_machines: SecretMachines,
        /// The machines (ids) picked for `SecretMachines::Picked`.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
        pub secret_machines_picked: Vec<String>,
        /// Whether ordinary chats may hand work to bots. Older servers don't
        /// send it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<BotHandoff>", optional))]
        pub bot_handoff: BotHandoff,
        /// How much freedom new chats get: one value for all the user's
        /// devices. Left out while none of them has set it (each app keeps
        /// its own choice until then) and by older servers. A Windows server
        /// (no command sandbox) sends `ask` until then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub default_permission: Option<PermissionMode>,
    }

    /// A file attached to a message (base64).
    pub struct Attachment {
        pub name: String,
        pub mime: String,
        pub data: String,
    }

    /// Mosaic layout shared by every client (desktop, phone, terminal).
    pub struct MosaicLayout {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub version: u64,
        pub mosaics: Vec<Mosaic>,
    }

    pub struct Mosaic {
        pub id: String,
        pub name: String,
        /// Tiling mode (dwindle, columns, rows, grid, monocle…).
        #[serde(default)]
        pub layout: String,
        pub tiles: Vec<MosaicTile>,
        /// Client-specific geometry (e.g. the desktop split tree); opaque here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
        pub geometry: Option<Value>,
    }

    pub struct MosaicTile {
        pub id: String,
        /// `None` = an empty "new chat" tile.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub session_id: Option<String>,
        /// A whiteboard tile (instead of a chat).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub board_id: Option<String>,
        /// A note tile (instead of a chat).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub note_id: Option<String>,
    }

    /// Something an agent shows the user in a tile of its own: a visual
    /// explanation, a web page, an image, a remote desktop… Its content is
    /// untrusted and clients render it isolated from the app.
    pub struct Whiteboard {
        pub id: String,
        /// The chat that made it.
        pub session_id: String,
        pub title: String,
        pub content: BoardContent,
        /// Bumped on every change (clients reload the tile).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub version: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
        /// Input the agent is waiting for (a signature, a choice…).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub ask: Option<BoardAsk>,
        /// Shown by the user (`create_board`), not by the agent.
        #[serde(default)]
        pub by_user: bool,
        /// A web view's browser: the machine it runs on (its id), or none
        /// for this server's own. Set when the board is made and kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub host: Option<String>,
    }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum BoardContent {
        /// A self-contained page (may use the bundled `three` and `d3`).
        Html { html: String },
        Markdown { markdown: String },
        /// Base64 image data.
        Image { mime: String, data: String },
        /// A page on the web or the local network, e.g. a remote desktop.
        Url { url: String },
        /// A web app running on the Tessera server's machine (loopback
        /// only), shown through the encrypted connection.
        Local { port: u16, path: String },
    }

    pub struct BoardAsk {
        pub id: String,
        pub prompt: String,
        pub input: AskInput,
    }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum AskInput {
        Signature,
        Text {
            #[serde(default)]
            multiline: bool,
            #[serde(default)]
            placeholder: String,
        },
        Choice {
            options: Vec<String>,
            /// Several may be picked (answered with `choices`); a client from
            /// before this lets the user pick one.
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            multi: bool,
        },
        Confirm,
    }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum AskAnswer {
        /// Base64 PNG with a transparent background.
        Signature { png: String },
        Text { text: String },
        Choice { index: u32 },
        /// The options picked of a `multi` choice, in the order shown.
        Choices { indexes: Vec<u32> },
        Confirm { yes: bool },
        Cancel,
    }

    /// What a device saw of a board's tile (`board_snapshot`).
    pub struct SnapshotImage {
        pub mime: String,
        /// Base64.
        pub data: String,
        pub width: u32,
        pub height: u32,
    }

    /// A markdown document in the notes library, made by the user or an agent.
    pub struct Note {
        pub id: String,
        pub title: String,
        /// Markdown (rendered sanitised, never as raw HTML).
        pub content: String,
        /// Bumped on every change (`save_note`'s `base_version`).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub version: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
        /// The chat that made it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub session_id: Option<String>,
        #[serde(default)]
        pub by_user: bool,
        /// The latest change was an agent's (its `note` tool), not the user's.
        #[serde(default)]
        pub agent_edit: bool,
        /// The chat whose agent made that change (never a private one).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub edit_session: Option<String>,
        /// For older apps (their "All my bots" choice): always the opposite
        /// of `keep_from_agents` now. Older servers (before batch 17) meant
        /// by it that bots with their own memory may read the note too.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub all_bots: bool,
        /// The user keeps it from agents (`set_note_agents`): no agent or bot
        /// reads it, lists it or changes it, except where the user brought it
        /// into a chat themselves (a `[[link]]` or its id in their message)
        /// or gave it to a bot as knowledge. Off by default: every agent and
        /// bot may read every note. Servers before batch 17 don't send it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub keep_from_agents: bool,
    }

    /// A version of a note kept from before a change (`note_history`,
    /// `get_note_version`): one before every switch between the user and an
    /// agent, and one per stretch of editing.
    pub struct NoteVersion {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub version: u64,
        pub title: String,
        /// Left empty in `note_history`'s list.
        #[serde(default)]
        pub content: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
        /// Written by an agent (else by the user).
        #[serde(default)]
        pub agent_edit: bool,
        /// The chat whose agent wrote it (never a private one).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub edit_session: Option<String>,
    }

    /// A note without its content, for lists.
    pub struct NoteInfo {
        pub id: String,
        pub title: String,
        /// One line, at most 140 characters.
        pub preview: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub version: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub created_at: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub session_id: Option<String>,
        #[serde(default)]
        pub by_user: bool,
        /// As `Note::all_bots`.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub all_bots: bool,
        /// As `Note::keep_from_agents`.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub keep_from_agents: bool,
    }

    /// A picture in the notes library (`put_note_asset`). Notes refer to it
    /// as `![alt](assets/<name>)`; the name comes from its content.
    pub struct NoteAsset {
        /// `<16 hex>.<png|jpg|webp|gif>`.
        pub name: String,
        pub mime: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub size: u64,
        pub width: u32,
        pub height: u32,
    }

    /// A built-in user guide (`get_guide`), as markdown.
    pub struct Guide {
        pub name: String,
        pub title: String,
        pub markdown: String,
    }

    /// A device's offset from UTC from an instant on (`get_usage`): its
    /// days are counted in its own time zone, summer time included.
    pub struct UtcOffset {
        /// From this instant on (unix ms).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub since: u64,
        /// Minutes east of UTC (Chicago in summer: -300).
        pub minutes: i32,
    }

    /// Model requests over a period (`get_usage`).
    pub struct UsageReport {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub from: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub to: u64,
        pub total: UsageTotals,
        pub by_model: Vec<ModelUsage>,
        pub by_kind: Vec<KindUsage>,
        pub by_day: Vec<DayUsage>,
        /// `provider/model` of models used in the period whose price is
        /// unknown (their requests count in no cost; `set_model_price`).
        /// Local servers are never listed.
        #[serde(default)]
        pub unpriced: Vec<String>,
        /// Providers billed by a flat-fee plan, with what their requests in
        /// the period would have cost at API prices.
        #[serde(default)]
        pub subscriptions: Vec<SubscriptionInfo>,
    }

    #[derive(Default)]
    pub struct UsageTotals {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub requests: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub input: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub output: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub cache_read: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub cache_write: u64,
        /// Billed per token (API keys, or a price you set), estimated for
        /// requests whose price is known (absent: none). Subscriptions are
        /// never in it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cost_usd: Option<f64>,
        /// What the same requests cost, or would have cost, at pay-per-token
        /// API prices, subscriptions included (absent: no price known).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub api_cost_usd: Option<f64>,
    }

    pub struct ModelUsage {
        pub provider: String,
        pub model: String,
        pub totals: UsageTotals,
        /// How its provider is paid for (absent: the provider was removed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub billing: Option<Billing>,
        /// Its price now, when known (`by_model` only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub price: Option<Price>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub price_source: Option<PriceSource>,
    }

    /// US dollars per million tokens. Cache reads and writes cost the input
    /// price unless given.
    #[derive(Copy, Default)]
    pub struct Price {
        pub input: f64,
        pub output: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cache_read: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cache_write: Option<f64>,
    }

    /// Where a model's price comes from: set by the user (`set_model_price`
    /// or `config.toml`), Tessera's built-in list, or the public catalog.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum PriceSource { Custom, Builtin, Catalog }

    /// How a provider is paid for: per token with an API key, by a flat-fee
    /// plan (ChatGPT sign-in, the user's Claude Code as `claude-code`, or a
    /// plan set with `set_subscription`), or not at all (an OpenAI-compatible
    /// server on this machine or network, or one without a key).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum Billing { Api, Subscription, Local }

    /// A provider billed by plan, in a `UsageReport`.
    pub struct SubscriptionInfo {
        pub provider: String,
        /// The provider's name, e.g. "ChatGPT (subscription)".
        pub label: String,
        /// e.g. "ChatGPT Pro", as the user set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plan: Option<String>,
        /// The plan's monthly fee in US dollars, as the user set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub monthly_usd: Option<f64>,
        /// The fee prorated over the report's period (months of 30.44 days).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub period_usd: Option<f64>,
        /// Its requests in the period.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub requests: u64,
        /// What they would have cost at API prices (known prices only; see
        /// `UsageReport::unpriced`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub api_cost_usd: Option<f64>,
        /// The plan's usage limits as the provider last reported them with a
        /// reply (ChatGPT, Claude Code), 5-hour first, then weekly; empty
        /// until one came (absent from older servers).
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<PlanLimit>>", optional))]
        pub limits: Vec<PlanLimit>,
    }

    /// One of a plan's usage limits, as of `updated_at`.
    pub struct PlanLimit {
        /// `five_hour`, `weekly`, a model's own weekly limit (`weekly_opus`,
        /// `weekly_sonnet`…), `extra_usage`, or `<minutes>m` for other lengths.
        pub window: String,
        /// e.g. "5-hour limit", "Weekly limit, Opus".
        pub label: String,
        /// Share used, in percent (can pass 100); absent when only a status
        /// was given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub used_percent: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub status: Option<LimitStatus>,
        /// When the window starts over (unix ms).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub resets_at: Option<u64>,
        /// When the provider reported this (unix ms).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub updated_at: u64,
    }

    /// Where a limit stands: fine, close to it, or reached (requests wait
    /// until it resets).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum LimitStatus { Ok, Warning, Reached }

    /// `kind`: `chat`, `subagent`, `memory`, `title`, `compaction`, `job`
    /// or `other`.
    pub struct KindUsage {
        pub kind: String,
        pub totals: UsageTotals,
    }

    pub struct DayUsage {
        /// `YYYY-MM-DD` in the server's local time.
        pub day: String,
        pub totals: UsageTotals,
        pub models: Vec<ModelUsage>,
    }

    /// A proxied response for a `Local` board.
    pub struct BoardResponse {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        /// Base64.
        pub body: String,
    }

    /// Something the agent learned, as shown after a memory review.
    pub struct LearnedItem {
        pub kind: LearnedKind,
        pub action: LearnedAction,
        pub text: String,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum LearnedKind { Profile, Note, Skill }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum LearnedAction { Added, Updated, Removed }

    pub struct CloudflareZone {
        pub id: String,
        pub name: String,
        pub account_id: String,
        /// Whether Cloudflare lets this token use Access on the domain's
        /// account. `false` almost always means Zero Trust isn't turned on
        /// there yet (the setup would fail at its Access step); the token
        /// lacking the Access permissions looks the same. Absent from older
        /// servers, and when it couldn't be checked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub zero_trust: Option<bool>,
    }

    /// How devices can reach this server.
    pub struct RemoteStatus {
        pub lan_urls: Vec<String>,
        pub urls: Vec<String>,
        pub header_names: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub tunnel: Option<TunnelStatus>,
        /// "Use Tessera in a web browser" (servers before it: none).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub web: Option<WebAccess>,
    }

    /// "Use Tessera in a web browser": off by default. While on, the server
    /// gives the web app to browsers at `url` (over https only), and a
    /// browser paired with a code from one of the user's devices can use
    /// Tessera there, end-to-end encrypted. While off, nothing is served
    /// there and paired browsers can't connect.
    pub struct WebAccess {
        pub on: bool,
        /// This server has the web app to give (built into it, or `--web`).
        pub available: bool,
        /// Where to open it, e.g. `https://tessera.example.com/web/` (none
        /// without an https address).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub url: Option<String>,
        /// The Cloudflare tunnel Tessera set up lets only the apps through:
        /// turning this on asks Cloudflare once to let browsers reach
        /// `/web` too, which needs a Cloudflare token (`set_web_access`).
        #[serde(default)]
        pub needs_token: bool,
        /// Browsers paired so far (signed out in Devices).
        #[serde(default)]
        pub browsers: u32,
    }

    pub struct TunnelStatus {
        pub hostname: String,
        pub tunnel_id: String,
        /// cloudflared is running (Tessera supervises it).
        pub running: bool,
        /// It has a live connection to Cloudflare: the address works from
        /// anywhere.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub connected: bool,
        /// Why it isn't running or connected, in words for the owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub error: Option<String>,
        /// When the key the devices carry through Cloudflare Access (its
        /// service token) expires, unix seconds. Cloudflare's tokens last a
        /// year; `cloudflare_renew` adds another. Absent when the server
        /// doesn't know (set up before it kept the date) and on older
        /// servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub key_expires_at: Option<u64>,
        /// This server renews that key (`cloudflare_renew`) and reminds the
        /// user before it runs out. Absent (false) from older servers.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub renewable: bool,
    }

    /// How the user's own devices reach this server right now: the
    /// addresses and proxy headers a pairing code would carry (home
    /// addresses first, then the remote ones). Header values are
    /// credentials (Cloudflare Access): only the user's apps get this, over
    /// their end-to-end encrypted connection.
    pub struct ServerRoute {
        pub urls: Vec<String>,
        pub headers: std::collections::BTreeMap<String, String>,
    }

    /// The user's Claude Code CLI on the server, which chats can run as
    /// their engine (provider `claude-code`). Tessera never signs in to
    /// Claude itself: Claude Code does, once, when the user runs `claude`.
    pub struct ClaudeCodeInfo {
        pub installed: bool,
        /// Where `claude` was found.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub command: Option<String>,
        /// e.g. "2.1.284".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub version: Option<String>,
        pub signed_in: bool,
        /// The plan Claude Code reports, e.g. "max" or "pro".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plan: Option<String>,
        /// The account email Claude Code reports.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub account: Option<String>,
        /// Signed in with an API key: usage is billed per token, not the plan.
        #[serde(default)]
        pub api_key: bool,
        /// One plain sentence: what to do next.
        pub hint: String,
        /// Which Claude account (`claude_code` provider) this is about.
        /// Older servers have one only and don't say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub provider: Option<String>,
        /// What to run in a terminal on the server to sign this account in to
        /// Claude Code (another account: `claude` with its own
        /// `CLAUDE_CONFIG_DIR`). Absent for the usual account (`claude`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub sign_in: Option<String>,
    }

    /// Several subscriptions of one kind (`get_accounts`): whether a chat
    /// whose account reached its usage limit carries on with the same model
    /// on the next account of that kind (ChatGPT, Claude) that is signed in
    /// and hasn't reached its own. Off unless the user turns it on.
    #[derive(Default)]
    pub struct AccountSettings {
        #[serde(default)]
        pub continue_on_next: bool,
    }

    /// What is and isn't set up yet (drives the setup wizards).
    pub struct SetupStatus {
        /// Setup was completed (or skipped) once.
        pub completed: bool,
        /// A model is ready to use.
        pub has_model: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub default_model: Option<ModelRef>,
        pub web_search: String,
        pub voice: bool,
        pub remote: bool,
        pub devices: u32,
        pub sandbox: String,
        pub server_name: String,
        /// The server listens beyond this machine (LAN or tunnel reachable).
        pub lan: bool,
        /// Where it will listen from its next start (`server.bind`).
        pub bind: String,
        /// `bind` differs from where it listens now.
        pub restart_needed: bool,
        /// Plain (unencrypted) connections are refused from the network.
        pub e2ee_required: bool,
        /// Default permission mode for new chats.
        pub permission: PermissionMode,
    }

    /// Voice settings. `engine` says where speech is turned into text and
    /// back; `enabled`…`has_key` describe "another transcription service"
    /// (any OpenAI-compatible `/audio/transcriptions`), used by `service`.
    pub struct VoiceConfig {
        /// The other transcription service is set up.
        pub enabled: bool,
        /// `groq`, `openai`, `local` or `custom` (presets for the UI).
        pub provider: String,
        pub base_url: String,
        pub model: String,
        /// ISO-639-1 hint, e.g. "en" (empty = auto-detect).
        #[serde(default)]
        pub language: String,
        /// Whether an API key is stored (the key itself is write-only).
        #[serde(default)]
        pub has_key: bool,
        /// `auto` (the device's own private engine, else the server's),
        /// `device`, `server`, `openai` (the user's own key), `service` (the
        /// one above) or `off`. Empty from older servers: `service` when
        /// `enabled`, else off.
        #[serde(default)]
        pub engine: String,
        /// Recordings sent to the server are kept in its data folder.
        #[serde(default)]
        pub keep_audio: bool,
        /// The server's speech model: `auto` (by the processor), `tiny`, `base`.
        #[serde(default)]
        pub server_model: String,
        /// The server's voice (`VoiceChoice::id`); empty = by `language`.
        #[serde(default)]
        pub server_voice: String,
        /// The server's own engines (read-only; ignored by `save_voice`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub server: Option<VoiceServer>,
        /// An OpenAI API key is saved (Passwords & keys `OPENAI_API_KEY`, or
        /// the OpenAI model service's). Read-only.
        #[serde(default)]
        pub openai_key: bool,
        /// Recordings kept (`keep_audio`), and their total size. Read-only.
        #[serde(default)]
        pub kept: u32,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub kept_bytes: u64,
    }

    /// The server's own speech engines (whisper.cpp and Piper).
    pub struct VoiceServer {
        /// `ready`, `missing` (not downloaded yet), `installing`, `failed`,
        /// or `unsupported` (none made for this computer).
        pub state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub error: Option<String>,
        /// What is left to download for the chosen model and voice.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub download_bytes: u64,
        /// Download progress while `installing`.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub done_bytes: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub total_bytes: u64,
        /// The speech model in use (`tiny`, `base`).
        pub model: String,
        /// Speech to text works now.
        pub listen: bool,
        /// Text to speech works now (with `voice`).
        pub speak: bool,
        /// The voice in use.
        pub voice: String,
        /// Fast enough to show the words while the user is still speaking.
        pub live: bool,
        pub voices: Vec<VoiceChoice>,
    }

    /// A voice the server can speak with.
    pub struct VoiceChoice {
        /// e.g. `en_US-joe-medium`.
        pub id: String,
        /// e.g. "English (US), Joe".
        pub label: String,
        /// ISO-639-1, e.g. "en".
        pub language: String,
        pub installed: bool,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub size_bytes: u64,
    }

    pub struct Transcript {
        pub text: String,
        /// The engine that heard it (`server`, `openai`, `service`).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<String>", optional))]
        pub engine: String,
    }

    /// Speech made from text (`speak`): base64 audio.
    pub struct SpeechAudio {
        pub audio: String,
        /// `audio/wav` (the server's voice) or `audio/mpeg` (OpenAI).
        pub mime: String,
        /// Length in milliseconds (0 when unknown).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub ms: u64,
    }

    /// A model role: a named model used for one job, usable as `@name`.
    pub struct ModelRole {
        pub name: String,
        pub label: String,
        pub description: String,
        /// What it is set to (`provider/model` or `@other-role`); `None` =
        /// unset, so it falls back (see `description`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub value: Option<String>,
        /// The model it resolves to now, fallbacks included.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub resolved: Option<ModelRef>,
        /// One of Tessera's own roles (listed first), not a user-defined one.
        pub builtin: bool,
    }

    pub struct ModelRoles {
        pub roles: Vec<ModelRole>,
    }

    /// The live tab behind a web view board (`Url` or `Local`) in the
    /// server's browser. Every device attached to it sees the same page.
    pub struct BrowserTab {
        pub board_id: String,
        pub url: String,
        pub title: String,
        pub loading: bool,
        pub can_go_back: bool,
        pub can_go_forward: bool,
        /// Page zoom (1 = 100%).
        pub zoom: f64,
        /// `data:` URL of the site's icon, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub favicon: Option<String>,
        /// This is a window the page opened (a sign-in popup, a link that
        /// opens a new window), shown on top of it. `back` with no history
        /// left in it closes it and returns to the page, which keeps its
        /// connection to the popup (sign-ins complete as in a browser).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub popup: bool,
        /// The address of the page under the popup.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub opener: Option<String>,
        /// The page looks like a site's "prove you're a person" check (a
        /// verification challenge), which a web view can't always pass: the
        /// tile offers ways to finish it yourself. A hint only.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub check: bool,
        /// Pages in this web view's tab strip (saved boards; empty for a lone page).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
        pub tab_ids: Vec<String>,
        /// A file chooser opened by this page. Its opaque id expires on
        /// navigation; only bytes explicitly selected by the user are accepted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub file_chooser: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub file_multiple: bool,
    }

    /// Input for a browser tab. Coordinates are CSS pixels of the tab's
    /// viewport (the `width` × `height` of the frames). `modifiers` is a
    /// bit set: Alt 1, Ctrl 2, Meta 4, Shift 8.
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum BrowserInputEvent {
        Mouse {
            kind: BrowserMouseKind,
            x: f64,
            y: f64,
            #[serde(default)]
            button: BrowserMouseButton,
            /// Click count (2 = double click) for `down`/`up`.
            #[serde(default)]
            clicks: u32,
            /// Wheel deltas in CSS pixels (`wheel`; positive = down/right).
            #[serde(default)]
            dx: f64,
            #[serde(default)]
            dy: f64,
            #[serde(default)]
            modifiers: u32,
        },
        Touch {
            kind: BrowserTouchKind,
            /// Touches still on the screen (empty for `end`/`cancel` is fine).
            points: Vec<BrowserTouchPoint>,
            #[serde(default)]
            modifiers: u32,
        },
        Key {
            kind: BrowserKeyKind,
            /// DOM `KeyboardEvent.key`, e.g. `a`, `Enter`, `ArrowLeft`.
            key: String,
            /// DOM `KeyboardEvent.code`, e.g. `KeyA`, `Enter`.
            #[serde(default)]
            code: String,
            /// The text the key types (`down` only), if any.
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            text: Option<String>,
            #[serde(default)]
            modifiers: u32,
        },
        /// Insert text at the caret (typing, paste, an IME commit).
        Text { text: String },
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum BrowserMouseKind { Down, Up, Move, Wheel }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum BrowserMouseButton {
        #[default]
        None,
        Left,
        Middle,
        Right,
        Back,
        Forward,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum BrowserTouchKind { Start, Move, End, Cancel }

    pub struct BrowserTouchPoint {
        pub x: f64,
        pub y: f64,
        /// Stable while the finger stays down.
        #[serde(default)]
        pub id: u32,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum BrowserKeyKind { Down, Up }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum BrowserNavAction {
        /// Open an http(s) address (a bare host or words also work).
        Go { url: String },
        Back,
        Forward,
        Reload,
        Stop,
        /// Set the zoom (0.25–5; 1 = 100%).
        Zoom { factor: f64 },
    }

    pub struct BrowserUploadFile { pub name: String, pub size: u32 }

    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum BrowserUploadAction {
        Start { files: Vec<BrowserUploadFile> },
        Chunk { upload: String, index: u32, data: String },
        Finish { upload: String },
        Cancel { upload: String },
    }

    pub struct BrowserUploadResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub upload: Option<String>,
        pub received: u32,
    }

    /// Device-only page commands, never access to the host's clipboard.
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum BrowserPageAction {
        Context { x: f64, y: f64, #[serde(default)] select_word: bool },
        Selection,
        Find { text: String, #[serde(default)] backwards: bool },
        ClearFind,
        PrintPdf,
    }

    pub struct BrowserPageResult {
        #[serde(default)]
        pub text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub link: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub found: Option<bool>,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum BrowserDialogKind { Alert, Confirm, Prompt, Beforeunload }

    /// The server's browser (Settings).
    pub struct BrowserInfo {
        /// Web views may run in a browser on this server (`[browser]
        /// enabled`; off by default on a machine with little memory).
        #[serde(default = "crate::yes", skip_serializing_if = "crate::is_true")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub enabled: bool,
        /// This server's memory in bytes, when it is little enough that its
        /// own browser is off unless the user turns it on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub low_memory: Option<u64>,
        /// A Chromium-family browser is installed (or `[browser] path` exists).
        pub found: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub path: Option<String>,
        /// e.g. "Chromium 153".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
        pub running: bool,
        /// Without a window (`false`: windows on the server's desktop).
        pub headless: bool,
        /// Tabs open now.
        pub tabs: u32,
        /// How sharp the web view's moving pictures are (`[browser] quality`).
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<BrowserQuality>", optional))]
        pub quality: BrowserQuality,
        /// How fast this server draws pages, measured when its browser first
        /// started: sharp frames a second of a 1280×800 page at twice its
        /// resolution (60 is as fast as pages go; below ~30 is slow).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub speed: Option<f64>,
    }

    /// How sharp a web view is while its page moves. Still pages are always
    /// sent sharp (to devices that draw WebP).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum BrowserQuality {
        /// Each device as sharp as the server can draw smoothly (measured).
        #[default]
        Auto,
        /// At the device's full sharpness (up to `[browser] scale`), even
        /// if the server draws slowly.
        Sharp,
        /// At normal resolution while moving: the least work for the server.
        Smooth,
    }

    // ---- machines -----------------------------------------------------------
    // Another computer of the user's that runs `tessera node`: agents can use
    // it within what that computer allows (`MachineScope`). Turning it on and
    // widening what it allows happen only on that computer; other devices can
    // pause, resume and revoke it. See docs/MACHINES.md.

    /// Whether a machine's actions ask the user first (`MachineScope::ask`,
    /// "Ask first" in the apps). A chat whose own mode asks more still does.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum MachineAsk {
        /// Every action asks in the chat, whatever the chat's mode.
        #[default]
        Always,
        /// Only risky ones: commands the safety rules flag, and changes to
        /// startup files (with desktop control).
        Risky,
        /// Never: the chat's own mode decides (YOLO chats run without
        /// asking; read-only chats only read).
        Chat,
    }

    /// What a machine allows, as one named choice (worked out from its
    /// scope, so it can't disagree with it).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum MachinePreset {
        /// Read the chosen folders.
        LookOnly,
        /// Read and change the chosen folders, and run commands there.
        WorkInFolders,
        /// The home folder, read-write, and commands.
        FullAccess,
        /// Full access plus startup and window-manager settings and the
        /// graphical session.
        DesktopControl,
        /// Anything else ("Advanced": the user's own folder rules).
        #[default]
        Custom,
    }

    /// What a machine lets agents do. Set only on that computer (`tessera
    /// node scope`, or its desktop app); it enforces this itself.
    pub struct MachineScope {
        /// Folders agents may use; `write: false` = read-only. Empty (the
        /// default) = none.
        #[serde(default)]
        pub folders: Vec<FolderAccess>,
        /// Commands may run (off by default): in an allowed folder, changing
        /// files only in read-write ones.
        #[serde(default)]
        pub commands: bool,
        #[serde(default)]
        pub ask: MachineAsk,
        /// Longest one action may run, in seconds.
        #[serde(default = "crate::machine_timeout")]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub timeout_secs: u64,
        /// Desktop control (off by default; switchable only on that
        /// computer): startup files in read-write folders may change (each
        /// is backed up first) and commands get the graphical session.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub desktop: bool,
        /// Who can use it: `None` (the default) any chat; otherwise only
        /// these bots (ids or handles), including their subagents and
        /// scheduled duties.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bots: Option<Vec<String>>,
        /// "Web views here" (off by default; switchable only on that
        /// computer): chats and bots that may use the machine can open web
        /// views whose browser runs on it. Those pages can reach its local
        /// network and its localhost services.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub webviews: bool,
        /// "Terminal from my devices" (off by default; switchable only on
        /// that computer): the user's paired devices may open a terminal
        /// here. Agents never get one.
        #[serde(default, skip_serializing_if = "TerminalAccess::is_off")]
        #[cfg_attr(feature = "ts", ts(as = "Option<TerminalAccess>", optional))]
        pub terminal: TerminalAccess,
        /// "Administrator" (off by default; switchable only on that
        /// computer): agents may run commands as this computer's
        /// administrator (`run` with `admin`): outside the sandbox, with
        /// `sudo` working (the password saved on that computer, if it needs
        /// one). Needs commands on.
        #[serde(default, skip_serializing_if = "AdminAccess::is_off")]
        #[cfg_attr(feature = "ts", ts(as = "Option<AdminAccess>", optional))]
        pub admin: AdminAccess,
        /// Stopped by its user on that computer (its Stop bar, the Stop
        /// key, `tessera node stop`): it refuses every action until they
        /// resume it there. Nothing else can resume it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub stopped: bool,
        /// How many times it was stopped there: a chat's "allow for this
        /// chat" for its screen ends with each Stop.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
        pub stops: u32,
    }

    /// Whether agents may run administrator commands on a machine
    /// (`MachineScope::admin`).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum AdminAccess {
        /// Never (the default).
        #[default]
        Off,
        /// Each administrator command asks the user first, with the exact
        /// command, whatever the chat's mode.
        Ask,
        /// As the machine's "Ask first" choice and the chat's mode say: in a
        /// YOLO chat they run without asking.
        Chat,
    }

    /// Whether terminals may open, and how they are confined (the server's
    /// `[terminal] access`, or a machine's own `MachineScope::terminal`).
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum TerminalAccess {
        /// No terminals (the default).
        #[default]
        Off,
        /// In the command sandbox: Tessera's own data and the session's
        /// control sockets are out of reach.
        Sandboxed,
        /// With the account's full rights.
        Full,
    }

    /// `get_helpers`: the helper limits of chats that didn't choose their
    /// own (`HelperLimits`), and how many helpers the server runs at once.
    pub struct HelperSettings {
        /// Explicitly disabled is distinct from at_once = 0 (unlimited).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub enabled: Option<bool>,
        /// Helpers working at once in one chat (`[agent] max_subagents`);
        /// more wait their turn; 0 means no limit.
        pub at_once: u32,
        /// What this server would pick by itself (from its memory).
        pub default: u32,
        /// The largest number accepted for any helper limit.
        pub max: u32,
        /// Tool calls each helper may make before it must report
        /// (`[agent] subagent_steps`); 0 means no limit. Older servers leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub steps: Option<u32>,
        /// Minutes each helper may work before it must report
        /// (`[agent] subagent_minutes`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub minutes: Option<u32>,
        /// Helpers this server lets work at once across all chats (from its
        /// memory and processors, or `[agent] helper_capacity`); more wait,
        /// whatever the chats chose. A new one also waits while memory is low.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub capacity: Option<u32>,
    }

    /// One chat's own helper limits (`set_chat_helpers`). A field left out
    /// follows the server's default (`HelperSettings`); 0 means no limit.
    #[derive(Default, Eq)]
    pub struct HelperLimits {
        /// Helpers working at once; more wait their turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub at_once: Option<u32>,
        /// Tool calls each helper may make before it must report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub steps: Option<u32>,
        /// Minutes each helper may work before it must report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub minutes: Option<u32>,
    }

    /// The server's terminal settings (Settings → Privacy & safety → What commands can reach).
    pub struct TerminalSettings {
        pub access: TerminalAccess,
        /// A terminal closes after this many minutes without typing while
        /// nothing runs in it (15 to 240).
        pub idle_minutes: u32,
    }

    /// One open terminal (`terminal_open`, `terminal_attach`).
    pub struct TerminalInfo {
        pub id: String,
        /// The machine it runs on; none: this server.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub machine: Option<String>,
        /// Where, in words: the server's or the machine's name.
        pub place: String,
        /// Confined by the command sandbox.
        pub sandboxed: bool,
        /// The program, e.g. `bash`.
        pub shell: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub started_at: u64,
    }

    /// What is kept of one terminal: who, when, where. Never what was
    /// typed or shown.
    pub struct TerminalAudit {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub opened_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub closed_at: Option<u64>,
        /// The device's name.
        pub device: String,
        pub place: String,
        pub sandboxed: bool,
        /// How it ended, in words (empty while open).
        #[serde(default)]
        pub ended: String,
    }

    /// `terminal_status`: the settings, what this server can do, and this
    /// device's open terminals.
    pub struct TerminalStatus {
        pub settings: TerminalSettings,
        /// Terminals work on this server's system at all (not on Windows yet).
        pub supported: bool,
        /// The command sandbox can confine terminals here now.
        pub sandbox_ok: bool,
        /// The sandbox in one line (or why it can't be used).
        pub sandbox: String,
        /// Everything must run sandboxed (`sandbox = "strict"`): full access
        /// isn't offered.
        pub strict: bool,
        /// This connection may open terminals: end-to-end encrypted, or on
        /// the server itself.
        pub private: bool,
        pub max_open: u32,
        /// How long a terminal waits for its device to come back.
        pub grace_secs: u32,
        /// This device's open terminals.
        pub open: Vec<TerminalInfo>,
        /// The latest terminals on this server and its machines, newest first.
        pub recent: Vec<TerminalAudit>,
    }

    /// What the server tells a machine about its terminals.
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum NodeTermDown {
        /// Open one for `device` (its id and name, for the machine's log).
        Open {
            id: String,
            device: String,
            device_name: String,
            cols: u16,
            rows: u16,
        },
        Input {
            id: String,
            data: String,
        },
        Resize {
            id: String,
            cols: u16,
            rows: u16,
        },
        /// The device drew everything before this byte.
        Ack {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            upto: u64,
        },
        /// The device shows it again: the kept output comes once more.
        Attach {
            id: String,
        },
        /// No device shows it now (it waits for one to come back).
        Detach {
            id: String,
        },
        Close {
            id: String,
        },
        /// That device was signed out: close all of its terminals.
        SignedOut {
            device: String,
        },
    }

    /// What a machine tells the server about its terminals.
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum NodeTermUp {
        Opened {
            id: String,
            shell: String,
            sandboxed: bool,
        },
        /// It didn't open, in plain words.
        Failed {
            id: String,
            error: String,
        },
        /// Output (base64); `seq` is the offset of its first byte.
        Output {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            seq: u64,
            data: String,
        },
        Exit {
            id: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            code: Option<i32>,
            reason: String,
        },
    }

    /// A computer the user turned into a machine (`list_machines`).
    pub struct MachineInfo {
        /// The id of the paired device it connects as.
        pub id: String,
        pub name: String,
        /// e.g. `linux x86_64`.
        pub os: String,
        /// Its Tessera version.
        pub version: String,

        pub online: bool,
        /// When it was last connected (unix ms; now while online).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub last_seen: u64,
        /// Paused from a device: it refuses every action until resumed.
        pub paused: bool,
        pub scope: MachineScope,
        /// How it confines commands, one line (e.g. "on (namespaces + Landlock)").
        #[serde(default)]
        pub sandbox: String,
        /// Its sandbox can confine commands; without it commands are refused
        /// even when switched on.
        #[serde(default)]
        pub sandbox_ok: bool,
        /// Actions running there now.
        #[serde(default)]
        pub running: u32,
        /// What it allows, as one named choice.
        #[serde(default)]
        pub preset: MachinePreset,
        /// Its home folder (`~` in its paths), as it reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub home: Option<String>,
        /// Its latest actions as this server saw them, newest first.
        #[serde(default)]
        pub recent: Vec<MachineActivity>,
        /// Built backends (`NodeHello::features`), not permissions. Absent
        /// on older nodes; an explicit empty list means no backends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub features: Option<Vec<String>>,
    }

    /// One action on a machine, as the server saw it (Settings → Devices).
    pub struct MachineActivity {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub ts: u64,
        /// `read`, `list`, `write`, `edit` or `run`.
        pub action: String,
        /// The path or command (one line, shortened, scrubbed).
        pub target: String,
        /// The chat's title.
        pub chat: String,
        /// The bot that acted, by name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bot: Option<String>,
        /// `done`, `failed`, `refused`, `declined` or `stopped`.
        pub outcome: String,
    }

    /// The bot an action on a machine comes from (`NodeCall::bot`).
    pub struct NodeBot {
        pub id: String,
        pub handle: String,
        pub name: String,
    }

    /// One action on a machine. Paths are that machine's own (absolute or `~/…`).
    #[serde(tag = "action", rename_all = "snake_case")]
    pub enum NodeAction {
        Read {
            path: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            offset: Option<u64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            limit: Option<u64>,
        },
        /// A folder's entries.
        List { path: String },
        Write { path: String, content: String },
        /// Replace `old` (exactly once, or everywhere with `all`) with `new`.
        Edit {
            path: String,
            old: String,
            new: String,
            #[serde(default)]
            all: bool,
        },
        Run {
            command: String,
            cwd: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            timeout_secs: Option<u64>,
            /// As this computer's administrator: outside the sandbox, with
            /// `sudo` working. Only where the machine allows it
            /// (`MachineScope::admin`); older machines ignore it (and run
            /// the command in their sandbox, where `sudo` can't work).
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            admin: bool,
        },
        /// Seeing and using the screen of the signed-in desktop (desktop
        /// control; machines that report the `desktop` feature). `look`:
        /// a screenshot of the same screen after acting.
        Desktop {
            desktop: DesktopOp,
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            look: bool,
        },
    }

    /// One desktop action. `screen` counts from 1 (the main screen, the
    /// default); `x`/`y` are pixels of that screen's screenshot.
    #[serde(tag = "do", rename_all = "snake_case")]
    pub enum DesktopOp {
        /// The screens, their sizes and which is the main one.
        Screens,
        Screenshot {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            screen: Option<u32>,
        },
        Move {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            screen: Option<u32>,
            x: i32,
            y: i32,
        },
        /// `clicks`: 2 for a double click.
        Click {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            screen: Option<u32>,
            x: i32,
            y: i32,
            #[serde(default)]
            button: MouseButton,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            clicks: Option<u8>,
        },
        /// Press the left button at `x`,`y`, move to `to_x`,`to_y`, let go.
        Drag {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            screen: Option<u32>,
            x: i32,
            y: i32,
            to_x: i32,
            to_y: i32,
        },
        /// Wheel notches at `x`,`y`: `dy` > 0 scrolls down, `dx` > 0 right.
        Scroll {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            screen: Option<u32>,
            x: i32,
            y: i32,
            #[serde(default)]
            dx: i32,
            #[serde(default)]
            dy: i32,
        },
        /// Text typed as it is, into whatever has the keyboard focus.
        Type { text: String },
        /// Keys pressed together (`ctrl+c`), several combinations apart
        /// (`ctrl+a ctrl+c`).
        Key { keys: String },
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq, Default)]
    pub enum MouseButton {
        #[default]
        Left,
        Right,
        Middle,
    }

    /// A picture a machine sends back (a desktop screenshot).
    pub struct NodeImage {
        pub mime: String,
        /// Base64.
        pub data: String,
        pub width: u32,
        pub height: u32,
    }

    /// An action the server sends to one machine's own connection.
    pub struct NodeCall {
        pub call_id: String,
        pub action: NodeAction,
        /// The chat it comes from, and its title (for the machine's log).
        pub session_id: String,
        pub chat: String,
        /// The chat's permission mode: a machine never changes anything
        /// for a read-only chat.
        pub mode: PermissionMode,
        /// The user approved this action in the chat.
        pub approved: bool,
        /// The bot acting (a subagent's or scheduled duty's bot too); none
        /// for a plain chat. Machines that only some bots may use check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bot: Option<NodeBot>,
        /// For `run`: the stored secrets the command names that this chat
        /// may use on this machine, as environment variables. The machine
        /// scrubs them from what it sends back and never logs them.
        #[serde(default, skip_serializing_if = "SecretEnv::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<SecretEnv>", optional))]
        pub env: SecretEnv,
        /// The machine's `MachineScope.stops` when the turn asking for this
        /// was accepted (0 if it didn't know the machine then): a turn from
        /// before the user's latest Stop there is refused, never run after a
        /// Resume. Older servers send none, or the count when it was asked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub stops: Option<u32>,
        /// The bot acting may only read files and list folders here (its
        /// access to this machine is read-only): nothing else, looking at the
        /// screen included. Older servers send none.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub files_only: bool,
    }

    /// A machine's answer to a `node_call`.
    pub struct NodeResult {
        /// Text for the agent (already scrubbed of credential-looking values).
        pub output: String,
        pub ok: bool,
        /// The machine refused it (its scope, pause or state): nothing ran.
        #[serde(default)]
        pub refused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub exit_code: Option<i32>,
        /// A desktop screenshot (`NodeAction::Desktop`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub image: Option<NodeImage>,
    }

    /// What the server tells a machine's web view browser ("Web views
    /// here"). `run` numbers one start of it; anything for another run is
    /// ignored.
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum NodeBrowserDown {
        /// Start it. `scale`: one render scale for every tab (a browser
        /// without virtual screens).
        Start {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            scale: Option<f64>,
        },
        /// DevTools protocol commands, one JSON text each. The machine
        /// passes on only those it allows.
        Cdp {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
            messages: Vec<String>,
        },
        Stop {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
        },
        /// Show `url` in an ordinary, visible browser window on this
        /// computer's screen, with the web views' own profile (the same
        /// sign-ins), so the user can finish what a web view can't (a
        /// site's verification check) themselves. The browser for web
        /// views stops meanwhile. No `url`: close that window. Answered
        /// with `screen`.
        Screen {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            url: Option<String>,
        },
    }

    /// What a machine's web view browser tells the server.
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum NodeBrowserUp {
        Started {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
            /// e.g. `Chrome/153.0.8010.36`.
            product: String,
            /// What its measured drawing speed depends on (browser and CPU).
            speed_key: String,
            /// Tabs it keeps open at most.
            max_tabs: u32,
        },
        /// It couldn't start, in plain words.
        Failed {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
            error: String,
        },
        /// DevTools protocol replies and events, one JSON text each.
        Cdp {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
            messages: Vec<String>,
        },
        /// It stopped, and why (none: the server asked it to). `crashed`:
        /// by itself, unexpectedly (it may be started again); otherwise this
        /// machine stopped it (switched off, paused, idle, out of memory…).
        Stopped {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            run: u64,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            reason: Option<String>,
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            crashed: bool,
        },
        /// The window on this computer's screen (`screen`): open now, or
        /// closed (asked to, or by the user there), or why it couldn't open.
        Screen {
            shown: bool,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            error: Option<String>,
        },
    }

    /// The answer to `machine_login`: a token that can only serve as this
    /// device's machine (`node_hello` and the other `node_*` requests).
    pub struct MachineLoginGrant {
        pub device_id: String,
        pub token: String,
    }

    /// The server's answer to `node_hello`.
    pub struct NodeWelcome {
        pub machine_id: String,
        pub paused: bool,
        /// The server's display name.
        pub server: String,
    }

    /// `tessera node status --json`: this computer as a machine. Read
    /// locally (its desktop app); never sent to the server.
    pub struct NodeStatus {
        pub enabled: bool,
        /// Turned off from another device (`revoke_machine`); only turning it
        /// on here again brings it back.
        pub revoked: bool,
        pub name: String,
        /// This computer's Tessera version.
        pub version: String,
        /// The node's own operating system, for older-client fallback.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<String>", optional))]
        pub os: String,
        /// Built capabilities, also reported through `NodeHello::features`.
        /// `desktop` is native screen/input; `desktop_session` is Linux's
        /// command-based desktop control. Absent on older nodes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub features: Option<Vec<String>>,
        pub scope: MachineScope,
        /// It has a paired server to connect to.
        pub paired: bool,
        /// That server's name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub server: Option<String>,
        /// A `tessera node run` process is running (its pid).
        pub running: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub pid: Option<u32>,
        /// Connected to the server now, and the address in use.
        pub connected: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub endpoint: Option<String>,
        /// Paused from another device.
        pub paused: bool,
        /// Why it isn't connected, or what last went wrong.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub error: Option<String>,
        /// Actions running now.
        pub calls: u32,
        /// How commands are confined here, one line.
        pub sandbox: String,
        /// Commands can run here (the sandbox is complete).
        pub sandbox_ok: bool,
        pub service: NodeService,
        /// The local log (`tessera node log`).
        pub log_file: String,
        /// What it allows, as one named choice.
        #[serde(default)]
        pub preset: MachinePreset,
        /// This computer's home folder.
        #[serde(default)]
        pub home: String,
        /// The graphical session desktop control would hand to commands
        /// (e.g. "Hyprland on Wayland"), if one is running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub desktop_session: Option<String>,
        /// Commands with desktop control reach the session bus only through
        /// a filter (`xdg-dbus-proxy`), which is installed here.
        #[serde(default)]
        pub bus_filter: bool,
        /// A password for administrator commands is saved on this computer
        /// (`tessera node admin password`); never shown or sent anywhere.
        #[serde(default)]
        pub admin_password: bool,
        /// Startup files changed by agents, which can be put back.
        #[serde(default)]
        pub startup_changes: u32,
        /// The Stop bar while it runs (Windows, desktop control on): its
        /// Stop key, or why it couldn't start (desktop actions are refused).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub stop_bar: Option<String>,
        /// Tessera Guard (Windows; absent elsewhere).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub guard: Option<GuardStatus>,
    }

    /// Tessera Guard on a Windows computer: the user's Stop kept where the
    /// user's own programs can't undo it (resuming takes Windows'
    /// administrator prompt); desktop control and commands need it.
    pub struct GuardStatus {
        pub installed: bool,
        /// Its service runs and answers.
        pub running: bool,
        /// The Stop it keeps.
        pub stopped: bool,
        pub stops: u32,
        /// Why desktop control and commands are refused here, if they are
        /// (not installed, not answering, User Account Control too weak).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub blocked: Option<String>,
    }

    /// A startup file an agent changed on this machine (desktop control),
    /// kept so it can be put back (`tessera node restore --json`).
    pub struct StartupChange {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub id: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub ts: u64,
        pub path: String,
        /// It existed before (otherwise putting it back removes it).
        pub existed: bool,
        /// It exists now (otherwise it was deleted).
        pub exists: bool,
        /// `run`, `write`, `edit`, or `restore` (putting one back).
        pub action: String,
        /// The chat (or bot) whose action changed it.
        #[serde(default)]
        pub by: String,
        /// Its earlier content was too large to keep (it can't be put back).
        #[serde(default)]
        pub lost: bool,
    }

    /// The background service that keeps a machine on ("stay on").
    pub struct NodeService {
        /// Background services work here (Linux with systemd).
        pub available: bool,
        pub installed: bool,
        pub active: bool,
        /// Starts when the user logs in.
        pub enabled: bool,
        /// Also runs without a login (`loginctl enable-linger`).
        pub linger: bool,
    }

    /// What happened, in a machine's local log.
    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum NodeLogEvent {
        /// An action was allowed and started.
        Started,
        Done,
        Failed,
        /// Refused by this machine (scope, pause, state); nothing ran.
        Refused,
        Cancelled,
        TimedOut,
        Enabled,
        Disabled,
        ScopeChanged,
        Connected,
        Disconnected,
        Paused,
        Resumed,
        Revoked,
        /// An action changed startup files (desktop control); a copy of
        /// each was kept to put back.
        StartupChanged,
        /// A startup file was put back as it was before an agent changed it.
        Restored,
        /// One of the user's devices opened a terminal here.
        TerminalOpened,
        TerminalClosed,
    }

    /// One line of a machine's local log (`tessera node log --json`).
    pub struct NodeLogEntry {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub ts: u64,
        pub event: NodeLogEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub session_id: Option<String>,
        /// The chat's title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub chat: Option<String>,
        /// `read`, `list`, `write`, `edit` or `run`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub action: Option<String>,
        /// The path or command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub target: Option<String>,
        /// Why it was refused, the exit code, what changed…
        #[serde(default)]
        pub detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub duration_ms: Option<u64>,
        /// The bot that asked, by name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub bot: Option<String>,
    }

    /// A client → server call. Each variant's response type is documented.
    #[serde(tag = "method", content = "params", rename_all = "snake_case")]
    pub enum Request {
        /// → `ServerInfo`
        Hello { client: String, version: String },
        /// → `Vec<SessionInfo>`
        ListSessions { #[serde(default)] archived: bool },
        /// → `SessionInfo`
        CreateSession {
            #[serde(default)] cwd: Option<String>,
            #[serde(default)] model: Option<ModelRef>,
            #[serde(default)] title: Option<String>,
            /// Chat with this bot.
            #[serde(default)] bot_id: Option<String>,
            /// A private chat (see `SessionInfo::private`); `cwd` and `bot_id` are ignored.
            #[serde(default)] private: bool,
            /// A terminal chat with the lean coding tool set (see
            /// `SessionInfo::lean`); ignored for private and bot chats.
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            lean: bool,
        },
        /// → `SessionSnapshot` (entries after `since_seq` when given)
        GetSession {
            session_id: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            since_seq: Option<u64>,
        },
        /// → `SessionSnapshot`, newest bounded display page in ascending seq order.
        /// Opt-in. `GetSession` keeps its full-history/incremental behavior.
        GetSessionPage {
            session_id: String,
            /// Entries strictly before this seq, or the newest page when absent.
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            before_seq: Option<u64>,
            /// Default 120, clamped to 1–200, with a soft 512 KiB body budget.
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            limit: Option<u32>,
        },
        /// → `SessionInfo`
        UpdateSession {
            session_id: String,
            #[serde(default)] title: Option<String>,
            #[serde(default)] model: Option<ModelRef>,
            #[serde(default)] thinking: Option<ThinkingLevel>,
            #[serde(default)] permission: Option<PermissionMode>,
            #[serde(default)] pinned: Option<bool>,
            #[serde(default)] archived: Option<bool>,
            #[serde(default)] cwd: Option<String>,
            /// Channel members (bot ids).
            #[serde(default)] members: Option<Vec<String>>,
            /// Fast mode (see `SessionInfo::fast`).
            #[serde(default)] fast: Option<bool>,
            /// Allow subagents (see `SessionInfo::subagents`).
            #[serde(default)] subagents: Option<bool>,
        },
        /// → null
        DeleteSession { session_id: String },
        /// → `SessionInfo`. Set the session goal (kept verbatim through every
        /// compaction and told to the agent); empty clears it.
        SetGoal { session_id: String, goal: String },
        /// → `SessionInfo`. Opt this chat into Daybreak Blue. Enabling requires
        /// confirmed access for its current OpenAI model. Off by default.
        SetSessionDaybreak { session_id: String, enabled: bool },
        /// → null. Withdraw a queued message (index into `queued`).
        Dequeue { session_id: String, index: u32 },
        /// → null. Change the text of a queued message.
        EditQueued { session_id: String, index: u32, text: String },
        /// → null. Deliver a queued message now, as steering.
        SteerQueued { session_id: String, index: u32 },
        /// → null. Send a queued message now: as `send` with delivery
        /// `interrupt` (the running turn stops and the next starts with it).
        InterruptQueued { session_id: String, index: u32 },

        /// → `Vec<Whiteboard>` (all, or one chat's).
        ListBoards {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            session_id: Option<String>,
        },
        /// → `Whiteboard`
        GetBoard { id: String },
        /// → `Whiteboard`. Show something beside a chat yourself (a page, an
        /// image, notes…); its agent can look at it with `whiteboard read`.
        /// The server puts its tile beside the chat's, unless `placed` (the
        /// device already shows it in a tile of its own).
        CreateBoard {
            session_id: String,
            title: String,
            content: BoardContent,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            placed: bool,
        },
        /// → null. Removes it and its tiles.
        DeleteBoard { id: String },
        /// → null. Answer the board's pending `ask`.
        AnswerBoard { id: String, ask_id: String, answer: AskAnswer },
        /// → `BoardResponse`. A request to a `Local` board's web app.
        BoardFetch {
            id: String,
            method: String,
            path: String,
            #[serde(default)]
            headers: Vec<(String, String)>,
            /// Base64.
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            body: Option<String>,
        },
        /// → `{ "path": string }`: where this server serves the board as a
        /// page (for a browser on the server's own machine).
        BoardLink { id: String },
        /// → null. A device's answer to `board_snapshot_request`: what the
        /// board's tile shows, or why it can't say.
        BoardSnapshot {
            request_id: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            image: Option<SnapshotImage>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            error: Option<String>,
        },

        /// → `Vec<NoteInfo>`, newest first.
        ListNotes,
        /// → `Note`
        GetNote { id: String },
        /// → `Note`. No `id` creates one; a `base_version` that is no longer
        /// current is a `conflict` (fetch it and merge).
        SaveNote {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            id: Option<String>,
            title: String,
            content: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            base_version: Option<u64>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            session_id: Option<String>,
        },
        /// → null. Removes it and its tiles.
        DeleteNote { id: String },
        /// → `Note`. Older apps' "Only bots I add" / "All my bots": now the
        /// same as `set_note_agents` with `keep_from_agents: !all_bots`.
        SetNoteBots { id: String, all_bots: bool },
        /// → `Note`. Keep it from agents and bots (`Note::keep_from_agents`),
        /// or let them read it again (the default). Only the user sets this,
        /// from a device; agents can't.
        SetNoteAgents { id: String, keep_from_agents: bool },
        /// → `Vec<NoteVersion>`, newest first, without their content: the
        /// versions kept from before the current one.
        NoteHistory { id: String },
        /// → `NoteVersion` with its content (any kept version, or the current one).
        GetNoteVersion {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            version: u64,
        },
        /// → `{ path }`: `html` as a page served like an HTML whiteboard's (no
        /// network; the UI frames it sandboxed) for an hour. For previews of
        /// a note's HTML in the browser UI on the server's machine; the app
        /// serves its own.
        PreviewPage { html: String },
        /// → `NoteAsset`: keep a picture for notes (`mime` image/png, jpeg,
        /// webp or gif; `data` base64, at most 8 MB). The same picture twice
        /// is kept once. Unused ones are removed after a day.
        PutNoteAsset { mime: String, data: String },
        /// → `FileChunk`: up to `len` bytes (at most 1 MiB) of a note's
        /// picture from `offset`; `name` as in `assets/<name>`.
        ReadNoteAsset {
            name: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<f64>", optional))]
            offset: u64,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
            len: u32,
        },
        /// → `Vec<NoteInfo>`, newest first: the notes that link to this one
        /// (`[[its title]]`).
        NoteBacklinks { id: String },

        /// → `Guide`. A built-in user guide: `security`, `remote-access`,
        /// `claude-code` or `machines` (the docs/*.md the apps show in place).
        GetGuide { name: String },
        /// → `UsageReport` for `from`..`to` (unix ms; default: the last 30
        /// days up to now).
        GetUsage {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            from: Option<u64>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            to: Option<u64>,
            /// The asking device's time zone over the period, oldest first
            /// (the first entry stands for everything before it too): days
            /// (`by_day`) are the device's own. Empty (older clients): the
            /// server's time zone.
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            #[cfg_attr(feature = "ts", ts(as = "Option<Vec<UtcOffset>>", optional))]
            utc_offsets: Vec<UtcOffset>,
        },
        /// → null. The flat-fee plan a provider is billed by (e.g.
        /// `openai-codex`): its name and monthly fee in US dollars, shown
        /// against what its requests would cost at API prices. On a
        /// pay-per-token provider it marks it as billed by plan (a "coding
        /// plan" used with an API key). Both absent clears it.
        SetSubscription {
            provider: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            plan: Option<String>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            monthly_usd: Option<f64>,
        },
        /// → null. Your price for a model, used instead of the built-in and
        /// catalog ones (saved as `[prices]."provider/model"`); absent clears it.
        SetModelPrice {
            provider: String,
            model: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            price: Option<Price>,
        },
        /// → null. Small model mode for one model (saved as
        /// `[tuning."provider/model"] small`). Chats already open keep their
        /// mode until their next context checkpoint; new chats use it.
        SetModelSmall {
            provider: String,
            model: String,
            mode: SmallMode,
        },
        /// → `SessionInfo` (copy of history up to and including `at_seq`)
        ForkSession {
            session_id: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
            at_seq: Option<u64>,
        },
        /// → `RewindResult`. Undo work done after entry `to_seq` (the last
        /// entry kept; to undo a user message and everything after it, pass
        /// its seq − 1). `files`: put back every file the agent (or its
        /// subagents) changed with `write`/`edit` since then; files it
        /// created are deleted only if unchanged since it wrote them.
        /// Changes made by shell commands are not undone (they are listed in
        /// `not_undone`). `conversation`: drop the history after `to_seq`;
        /// the full history is kept as a new "(rewound)" session. `dry_run`
        /// reports what would happen and changes nothing. Refused while the
        /// session or its subagents are running.
        Rewind {
            session_id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            to_seq: u64,
            #[serde(default)]
            files: bool,
            #[serde(default)]
            conversation: bool,
            #[serde(default)]
            dry_run: bool,
        },
        /// → null
        Send {
            session_id: String,
            content: Vec<ContentPart>,
            #[serde(default = "default_delivery")] delivery: Delivery,
            /// Files of any kind. Saved where the agent can read them (text
            /// files are also inlined); private chats inline text only.
            #[serde(default)]
            attachments: Vec<Attachment>,
        },
        /// → null. Stops the running turn; queued messages are kept.
        Abort { session_id: String },
        /// → null
        Compact { session_id: String, #[serde(default)] instructions: Option<String> },
        /// → null
        RespondApproval { session_id: String, request_id: String, decision: ApprovalDecision },
        /// → `Vec<SearchHit>`
        Search { query: String, #[serde(default)] limit: Option<u32> },

        /// → `Vec<ProviderInfo>`
        ListProviders { #[serde(default)] refresh: bool },
        /// → `ProviderInfo`
        SaveProvider { spec: ProviderSpec },
        /// → null
        RemoveProvider { id: String },
        /// → `OAuthStart`
        StartOauth { provider: String },
        /// → `ProviderInfo`. `input` is the redirected URL or the bare code.
        CompleteOauth { provider: String, input: String },
        /// → null
        Logout { provider: String },
        /// → null
        SetDefaultModel { model: ModelRef },
        /// → `ModelRoles`
        GetModelRoles,
        /// → `ModelRoles`. `model` is `provider/model` or `@other-role`;
        /// `None` or empty clears the role.
        SetModelRole {
            role: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            model: Option<String>,
        },

        /// → `Vec<Bot>`
        ListBots,
        /// → `Bot`. An empty `id` creates a new bot.
        SaveBot { bot: Bot },
        /// → null
        DeleteBot { id: String },
        /// → `SessionInfo`. A room where the listed bots talk; the user's
        /// messages go to the @mentioned bots (or all when none is mentioned).
        CreateChannel { title: String, members: Vec<String>, #[serde(default)] cwd: Option<String> },

        /// → `Vec<SecretInfo>`
        ListSecrets,
        /// → null. `name` is an environment-variable name (A–Z, 0–9, _).
        SetSecret { name: String, value: String },
        /// → null
        DeleteSecret { name: String },
        /// → `SecretInfo`. Create or update a secret or login. `fields` values
        /// are write-only; an empty string keeps the stored value. `bots`,
        /// `on_server` and `on_machines` left out keep what is set (new: any
        /// chat, this server only). `new_name` renames it (values kept).
        SaveSecret {
            name: String,
            #[serde(default)] kind: SecretKind,
            #[serde(default)] description: String,
            #[serde(default)] fields: std::collections::BTreeMap<String, String>,
            #[serde(default)] remove_fields: Vec<String>,
            #[serde(default = "crate::yes")] ask: bool,
            #[serde(default)] hosts: Vec<String>,
            /// Left out: keep; `null`: any chat; a list: only these bots.
            #[serde(default, deserialize_with = "crate::present", skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            bots: Option<Option<Vec<String>>>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            on_server: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            on_machines: Option<Vec<String>>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            new_name: Option<String>,
            /// `true`: its machines follow the setting for all passwords;
            /// `false`: exactly `on_machines`. Left out: kept, except that a
            /// non-empty `on_machines` (an older app) means `false`.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            machines_default: Option<bool>,
        },
        /// → `SecretInfo`. Change only its title (the description); values,
        /// who and where stay as they are.
        SetSecretTitle { name: String, title: String },
        /// → `Vec<SecretTitle>`. Clearer titles for the saved passwords and
        /// keys whose titles say little ("saved from a chat"), made from
        /// their names, kinds and sites (no model involved).
        SuggestSecretTitles,
        /// → null. The user's answer about `name`, a secret kept for the
        /// chat `session_id` only (`SecretInfo::chat`); the chat's card about
        /// it changes to say what happened.
        DecideChatSecret { session_id: String, name: String, choice: ChatSecretChoice },
        /// → null. Secure app-only entry for a completed `save_secret` card.
        /// Values never become a message or model tool argument.
        SaveChatSecret {
            session_id: String,
            call_id: String,
            fields: SecretEnv,
            #[serde(default)]
            chat_only: bool,
        },

        /// → `SecurityInfo`
        GetSecurity,
        /// → `SecurityInfo`. Change what is given; `folders` replaces the list.
        SetSecurity {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            sandbox: Option<SandboxMode>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            folders: Option<Vec<FolderAccess>>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            ask_outside: Option<bool>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            redact_patterns: Option<bool>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            save_secrets_from_chat: Option<bool>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            secret_saving: Option<SecretSaving>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            secret_machines: Option<SecretMachines>,
            /// Replaces the list (ids of known machines).
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            secret_machines_picked: Option<Vec<String>>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            bot_handoff: Option<BotHandoff>,
            /// The freedom new chats get on every device (once set, it
            /// can be changed but not unset).
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            default_permission: Option<PermissionMode>,
        },

        /// → `MosaicLayout`
        GetMosaics,
        /// → `MosaicLayout`. `layout.version` must be the current version
        /// (else `conflict`: fetch and retry). Private tiles are not stored.
        SaveMosaics { layout: MosaicLayout },

        /// → `SetupStatus`
        SetupStatus,
        /// → null. Mark the setup wizard as done.
        CompleteSetup,
        /// → `Vec<ProviderInfo>`. Look for model servers again: on this
        /// machine and on its home network.
        DetectLocalModels,
        /// → `ClaudeCodeInfo`: whether the user's Claude Code (`claude`) is
        /// installed and signed in here (`refresh`: ask it again).
        ClaudeCodeStatus {
            #[serde(default)] refresh: bool,
            /// Which Claude account (default: the usual one, `claude-code`).
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            provider: Option<String>,
        },
        /// → `ProviderInfo`. Another subscription account: `kind` is
        /// `openai_codex` (then `start_oauth` with its id signs it in) or
        /// `claude_code` (signed in by the user through Claude Code itself,
        /// with its own configuration folder; see `ClaudeCodeInfo::sign_in`).
        /// `name` is the user's ("Work"). The server picks the id.
        AddAccount { kind: ProviderKind, name: String },
        /// → `ProviderInfo`. A provider's name, as the user calls it.
        RenameProvider { id: String, name: String },
        /// → `AccountSettings`.
        GetAccounts,
        /// → `AccountSettings`.
        SetAccounts { continue_on_next: bool },

        /// → `Vec<CloudflareZone>`. Checks the token and lists its domains.
        CloudflareZones { token: String },
        /// → `RemoteStatus`. Creates a tunnel, DNS record, Access app and
        /// service token, runs cloudflared, and records the address and
        /// headers for pairing links. Progress arrives as `setup_progress`.
        /// The API token is used once and never stored.
        CloudflareSetup { token: String, zone_id: String, hostname: String },
        /// → `RemoteStatus`. Undo `cloudflare_setup` (needs a token again).
        CloudflareRemove { token: String },
        /// → `RemoteStatus`. Renew the key the devices carry through
        /// Cloudflare Access (its service token lasts a year) with an API
        /// token that has Access: Service Tokens · Edit (used once, never
        /// stored); the devices keep the same key. `token` omitted: the user
        /// renewed it in Cloudflare's dashboard (Refresh adds a year), so
        /// its date moves a year on from now.
        CloudflareRenew {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            token: Option<String>,
        },
        /// → `RemoteStatus`
        RemoteStatus,

        /// → `ServerKeyInfo`: the server's public key, so a client already
        /// connected on the same machine can pin it and switch to encryption.
        ServerKey,
        /// → `WebSearchInfo`
        GetWebSearch,
        /// → `WebSearchInfo`. `search` is `auto`, `duckduckgo`, `searxng`,
        /// `brave` or `tavily` (the first provider tried; the others that are
        /// configured follow when it fails); `headers` (e.g. Cloudflare Access
        /// for a self-hosted SearXNG) omitted keeps the stored ones, `{}`
        /// clears them.
        SaveWebSearch {
            search: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            searxng_url: Option<String>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            headers: Option<std::collections::BTreeMap<String, String>>,
        },
        /// → `WebSearchInfo`. Store the API key of a search provider (`brave`
        /// or `tavily`) with the provider credentials; an empty `key` removes
        /// it. Keys are write-only and never reach a model.
        SetSearchKey { provider: String, #[serde(default)] key: String },
        /// → `RemoteAccessInfo`. Header values are never returned.
        GetRemoteAccess,
        /// → `ServerRoute`: the addresses and proxy headers this device
        /// should use, so a paired app keeps up when remote access is set
        /// up, changed or removed (`server_route_changed`). Only for the
        /// user's apps on an end-to-end encrypted connection (and machines
        /// connecting from another computer); refused to web browsers.
        GetServerRoute,
        /// → `RemoteAccessInfo`. Addresses devices should use from elsewhere
        /// and headers they must send (e.g. Cloudflare Access); `headers`
        /// omitted keeps the stored ones, `{}` clears them. `add_headers`
        /// and `remove_headers` change single headers and keep the rest.
        SetRemoteAccess {
            urls: Vec<String>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            headers: Option<std::collections::BTreeMap<String, String>>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            add_headers: Option<std::collections::BTreeMap<String, String>>,
            #[serde(default)]
            remove_headers: Vec<String>,
        },
        /// → `SetupStatus`. Where the server listens from its next start:
        /// `127.0.0.1:<port>` (this machine only) or `0.0.0.0:<port>` (LAN).
        SetServerBind { bind: String },
        /// → `RemoteStatus`. "Use Tessera in a web browser" on or off. When
        /// the Cloudflare tunnel Tessera made needs to let browsers through
        /// first (`WebAccess::needs_token`), `token` is a Cloudflare API
        /// token (used once, never stored). Off closes paired browsers'
        /// connections at once.
        SetWebAccess {
            on: bool,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            token: Option<String>,
        },

        /// → `VoiceConfig`
        GetVoice,
        /// → `VoiceConfig`. `api_key` is write-only; omit to keep the stored one.
        SaveVoice { config: VoiceConfig, #[serde(default)] api_key: Option<String> },
        /// → `Transcript`. `audio` is base64 (wav is best; webm/ogg/mp3/m4a).
        /// `engine`: `server`, `openai` or `service`, only as the voice setting
        /// allows (absent: what the setting picks). `partial`: the words so
        /// far while the user still speaks (never kept; refused while the
        /// server's engine is busy).
        Transcribe {
            audio: String,
            mime: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            engine: Option<String>,
            /// Client-selected id, scoped to this connection; `cancel_voice` drops the operation.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            operation: Option<String>,
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            partial: bool,
        },
        /// → `SpeechAudio`: `text` (at most 4,000 characters) spoken by the
        /// server's voice (`engine: server`) or OpenAI's (`openai`), as the
        /// voice setting allows.
        Speak {
            text: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            engine: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            operation: Option<String>,
        },
        /// → null. Cancel this connection's STT/TTS by operation id, without closing it.
        CancelVoice { operation: String },
        /// → `VoiceConfig`. Download the server's speech engines, model and
        /// voice (in the background; `VoiceConfig::server` shows progress).
        InstallVoice,
        /// → `VoiceConfig`. Delete the recordings kept by `keep_audio`.
        DeleteVoiceRecordings,

        /// → `Vec<Job>`
        ListJobs,
        /// → `Job`
        SaveJob { job: Job },
        /// → null
        DeleteJob { id: String },
        /// → `SessionInfo`
        RunJob { id: String },
        /// → `Vec<JobRun>`: the job's latest runs (at most 50), newest
        /// first. Older servers reject it.
        ListJobRuns { id: String },

        /// → `MemoryState`
        GetMemory,
        /// → `MemoryState`
        SetMemory { target: MemoryTarget, content: String },
        /// → `Vec<SkillInfo>`
        ListSkills { #[serde(default)] cwd: Option<String> },
        /// → `String` (the skill's `SKILL.md`, cut at 20,000 characters)
        ReadSkill { name: String, #[serde(default)] cwd: Option<String> },
        /// → null. Deletes the skill's folder from the server.
        DeleteSkill { name: String, #[serde(default)] cwd: Option<String> },

        /// → `Vec<DeviceInfo>`
        ListDevices,
        /// → null
        RevokeDevice { id: String },
        /// → `PairingCode`
        CreatePairing,
        /// → `MachineLoginGrant`. A login for this device's machine side (`tessera
        /// node` on the same computer): it can only serve as that machine,
        /// under this device's id, and replaces the one given before.
        MachineLogin,
        /// → `Vec<DeviceInfo>`. Whether a device gets agents' notifications.
        SetDeviceNotify { id: String, notify: bool },
        /// → `DeviceInfo` (this device as the server has it). What this
        /// device is and which computer it runs on, sent by newer clients
        /// once connected; fields left out stay as they were. It never
        /// changes the device's name.
        DeviceHello {
            #[serde(default)]
            client: DeviceClient,
            /// `tessera_secure::computer_key()`: the same for every Tessera
            /// program on one computer, and nothing else.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            computer: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            host: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            model: Option<String>,
        },
        /// → null. This device paired again with a server it already knew
        /// (its saved entry was replaced): sign out the login it had before,
        /// `token`, so Settings → Devices doesn't list it twice. A name the
        /// user gave the old entry, and whether it took notifications,
        /// carry over. Only an ordinary device's login is signed out this
        /// way (not a machine's or a web browser's); an unknown one is
        /// already gone. Sent over the new, verified connection only.
        RetireLogin { token: String },
        /// → `Vec<DeviceInfo>`. Rename a paired device, with the devices
        /// shown on the same computer. A machine's own name (what agents
        /// call it) stays: only that computer changes it.
        RenameDevice { id: String, name: String },

        /// → `Vec<UserNotification>`: scheduled ones, then the latest sent.
        ListNotifications,
        /// → `Vec<UserNotification>`: this device's notification center:
        /// scheduled reminders first, then up to 500 from the last 30 days,
        /// read ones included, quiet ones too. Independent of this device's
        /// system notification choice. Older servers reject it.
        ListNotificationInbox,
        /// → `UserNotification`. Send one to the devices that take them, now
        /// or `delay_secs` from now (Settings: "Send a test").
        SendNotification {
            title: String,
            body: String,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            session_id: Option<String>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<f64>", optional))]
            delay_secs: u64,
        },
        /// → `PendingNotifications` for this device (after reconnecting, or
        /// a phone checking in the background).
        PendingNotifications,
        /// → true. Opt this connection into typed automatic notifications.
        /// Without this handshake the legacy alert channel carries only explicit messages.
        NegotiateNotifications,
        /// → PendingNotifications including automatic activity. The caller
        /// understands kind/quiet and enforces notification preferences/previews.
        PendingNotificationsV2,
        /// → null. Foreground heartbeat, renewed every 15 seconds. An empty
        /// chat list still means the app is active. Missing heartbeats expire.
        NotificationPresence {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            active: bool,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
            session_ids: Vec<String>,
        },
        /// → null. Shown on this device (or deliberately not): not pending
        /// here any more.
        AckNotifications {
            #[cfg_attr(feature = "ts", ts(type = "Array<number>"))]
            ids: Vec<u64>,
        },
        /// → null. Seen: these, or every one about `session_id`. The other
        /// devices remove them (`notifications_seen`).
        SeenNotifications {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(as = "Option<Vec<f64>>", optional))]
            ids: Vec<u64>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            session_id: Option<String>,
        },
        /// → null. Cancel a scheduled notification (or forget a sent one).
        CancelNotification {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            id: u64,
        },
        /// → null. Forget these sent ones (the notification center's
        /// Clear; scheduled reminders stay: cancel those). Every device
        /// gets `notifications_seen` with them. Older servers reject it.
        ClearNotifications {
            #[cfg_attr(feature = "ts", ts(type = "Array<number>"))]
            ids: Vec<u64>,
        },
        /// → `Vec<SharedFile>`: a chat's downloadable files (with its
        /// subagents'), or a web view's downloads; newest first.
        ListSharedFiles {
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            session_id: Option<String>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            board_id: Option<String>,
        },
        /// → `FileChunk`: up to `len` bytes (at most 1 MiB) from `offset`.
        ReadSharedFile {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            offset: u64,
            len: u32,
        },
        /// → `AppUpdate` or null: the app build for `platform` this server
        /// offers, when it is newer than what the device runs: `sha256` of
        /// the device's own build and `installed_at` (unix ms) when it
        /// knows them. Older servers answer "unknown method": no updates.
        GetAppUpdate {
            platform: String,
            version: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            sha256: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(type = "number", optional))]
            installed_at: Option<u64>,
        },
        /// → `FileChunk`: up to `len` bytes (at most 1 MiB) of an offered
        /// build, from `offset`.
        ReadAppUpdate {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            offset: u64,
            len: u32,
        },
        /// → `Vec<DirEntry>` (directories first)
        ListDir { path: String },
        /// → `DirEntry`: "New folder" in the folder picker. Makes one folder
        /// called `name` inside the existing folder `parent` (a full path or
        /// `~/…`) and nothing more. `name` is a plain name: no `/` or `\`,
        /// not `.` or `..`, no control characters, at most 255 bytes. Refused
        /// (`bad_request`) inside Tessera's own data or settings, and
        /// (`conflict`) when anything of that name is already there.
        CreateDir { parent: String, name: String },
        /// → a memory report (JSON; the shape is the server's own, see
        /// `tessera doctor`): what the server process uses and what its
        /// buffers and caches hold.
        MemoryReport,

        /// → null. Show a web view's page in an ordinary, visible browser
        /// window on the computer its browser runs on (a machine with "Web
        /// views here"), with the same sign-ins, so the user can finish
        /// what a web view can't (a site's verification check) themselves;
        /// the tile waits meanwhile. `show: false` closes that window again
        /// and the tile carries on with what was done there. Only the
        /// user's devices ask this (never agents); the machine checks it
        /// too. A web view on the server can't: it has no screen.
        BrowserShowOnScreen {
            board_id: String,
            #[serde(default = "yes")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            show: bool,
        },
        /// → `BrowserTab`. Show a web view board (`Url`/`Local`): this
        /// connection starts receiving `browser_frame`s for it, sized for a
        /// `width` × `height` CSS-pixel tile at `dpr`. The latest attacher's
        /// viewport and device type (`mobile`: touch, phone layout) apply.
        /// Attach while the tile is visible; detach when it is hidden.
        BrowserAttach {
            board_id: String,
            width: u32,
            height: u32,
            dpr: f64,
            #[serde(default)]
            mobile: bool,
            /// The connection takes several frames at a time: it draws the
            /// newest and acknowledges it (which covers the ones it skipped).
            /// The server then keeps as many in flight as the connection
            /// carries without queueing; otherwise one at a time.
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
            pipelined: bool,
            /// Picture types it draws besides `image/jpeg`. With
            /// `image/webp`, a page that stops moving is sent once more as a
            /// sharp WebP at the full `dpr`.
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
            formats: Vec<String>,
        },
        /// → null. Stop frames for this board on this connection.
        BrowserDetach { board_id: String },
        /// → null
        BrowserInput { board_id: String, event: BrowserInputEvent },
        /// → `BrowserTab`
        BrowserNav { board_id: String, action: BrowserNavAction },
        /// → `BrowserPageResult`. User selection, context menu or find in page.
        BrowserPage { board_id: String, action: BrowserPageAction },
        /// → `Whiteboard`. Open another saved page in this web view's tab strip.
        BrowserNewTab { board_id: String, url: String },
        /// → `BrowserUploadResult`. Bounded chunks of files selected on this device.
        BrowserUpload { board_id: String, chooser: String, action: BrowserUploadAction },
        /// → null. Frame `frame` was shown (or skipped for a newer one),
        /// and so were the frames before it: more may be sent.
        BrowserAck {
            board_id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            frame: u64,
        },
        /// → `BrowserInfo`
        GetBrowser,
        /// → `BrowserInfo`. Saved to `[browser]`; takes effect when the
        /// browser next starts (an idle one is stopped right away).
        SetBrowser {
            /// Let web views run in a browser on this server (`[browser]
            /// enabled`); off stops an idle one.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            enabled: Option<bool>,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            headless: Option<bool>,
            /// Applies at once to open web views.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            quality: Option<BrowserQuality>,
        },
        /// → null. Supply only the selected file bytes (empty: cancel).
        BrowserChooseFiles {
            board_id: String,
            chooser: String,
            files: Vec<Attachment>,
        },
        /// → null. Answer the tab's open `browser_dialog`.
        BrowserDialogAnswer {
            board_id: String,
            accept: bool,
            #[serde(default)]
            #[cfg_attr(feature = "ts", ts(optional))]
            text: Option<String>,
        },
        /// → `Whiteboard`. Show a web view whose machine is offline (or no
        /// longer lets it run) in this server's browser instead: it opens
        /// its address here.
        BrowserToServer { board_id: String },
        /// → `Whiteboard`. Watch what the agent of `session_id` does in one
        /// of its own background tabs (`tab`, from a `browser` step's
        /// details): the tab becomes a web view the user opened, as it is,
        /// placed by the device. A tab that is already a web view gives its
        /// board.
        BrowserWatch { session_id: String, tab: String },

        /// → `Vec<MachineInfo>`: the computers turned into machines.
        ListMachines,
        /// → `MachineInfo`. Pause a machine (it refuses every action and
        /// stops running ones) or resume it. Resuming never goes beyond
        /// what the machine itself allows.
        SetMachinePaused { id: String, paused: bool },
        /// → null. Turn a machine off from here: running actions stop and it
        /// turns itself off; only that computer can turn it on again.
        RevokeMachine { id: String },
        /// → `NodeWelcome`. Node connections only: this connection is
        /// `tessera node` on a computer, offering what `scope` allows.
        /// `generation` grows each time it is turned on locally (a revoke
        /// holds until then).
        NodeHello {
            name: String,
            os: String,
            version: String,
            scope: MachineScope,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            generation: u64,
            sandbox: String,
            sandbox_ok: bool,

            /// Its home folder (how `~` reads in its paths).
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            home: Option<String>,
            /// Which computer it is (see `DeviceHello::computer`).
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            computer: Option<String>,
            /// That computer's own name, which its machine name may differ from.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            host: Option<String>,
            /// Built backends, not permissions: desktop (native screen/input),
            /// desktop_session (Linux commands), terminal, admin, sandbox, webviews.
            /// Older nodes leave it out; an empty list means no backends.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            features: Option<Vec<String>>,
            /// Tessera Guard's public key on that computer (Windows; hex):
            /// the server pins it, and asks the guard's signed word that the
            /// computer isn't stopped before every action there.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            guard_key: Option<String>,
        },
        /// → null. Node connections only: its scope (or name) changed on that computer.
        NodeScope {
            scope: MachineScope,
            /// Its name, renamed there (`tessera node name`). Older nodes leave it out.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            name: Option<String>,
        },
        /// → null. Node connections only: live output of a running action.
        NodeProgress {
            call_id: String,
            chunk: String,
            /// Pipe identity lets the server hold secret suffixes independently.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            stream: Option<String>,
        },
        /// → null. Node connections only: how a `node_call` ended.
        NodeReply { call_id: String, result: NodeResult },
        /// → null. Node connections only: Tessera Guard's signed word on
        /// the computer's Stop, for the server's `node_attest` (`nonce`).
        /// No signature: the guard couldn't be asked.
        NodeAttestation {
            nonce: String,
            stopped: bool,
            stops: u32,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            generation: u64,
            #[serde(default)]
            signature: String,
        },
        /// → null. Node connections only: its web view browser's news, in
        /// order (`seq` counts up from 1 per connection).
        NodeBrowser {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            seq: u64,
            msgs: Vec<NodeBrowserUp>,
        },
        /// → `HelperSettings`.
        GetHelpers,
        /// → `HelperSettings`. The defaults of chats that didn't choose
        /// their own: saved as `[agent] max_subagents` (and `subagent_steps`,
        /// `subagent_minutes` when given) and used at once: more helpers start
        /// now, fewer as working ones finish. All three accept 0 for no limit;
        /// steps and minutes are captured when a helper's run starts.
        SetHelpers {
            at_once: u32,
            /// Omitted by legacy clients: 0 keeps its legacy disabled meaning.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            enabled: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            steps: Option<u32>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            minutes: Option<u32>,
        },
        /// → `SessionInfo`. This chat's own helper limits, replacing what it
        /// had (fields left out follow the server's defaults; 0 is no limit).
        /// Concurrency changes at once; step/time budgets apply when runs start.
        SetChatHelpers { session_id: String, limits: HelperLimits },
        /// → `TerminalStatus`. The terminal settings and this device's open
        /// terminals. Paired devices only (never chats, tools or machines).
        TerminalStatus,
        /// → `TerminalStatus`.
        SetTerminalSettings { settings: TerminalSettings },
        /// → `TerminalInfo`. Open a terminal on this server (no `machine`) or
        /// on one of the user's machines that allows it. It belongs to this
        /// device; its output comes as `terminal_output` to this connection.
        TerminalOpen {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            machine: Option<String>,
            cols: u16,
            rows: u16,
        },
        /// → `TerminalInfo`. Show one of this device's open terminals again
        /// (after a reconnect or reload): the kept output comes first.
        TerminalAttach { id: String, cols: u16, rows: u16 },
        /// → null. Stop showing it here; it waits for this device to come
        /// back for a short while, then closes.
        TerminalDetach { id: String },
        /// → null. Keystrokes (UTF-8).
        TerminalInput { id: String, data: String },
        /// → null.
        TerminalResize { id: String, cols: u16, rows: u16 },
        /// → null. Everything before byte `upto` was drawn (flow control).
        TerminalAck {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            upto: u64,
        },
        /// → null. End it (its programs are stopped).
        TerminalClose { id: String },
        /// → null. Node connections only: its terminals' news, in order.
        NodeTerminal { msgs: Vec<NodeTermUp> },
    }

    /// Server → client push.
    #[serde(tag = "type", rename_all = "snake_case")]
    #[allow(clippy::large_enum_variant)]
    pub enum Event {
        /// Whole state including `session.revision`; ignore older revisions.
        SessionUpdated { session: SessionInfo },
        SessionRemoved { session_id: String },
        /// History after `to_seq` was removed (`rewind`): drop those entries
        /// and fetch the session again.
        SessionRewound {
            session_id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            to_seq: u64,
        },
        MosaicsChanged { layout: MosaicLayout },
        BoardUpdated { board: Whiteboard },
        BoardRemoved { id: String, session_id: String },
        /// An agent wants to see a board as the user does: a device showing
        /// its tile right now answers with `board_snapshot`.
        BoardSnapshotRequest { request_id: String, board_id: String },
        NoteUpdated { note: NoteInfo },
        NoteRemoved { id: String },
        /// Progress of a long setup task (e.g. `cloudflare`).
        SetupProgress {
            task: String,
            step: String,
            done: bool,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            error: Option<String>,
        },
        /// A persisted history item.
        EntryAdded { session_id: String, entry: Entry },
        /// Persisted items were rewritten in place (a secret was moved out of
        /// the conversation): replace the shown ones that have these `seq`s.
        EntriesChanged { session_id: String, entries: Vec<Entry> },
        StreamStarted {
            session_id: String,
            model: ModelRef,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            author: Option<String>,
        },
        StreamDelta { session_id: String, delta: StreamDelta },
        /// Streaming finished; the final message arrives as `EntryAdded`.
        StreamEnded { session_id: String },
        ToolStarted { session_id: String, call_id: String, name: String },
        ToolOutput { session_id: String, call_id: String, chunk: String },
        ApprovalRequested { session_id: String, request: ApprovalRequest },
        ApprovalResolved { session_id: String, request_id: String, decision: ApprovalDecision },
        QueueChanged { session_id: String, queued: Vec<QueuedMessage> },
        JobUpdated { job: Job },
        JobRemoved { id: String },
        BotUpdated { bot: Bot },
        BotRemoved { id: String },
        ProvidersChanged,
        /// The server dropped events for this connection (slow client);
        /// resynchronise open sessions with `get_session`.
        Resync,
        /// How devices reach this server changed (remote access set up,
        /// changed or removed): apps fetch `get_server_route` and update
        /// their saved server. Carries nothing itself.
        ServerRouteChanged,
        Notice { #[serde(default)] session_id: Option<String>, level: NoticeLevel, message: String },
        /// A picture of a web view tab. Only sent to connections attached to
        /// `board_id`, one at a time (more with `pipelined`): the next ones
        /// follow `browser_ack`.
        BrowserFrame {
            board_id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            frame: u64,
            /// `image/jpeg`, or `image/webp` (a sharp still picture) for
            /// connections that attached with that format.
            mime: String,
            /// Base64.
            data: String,
            /// The viewport the picture shows, in CSS pixels: map pointer
            /// positions onto this (the image itself may be larger, × dpr).
            width: u32,
            height: u32,
            /// The page's scroll position in the picture (CSS pixels), so a
            /// scroll can be drawn before the next frame shows it.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            scroll_x: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            scroll_y: Option<f64>,
        },
        /// A web view tab's address, title, history or zoom changed.
        BrowserState { tab: BrowserTab },
        /// Focus moved in a tab (attached connections): show the soft
        /// keyboard while `editable`. `input_type` is e.g. `text`, `email`,
        /// `password`, `number`, `search`, `url`, `tel`, `textarea`,
        /// `contenteditable` (empty when not editable).
        BrowserFocus { board_id: String, editable: bool, input_type: String },
        /// The page opened a dialog; answer with `browser_dialog_answer`.
        /// Re-sent to a connection that attaches while it is open.
        BrowserDialog {
            board_id: String,
            kind: BrowserDialogKind,
            message: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            default: Option<String>,
        },
        /// The tab's dialog was answered (here or elsewhere): hide it.
        BrowserDialogClosed { board_id: String },
        /// Machines were added, removed, came online, went offline, were
        /// paused or changed what they allow, or a device connected or went
        /// away: fetch `list_machines` (and `list_devices`).
        MachinesChanged,
        /// A notification for the user, sent only to the devices that take
        /// them: show it when `sent`; a scheduled one (not `sent` yet) tells
        /// a phone when to wake up.
        Notification { notification: UserNotification },
        /// Seen on some device: remove them.
        NotificationsSeen {
            #[cfg_attr(feature = "ts", ts(type = "Array<number>"))]
            ids: Vec<u64>,
        },
        /// A file became downloadable (shared by an agent, downloaded in a
        /// web view).
        FileShared { file: SharedFile },
        /// Node connections only: run this action.
        NodeCall { call: NodeCall },
        /// Node connections only: stop this action now.
        NodeCancel { call_id: String },
        /// Node connections only (machines with Tessera Guard): answer with
        /// the guard's signed word on the computer's Stop for `nonce`
        /// (`node_attestation`).
        NodeAttest { nonce: String },
        /// Node connections only: an action the server refused, or the
        /// user declined, before it reached the machine. For the machine's
        /// own log; nothing runs and nothing is answered.
        NodeRefused { call: NodeCall, reason: String },
        /// Node connections only: paused or resumed from a device, or
        /// revoked (turn off and disconnect).
        NodeControl { paused: bool, revoked: bool },
        /// Node connections only: for its web view browser, in order.
        NodeBrowser { msgs: Vec<NodeBrowserDown> },
        /// Only to the connection showing that terminal: output (base64);
        /// `seq` is the offset of its first byte (acknowledge with
        /// `terminal_ack`).
        TerminalOutput {
            id: String,
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            seq: u64,
            data: String,
        },
        /// That terminal ended (its program exited, or it was closed), and how.
        TerminalExit {
            id: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[cfg_attr(feature = "ts", ts(optional))]
            code: Option<i32>,
            reason: String,
        },
        /// Node connections only: for its terminals, in order.
        NodeTerminal { msgs: Vec<NodeTermDown> },
    }

    #[serde(tag = "part", rename_all = "snake_case")]
    pub enum StreamDelta {
        Text { index: u32, text: String },
        Thinking { index: u32, text: String },
        ToolCall { index: u32, id: String, name: String, arguments: String },
    }

    pub struct ApiError {
        pub code: ErrorCode,
        pub message: String,
    }

    #[serde(rename_all = "snake_case")]
    #[derive(Copy, Eq)]
    pub enum ErrorCode { BadRequest, Unauthorized, NotFound, Conflict, Busy, Provider, Internal }

    /// Envelope sent by clients over the WebSocket.
    pub struct ClientMessage {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub id: u64,
        #[serde(flatten)]
        pub request: Request,
    }

    /// Envelope sent by the server over the WebSocket.
    #[serde(untagged)]
    #[allow(clippy::large_enum_variant)]
    pub enum ServerMessage {
        Response {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            id: u64,
            #[cfg_attr(feature = "ts", ts(type = "unknown"))]
            result: Value,
        },
        Failure {
            #[cfg_attr(feature = "ts", ts(type = "number"))]
            id: u64,
            error: ApiError,
        },
        Event { event: Event },
    }

    /// `POST /api/pair` body.
    pub struct PairRequest { pub code: String, pub device_name: String }

    /// `POST /api/pair` response.
    pub struct PairResponse { pub token: String, pub device_id: String, pub server: ServerInfo }
}

/// End-to-end encrypted pairing and sessions (native clients). These travel
/// as JSON over HTTPS/WebSocket but their secrets are sealed inside; see
/// `docs/PROTOCOL.md` → "End-to-end encryption".
pub mod e2e {
    use serde::{Deserialize, Serialize};

    /// `POST /api/pair/start`: the device's SPAKE2 message (base64).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PairStart {
        pub pake: String,
    }

    /// Reply to [`PairStart`]: an attempt id and the server's SPAKE2 message.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PairStarted {
        pub pairing: String,
        pub pake: String,
    }

    /// `POST /api/pair/finish`: a sealed [`PairHello`].
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PairFinish {
        pub pairing: String,
        pub sealed: String,
    }

    /// Reply to [`PairFinish`]: a sealed [`PairGrant`].
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PairFinished {
        pub sealed: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PairHello {
        pub device_name: String,
        /// Pair as a machine (`tessera node enable <link>`): the login it gets
        /// can only serve as that machine, never act as one of the user's
        /// devices. Servers before this field make an ordinary device.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub machine: bool,
        /// A web browser's own key (base64, an uncompressed P-256 point),
        /// made in the browser so it can't be read out. Browsers pair
        /// through `/web/api/pair/*` ("Use Tessera in a web browser") and
        /// must then prove they hold it each time they connect
        /// ([`SessionAuth::proof`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub device_key: Option<String>,
    }

    /// What a device receives when pairing succeeds.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PairGrant {
        pub token: String,
        pub device_id: String,
        /// The server's long-term public key (base64), pinned by the device.
        pub server_key: String,
        pub server: crate::ServerInfo,
        /// The token is a machine login (see [`PairHello::machine`]).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub machine: bool,
    }

    /// First message of an encrypted session, from the device.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SessionAuth {
        pub token: String,
        /// Web browsers only: their key's signature (base64, P-256 ECDSA
        /// with SHA-256, `r || s`) over [`BROWSER_PROOF`] followed by the
        /// session's Noise handshake hash, so a login works only together
        /// with the key that never leaves the browser, and only for this
        /// session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub proof: Option<String>,
    }

    /// What a browser's [`SessionAuth::proof`] signs, before the handshake hash.
    pub const BROWSER_PROOF: &[u8] = b"tessera browser login v1";

    /// The server's answer to [`SessionAuth`].
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SessionAuthReply {
        pub ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub error: Option<crate::ApiError>,
    }
}

/// A `tessera://pair?…` link (shown as a QR code) with everything a device
/// needs to connect. It carries the proxy headers and a live pairing code,
/// so it is only ever displayed to the owner.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PairLink {
    pub name: String,
    pub urls: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub code: String,
    /// Expected server key fingerprint; pairing fails if it doesn't match.
    pub fingerprint: Option<String>,
}

impl PairLink {
    pub fn to_link(&self) -> String {
        let mut q = vec![
            ("v".to_string(), "1".to_string()),
            ("n".into(), self.name.clone()),
        ];
        q.extend(self.urls.iter().map(|u| ("u".to_string(), u.clone())));
        q.extend(
            self.headers
                .iter()
                .map(|(k, v)| ("h".to_string(), format!("{k}:{v}"))),
        );
        q.push(("c".into(), self.code.clone()));
        if let Some(f) = &self.fingerprint {
            q.push(("f".into(), f.replace(' ', "")));
        }
        let query: Vec<String> = q
            .iter()
            .map(|(k, v)| format!("{k}={}", pct_encode(v)))
            .collect();
        format!("tessera://pair?{}", query.join("&"))
    }

    pub fn parse(link: &str) -> Result<Self, String> {
        let query = link
            .trim()
            .strip_prefix("tessera://pair?")
            .ok_or("not a Tessera pairing link")?;
        let mut out = PairLink::default();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = pct_decode(v).ok_or("malformed pairing link")?;
            match k {
                "v" if v != "1" => return Err("this pairing link needs a newer Tessera app".into()),
                "n" => out.name = v,
                "u" => out.urls.push(v),
                "h" => {
                    let (name, value) = v
                        .split_once(':')
                        .ok_or("malformed header in pairing link")?;
                    out.headers
                        .push((name.trim().to_string(), value.trim().to_string()));
                }
                "c" => out.code = v,
                "f" => {
                    // Stored without spaces; shown in groups of four.
                    let hex: String = v.chars().filter(char::is_ascii_hexdigit).collect();
                    let grouped: Vec<String> = hex
                        .as_bytes()
                        .chunks(4)
                        .map(|c| String::from_utf8_lossy(c).into_owned())
                        .collect();
                    out.fingerprint = Some(grouped.join(" "));
                }
                _ => {}
            }
        }
        let valid_url = |u: &String| u.starts_with("http://") || u.starts_with("https://");
        if out.urls.is_empty() || !out.urls.iter().all(valid_url) {
            return Err("the pairing link has no valid server address".into());
        }
        if out.code.chars().filter(char::is_ascii_digit).count() != 8 {
            return Err("the pairing link has no valid pairing code".into());
        }
        Ok(out)
    }
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn auto_level() -> ThinkingLevel {
    ThinkingLevel::Auto
}

impl SmallMode {
    pub fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// A field that may be `null` on purpose: present (even as `null`) is
/// `Some`, left out is `None` (with `#[serde(default)]`).
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

/// "Who can use it" (credentials, machines): `bots` `None` is any chat;
/// `Some(ids)` only sessions acting as one of those bots (none listed:
/// nobody). A session acts as its `bot_id` (a channel's current speaker);
/// subagents carry their root's bot and a bot's scheduled duties run as
/// that bot. `bot` is `None` for an ordinary chat. Private chats and helper
/// sessions are refused before this is asked.
pub fn bot_allowed(bots: Option<&[String]>, bot: Option<&str>) -> bool {
    match bots {
        None => true,
        Some(list) => bot.is_some_and(|b| list.iter().any(|x| x == b)),
    }
}

/// Stored secrets a machine command gets as environment variables (name →
/// value), only those the chat may use there. Travels only inside the call
/// to that machine; printed (`{:?}`) it shows the names alone.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(transparent)]
pub struct SecretEnv(pub std::collections::BTreeMap<String, String>);

impl SecretEnv {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretEnv")
            .field(&self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn default_delivery() -> Delivery {
    Delivery::Auto
}

/// A machine's default time limit per action (5 minutes).
pub const MACHINE_TIMEOUT_SECS: u64 = 300;
/// The longest time limit a machine may set (4 hours).
pub const MACHINE_TIMEOUT_MAX: u64 = 4 * 3600;

fn machine_timeout() -> u64 {
    MACHINE_TIMEOUT_SECS
}

/// The tightest scope: no folders, no commands, every action asks.
impl Default for MachineScope {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            commands: false,
            ask: MachineAsk::Always,
            timeout_secs: MACHINE_TIMEOUT_SECS,
            desktop: false,
            bots: None,
            webviews: false,
            terminal: TerminalAccess::Off,
            admin: AdminAccess::Off,
            stopped: false,
            stops: 0,
        }
    }
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl TerminalAccess {
    pub fn is_off(&self) -> bool {
        *self == TerminalAccess::Off
    }
}

impl AdminAccess {
    pub fn is_off(&self) -> bool {
        *self == AdminAccess::Off
    }
}

/// A terminal closes after half an hour without typing (while nothing runs).
pub const TERMINAL_IDLE_MINUTES: u32 = 30;

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            access: TerminalAccess::Off,
            idle_minutes: TERMINAL_IDLE_MINUTES,
        }
    }
}

impl Bot {
    /// What this bot may do on machine `id`, before the machine's own say
    /// (bots made before the setting: as the machine allows).
    pub fn machine_access(&self, id: &str) -> MachineAccess {
        match &self.machines {
            None => MachineAccess::Allowed,
            Some(m) => m.each.get(id).copied().unwrap_or(m.default),
        }
    }
}

impl NodeAction {
    /// `read`, `list`, `write`, `edit`, `run`, or the desktop action's
    /// own name (`screenshot`, `click`…).
    pub fn name(&self) -> &'static str {
        match self {
            NodeAction::Read { .. } => "read",
            NodeAction::List { .. } => "list",
            NodeAction::Write { .. } => "write",
            NodeAction::Edit { .. } => "edit",
            NodeAction::Run { .. } => "run",
            NodeAction::Desktop { desktop, .. } => desktop.name(),
        }
    }

    /// The path or command it acts on; for a desktop action, what it does
    /// where (typed text only by its length).
    pub fn target(&self) -> std::borrow::Cow<'_, str> {
        match self {
            NodeAction::Read { path, .. }
            | NodeAction::List { path }
            | NodeAction::Write { path, .. }
            | NodeAction::Edit { path, .. } => path.into(),
            NodeAction::Run { command, .. } => command.into(),
            NodeAction::Desktop { desktop, .. } => desktop.describe().into(),
        }
    }

    /// Only reads files or lists folders (what a bot with read-only access
    /// to a machine may do there; looking at the screen is more than that).
    pub fn only_reads_files(&self) -> bool {
        matches!(self, NodeAction::Read { .. } | NodeAction::List { .. })
    }

    /// Changes something (anything but reading, listing and looking).
    pub fn changes(&self) -> bool {
        match self {
            NodeAction::Read { .. } | NodeAction::List { .. } => false,
            NodeAction::Desktop { desktop, .. } => desktop.acts(),
            _ => true,
        }
    }
}

impl DesktopOp {
    pub fn name(&self) -> &'static str {
        match self {
            DesktopOp::Screens => "screens",
            DesktopOp::Screenshot { .. } => "screenshot",
            DesktopOp::Move { .. } => "move",
            DesktopOp::Click { .. } => "click",
            DesktopOp::Drag { .. } => "drag",
            DesktopOp::Scroll { .. } => "scroll",
            DesktopOp::Type { .. } => "type",
            DesktopOp::Key { .. } => "key",
        }
    }

    /// Uses the mouse or keyboard (anything but looking).
    pub fn acts(&self) -> bool {
        !matches!(self, DesktopOp::Screens | DesktopOp::Screenshot { .. })
    }

    /// The screen it is about (1: the main one).
    pub fn screen(&self) -> u32 {
        match self {
            DesktopOp::Screenshot { screen }
            | DesktopOp::Move { screen, .. }
            | DesktopOp::Click { screen, .. }
            | DesktopOp::Drag { screen, .. }
            | DesktopOp::Scroll { screen, .. } => screen.unwrap_or(1).max(1),
            _ => 1,
        }
    }

    /// One line: what it does where. Typed text isn't repeated (it may be
    /// something private), only how long it is.
    pub fn describe(&self) -> String {
        let on = |s: &Option<u32>| match s {
            Some(n) if *n > 1 => format!(" on screen {n}"),
            _ => String::new(),
        };
        match self {
            DesktopOp::Screens => "screens".into(),
            DesktopOp::Screenshot { screen } => format!("screenshot{}", on(screen)),
            DesktopOp::Move { screen, x, y } => format!("move to {x},{y}{}", on(screen)),
            DesktopOp::Click {
                screen,
                x,
                y,
                button,
                clicks,
            } => {
                let how = match (button, clicks.unwrap_or(1)) {
                    (MouseButton::Left, 2) => "double-click",
                    (MouseButton::Left, _) => "click",
                    (MouseButton::Right, _) => "right-click",
                    (MouseButton::Middle, _) => "middle-click",
                };
                format!("{how} {x},{y}{}", on(screen))
            }
            DesktopOp::Drag {
                screen,
                x,
                y,
                to_x,
                to_y,
            } => format!("drag {x},{y} → {to_x},{to_y}{}", on(screen)),
            DesktopOp::Scroll {
                screen,
                x,
                y,
                dx,
                dy,
            } => {
                let mut how = Vec::new();
                if *dy != 0 {
                    how.push(format!(
                        "{} {}",
                        if *dy > 0 { "down" } else { "up" },
                        dy.abs()
                    ));
                }
                if *dx != 0 {
                    how.push(format!(
                        "{} {}",
                        if *dx > 0 { "right" } else { "left" },
                        dx.abs()
                    ));
                }
                format!("scroll {} at {x},{y}{}", how.join(", "), on(screen))
            }
            DesktopOp::Type { text } => {
                let n = text.chars().count();
                format!("type {n} character{}", if n == 1 { "" } else { "s" })
            }
            DesktopOp::Key { keys } => {
                format!("key {}", keys.chars().take(80).collect::<String>())
            }
        }
    }
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// `provider` of a [`ModelRef`] that names a model role (`@fast`) instead of
/// a model; the server resolves it from its `[models]` configuration.
pub const ROLE_PROVIDER: &str = "@";

impl ModelRef {
    /// A reference to a configured model role, e.g. `ModelRef::role("fast")`.
    pub fn role(name: &str) -> Self {
        Self {
            provider: ROLE_PROVIDER.into(),
            model: name.trim_start_matches('@').into(),
        }
    }

    /// The role this names (`@fast` → `fast`), if it is one.
    pub fn as_role(&self) -> Option<&str> {
        (self.provider == ROLE_PROVIDER).then_some(self.model.as_str())
    }
}

impl Price {
    pub const fn new(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Self {
        Self {
            input,
            output,
            cache_read: Some(cache_read),
            cache_write: Some(cache_write),
        }
    }

    /// Cost of `input`, `output`, `cache_read` and `cache_write` tokens, in
    /// US dollars (`input` excludes the cached tokens, as in `Usage`).
    pub fn cost_of(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
        (input as f64 * self.input
            + output as f64 * self.output
            + cache_read as f64 * self.cache_read.unwrap_or(self.input)
            + cache_write as f64 * self.cache_write.unwrap_or(self.input))
            / 1_000_000.0
    }

    /// Cost of one response's usage, in US dollars.
    pub fn cost(&self, u: &Usage) -> f64 {
        self.cost_of(u.input, u.output, u.cache_read, u.cache_write)
    }

    /// Every rate is a finite, non-negative number of dollars (at most
    /// $100,000 per million tokens).
    pub fn is_valid(&self) -> bool {
        [
            Some(self.input),
            Some(self.output),
            self.cache_read,
            self.cache_write,
        ]
        .into_iter()
        .flatten()
        .all(|v| v.is_finite() && (0.0..=100_000.0).contains(&v))
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.as_role() {
            Some(role) => write!(f, "@{role}"),
            None => write!(f, "{}/{}", self.provider, self.model),
        }
    }
}

impl std::str::FromStr for ModelRef {
    type Err = String;
    /// `provider/model`, or `@role` for a configured model role.
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if let Some(role) = s.strip_prefix('@') {
            let ok = !role.is_empty()
                && role
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            return if ok {
                Ok(Self::role(role))
            } else {
                Err(format!(
                    "expected @role (letters, digits, - or _), got `{s}`"
                ))
            };
        }
        match s.split_once('/') {
            Some((p, m)) if !p.is_empty() && !m.is_empty() => Ok(Self {
                provider: p.into(),
                model: m.into(),
            }),
            _ => Err(format!("expected provider/model or @role, got `{s}`")),
        }
    }
}

impl ContentPart {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
}

/// Concatenated text of content parts (images omitted).
pub fn plain_text(parts: &[ContentPart]) -> String {
    let mut out = String::new();
    for p in parts {
        if let ContentPart::Text { text } = p {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::SessionUpdated { session } => Some(&session.id),
            Event::SessionRemoved { session_id }
            | Event::SessionRewound { session_id, .. }
            | Event::EntryAdded { session_id, .. }
            | Event::EntriesChanged { session_id, .. }
            | Event::StreamStarted { session_id, .. }
            | Event::StreamDelta { session_id, .. }
            | Event::StreamEnded { session_id }
            | Event::ToolStarted { session_id, .. }
            | Event::ToolOutput { session_id, .. }
            | Event::ApprovalRequested { session_id, .. }
            | Event::ApprovalResolved { session_id, .. }
            | Event::QueueChanged { session_id, .. } => Some(session_id),
            Event::Notice { session_id, .. } => session_id.as_deref(),
            Event::FileShared { file } => file.session_id.as_deref(),
            Event::BoardUpdated { board } => Some(&board.session_id),
            Event::BoardRemoved { session_id, .. } => Some(session_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn who_can_use_it_and_secret_env_never_print_values() {
        use super::{SecretEnv, bot_allowed};
        let only = vec!["b1".to_string()];
        assert!(bot_allowed(None, None) && bot_allowed(None, Some("b2")));
        assert!(bot_allowed(Some(&only), Some("b1")));
        assert!(!bot_allowed(Some(&only), Some("b2")) && !bot_allowed(Some(&only), None));
        assert!(!bot_allowed(Some(&[]), Some("b1")), "none listed: nobody");
        let env = SecretEnv([("LAB_KEY".to_string(), "v4lue-xyz".to_string())].into());
        assert_eq!(format!("{env:?}"), r#"SecretEnv(["LAB_KEY"])"#);
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"LAB_KEY":"v4lue-xyz"}"#
        );
    }

    #[test]
    fn pair_links_round_trip() {
        let l = PairLink {
            name: "Home lab".into(),
            urls: vec![
                "http://192.168.1.50:7878".into(),
                "https://tessera.example.com".into(),
            ],
            headers: vec![
                ("CF-Access-Client-Id".into(), "abc.access".into()),
                ("CF-Access-Client-Secret".into(), "s3cr&t=+x".into()),
            ],
            code: "1234-5678".into(),
            fingerprint: Some("3f9a 1c2b 77e0 5d41 a0c9".into()),
        };
        let link = l.to_link();
        assert!(link.starts_with("tessera://pair?v=1&"));
        assert!(!link.contains(' '));
        assert_eq!(PairLink::parse(&link).unwrap(), l);
        assert!(PairLink::parse("https://evil.example/pair?c=1").is_err());
        assert!(PairLink::parse("tessera://pair?v=1&u=javascript:alert(1)&c=12345678").is_err());
        assert!(PairLink::parse("tessera://pair?v=2&u=http://x&c=12345678").is_err());
        assert!(PairLink::parse("tessera://pair?v=1&u=http://x&c=12").is_err());
    }

    use super::*;

    #[test]
    fn request_roundtrip() {
        let msg = ClientMessage {
            id: 7,
            request: Request::Send {
                session_id: "s1".into(),
                content: vec![ContentPart::text("hi")],
                delivery: Delivery::Steer,
                attachments: Vec::new(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""method":"send""#), "{json}");
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        // Params with defaults may be omitted entirely.
        let m: ClientMessage = serde_json::from_str(r#"{"id":1,"method":"list_jobs"}"#).unwrap();
        assert_eq!(m.request, Request::ListJobs);
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":1,"method":"send","params":{"session_id":"a","content":[]}}"#,
        )
        .unwrap();
        assert!(matches!(
            m.request,
            Request::Send {
                delivery: Delivery::Auto,
                ..
            }
        ));
    }

    #[test]
    fn server_message_shapes() {
        let ev = ServerMessage::Event {
            event: Event::StreamEnded {
                session_id: "s".into(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"event":{"type":"stream_ended","session_id":"s"}}"#
        );
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
        let ok: ServerMessage = serde_json::from_str(r#"{"id":3,"result":null}"#).unwrap();
        assert!(matches!(ok, ServerMessage::Response { id: 3, .. }));
    }

    #[test]
    fn model_ref_parse() {
        let m: ModelRef = "local/qwen/qwen3".parse().unwrap();
        assert_eq!(m.provider, "local");
        assert_eq!(m.model, "qwen/qwen3");
        assert!("nope".parse::<ModelRef>().is_err());
        // Model roles round-trip as `@role`.
        let r: ModelRef = "@fast".parse().unwrap();
        assert_eq!(r.as_role(), Some("fast"));
        assert_eq!(r.to_string(), "@fast");
        assert_eq!(m.as_role(), None);
        assert!("@".parse::<ModelRef>().is_err());
        assert!("@a/b".parse::<ModelRef>().is_err());
        // Old ledgers and usage records still parse.
        let l: TaskLedger = serde_json::from_str(r#"{"goal":"g","constraints":[],"plan":[],"decisions":[],"open_questions":[],"artifacts":[],"notes":[],"done_criteria":["x"]}"#).unwrap();
        assert!(l.criteria.is_empty());
        let u: Usage =
            serde_json::from_str(r#"{"input":1,"output":2,"cache_read":0,"cache_write":0}"#)
                .unwrap();
        assert_eq!(u.cost_usd, None);
        assert!(!serde_json::to_string(&u).unwrap().contains("cost"));
    }

    #[test]
    fn new_fields_default_and_new_requests_parse() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"id":1,"method":"get_model_roles"}"#).unwrap();
        assert_eq!(m.request, Request::GetModelRoles);
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":2,"method":"set_model_role","params":{"role":"memory"}}"#,
        )
        .unwrap();
        assert_eq!(
            m.request,
            Request::SetModelRole {
                role: "memory".into(),
                model: None
            }
        );
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":3,"method":"create_board","params":{"session_id":"s","title":"t","content":{"type":"url","url":"https://example.com"}}}"#,
        )
        .unwrap();
        assert!(matches!(m.request, Request::CreateBoard { .. }));
        // Stored sessions and boards from before still load.
        let info: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": "s", "title": "t", "kind": "chat", "cwd": "/", "model": {"provider": "p", "model": "m"},
            "thinking": "auto", "permission": "default", "status": "idle", "created_at": 1, "updated_at": 1,
            "last_seq": 0, "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
            "context": {"tokens": 0, "window": 1, "compactions": 0}, "pinned": false, "archived": false,
            "preview": ""
        }))
        .unwrap();
        assert!(!info.internal && !info.fast && info.subagents);
        assert!(info.helper_limits.is_none() && !info.helper_queued);
        assert_eq!(info.revision, 0);
        let revised = SessionInfo {
            revision: 7,
            ..info.clone()
        };
        let event = Event::SessionUpdated {
            session: revised.clone(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["session"]["revision"], 7);
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
        // `internal`, `subagents` and the helper fields are only sent when not
        // the default, so apps from before see the same sessions.
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("internal").is_none() && json["fast"] == false);
        assert!(json.get("subagents").is_none());
        assert!(json.get("helper_limits").is_none() && json.get("helper_queued").is_none());
        let off = SessionInfo {
            subagents: false,
            helper_limits: Some(HelperLimits {
                at_once: Some(0),
                steps: Some(120),
                minutes: None,
            }),
            helper_queued: true,
            ..info
        };
        let json = serde_json::to_value(&off).unwrap();
        assert_eq!(json["subagents"], false);
        assert_eq!(
            json["helper_limits"],
            serde_json::json!({"at_once": 0, "steps": 120})
        );
        assert_eq!(json["helper_queued"], true);
        // A queued helper is still `running` on the wire: apps from before
        // decode the status as a closed list.
        assert!(serde_json::from_str::<SessionStatus>(r#""queued""#).is_err());
        // Helper settings from an older server, and requests from an older app.
        let old: HelperSettings =
            serde_json::from_str(r#"{"at_once":4,"default":4,"max":32}"#).unwrap();
        assert!(old.steps.is_none() && old.capacity.is_none());
        let m: ClientMessage =
            serde_json::from_str(r#"{"id":8,"method":"set_helpers","params":{"at_once":6}}"#)
                .unwrap();
        assert_eq!(
            m.request,
            Request::SetHelpers {
                at_once: 6,
                enabled: None,
                steps: None,
                minutes: None
            }
        );
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":9,"method":"set_chat_helpers","params":{"session_id":"s","limits":{"at_once":12}}}"#,
        )
        .unwrap();
        assert_eq!(
            m.request,
            Request::SetChatHelpers {
                session_id: "s".into(),
                limits: HelperLimits {
                    at_once: Some(12),
                    ..Default::default()
                }
            }
        );
        for (raw, want) in [
            (r#"{"id":4,"method":"get_usage","params":{}}"#, "GetUsage"),
            (r#"{"id":5,"method":"list_notes"}"#, "ListNotes"),
            (
                r#"{"id":6,"method":"save_note","params":{"title":"t","content":"c"}}"#,
                "SaveNote",
            ),
            (
                r#"{"id":7,"method":"board_snapshot","params":{"request_id":"r","error":"can't capture on this device"}}"#,
                "BoardSnapshot",
            ),
        ] {
            let m: ClientMessage = serde_json::from_str(raw).unwrap();
            assert!(format!("{:?}", m.request).starts_with(want), "{raw}");
        }
        let tile: MosaicTile = serde_json::from_str(r#"{"id":"t","note_id":"nt_1"}"#).unwrap();
        assert_eq!(tile.note_id.as_deref(), Some("nt_1"));
        let b: Whiteboard = serde_json::from_str(
            r#"{"id":"b","session_id":"s","title":"t","content":{"type":"markdown","markdown":"x"},"version":1,"created_at":1,"updated_at":1}"#,
        )
        .unwrap();
        assert!(!b.by_user);
    }

    #[test]
    fn the_shared_freedom_default_is_optional_both_ways() {
        // An older client's `set_security` changes nothing about it.
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":1,"method":"set_security","params":{"ask_outside":true}}"#,
        )
        .unwrap();
        assert!(matches!(
            m.request,
            Request::SetSecurity {
                default_permission: None,
                ask_outside: Some(true),
                ..
            }
        ));
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":2,"method":"set_security","params":{"default_permission":"yolo"}}"#,
        )
        .unwrap();
        assert!(matches!(
            m.request,
            Request::SetSecurity {
                default_permission: Some(PermissionMode::Auto),
                ..
            }
        ));
        // An older server's reply (and an unset value) has none; it is left
        // out rather than sent as null.
        let old = serde_json::json!({
            "sandbox": "auto", "sandbox_status": "on", "sandbox_complete": true, "protected": [],
            "folders": [], "ask_outside": true, "redact_patterns": true, "require_e2ee": true,
            "save_secrets_from_chat": true
        });
        let info: SecurityInfo = serde_json::from_value(old).unwrap();
        assert_eq!(info.default_permission, None);
        assert!(
            serde_json::to_value(&info)
                .unwrap()
                .get("default_permission")
                .is_none()
        );
        let set = SecurityInfo {
            default_permission: Some(PermissionMode::ReadOnly),
            ..info
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap()["default_permission"],
            "read_only"
        );
    }

    #[test]
    fn pricing_requests_and_reports_have_their_documented_shapes() {
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":1,"method":"set_subscription","params":{"provider":"openai-codex","plan":"ChatGPT Pro","monthly_usd":200}}"#,
        )
        .unwrap();
        assert_eq!(
            m.request,
            Request::SetSubscription {
                provider: "openai-codex".into(),
                plan: Some("ChatGPT Pro".into()),
                monthly_usd: Some(200.0)
            }
        );
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":2,"method":"set_model_price","params":{"provider":"p","model":"m","price":{"input":1,"output":2}}}"#,
        )
        .unwrap();
        let Request::SetModelPrice {
            price: Some(price), ..
        } = m.request
        else {
            panic!("{m:?}")
        };
        // Cache reads and writes cost the input price unless given.
        assert!((price.cost_of(1_000_000, 1_000_000, 1_000_000, 0) - 4.0).abs() < 1e-12);
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":3,"method":"set_model_price","params":{"provider":"p","model":"m"}}"#,
        )
        .unwrap();
        assert!(matches!(
            m.request,
            Request::SetModelPrice { price: None, .. }
        ));
        assert!(price.is_valid() && !Price::new(1.0, f64::NAN, 0.0, 0.0).is_valid());
        assert!(!Price::new(-1.0, 1.0, 0.0, 0.0).is_valid());
        // Reports from older servers still load.
        let r: UsageReport = serde_json::from_str(
            r#"{"from":0,"to":1,"total":{"requests":0,"input":0,"output":0,"cache_read":0,"cache_write":0},"by_model":[{"provider":"p","model":"m","totals":{"requests":0,"input":0,"output":0,"cache_read":0,"cache_write":0}}],"by_kind":[],"by_day":[]}"#,
        )
        .unwrap();
        assert!(r.unpriced.is_empty() && r.subscriptions.is_empty());
        assert_eq!(r.by_model[0].billing, None);
        // …and subscriptions from before the limits.
        let s: SubscriptionInfo =
            serde_json::from_str(r#"{"provider":"openai-codex","label":"ChatGPT","requests":3}"#)
                .unwrap();
        assert!(s.limits.is_empty());
        let l: PlanLimit = serde_json::from_str(
            r#"{"window":"five_hour","label":"5-hour limit","used_percent":42.5,"status":"warning","resets_at":1,"updated_at":2}"#,
        )
        .unwrap();
        assert_eq!(
            (l.used_percent, l.status, l.resets_at),
            (Some(42.5), Some(LimitStatus::Warning), Some(1))
        );
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["by_model"][0].get("price").is_none());
        assert_eq!(
            serde_json::to_value(Billing::Subscription).unwrap(),
            "subscription"
        );
        assert_eq!(
            serde_json::to_value(PriceSource::Catalog).unwrap(),
            "catalog"
        );
    }

    #[test]
    fn browser_messages_have_their_documented_shapes() {
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":1,"method":"browser_attach","params":{"board_id":"wb_1","width":800,"height":600,"dpr":2}}"#,
        )
        .unwrap();
        assert_eq!(
            m.request,
            Request::BrowserAttach {
                board_id: "wb_1".into(),
                width: 800,
                height: 600,
                dpr: 2.0,
                mobile: false,
                pipelined: false,
                formats: vec![],
            }
        );
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":2,"method":"browser_input","params":{"board_id":"b","event":{"type":"mouse","kind":"down","x":10.5,"y":3,"button":"left","clicks":1}}}"#,
        )
        .unwrap();
        assert!(matches!(
            m.request,
            Request::BrowserInput {
                event: BrowserInputEvent::Mouse {
                    kind: BrowserMouseKind::Down,
                    button: BrowserMouseButton::Left,
                    clicks: 1,
                    modifiers: 0,
                    ..
                },
                ..
            }
        ));
        for raw in [
            r#"{"type":"touch","kind":"start","points":[{"x":1,"y":2,"id":0}]}"#,
            r#"{"type":"key","kind":"down","key":"Enter","code":"Enter","text":"\r","modifiers":8}"#,
            r#"{"type":"text","text":"héllo"}"#,
            r#"{"type":"mouse","kind":"wheel","x":1,"y":1,"dy":120}"#,
        ] {
            let e: BrowserInputEvent = serde_json::from_str(raw).unwrap();
            let back: BrowserInputEvent =
                serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
            assert_eq!(back, e, "{raw}");
        }
        let m: ClientMessage = serde_json::from_str(
            r#"{"id":3,"method":"browser_nav","params":{"board_id":"b","action":{"type":"zoom","factor":1.25}}}"#,
        )
        .unwrap();
        assert!(matches!(
            m.request,
            Request::BrowserNav {
                action: BrowserNavAction::Zoom { .. },
                ..
            }
        ));
        let ev = Event::BrowserDialog {
            board_id: "b".into(),
            kind: BrowserDialogKind::Beforeunload,
            message: "Leave?".into(),
            default: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"type":"browser_dialog","board_id":"b","kind":"beforeunload","message":"Leave?"}"#
        );
        assert_eq!(ev.session_id(), None);
    }
}

/// Serving a whiteboard's HTML as an isolated page: the same strict policy
/// wherever it is shown (the apps' custom protocol, the server's own page).
pub mod board {
    /// No network at all: scripts, styles and images only from the page
    /// itself, the bundled libraries and data: URLs.
    pub const CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
        img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' data: blob:; connect-src 'none'; \
        worker-src blob:; frame-src 'none'; form-action 'none'; base-uri 'none'";

    /// The policy header for a page served from `origin`
    /// (`http://127.0.0.1:4000`). WebKit matches `'self'` against a sandboxed
    /// frame's opaque origin, so the page's own origin is named too (or the
    /// bundled libraries wouldn't load). Anything that isn't a plain origin
    /// is ignored. It also sandboxes the page wherever it is opened (the UI
    /// frames it so; opened on its own it would otherwise share its origin
    /// with the UI's).
    pub fn csp(origin: Option<&str>) -> String {
        format!("{}; sandbox allow-scripts", meta_csp(origin))
    }

    /// Only these routes may be loaded as HTML frames. This is also used by
    /// native pre-request navigation guards, not just by response policies.
    pub fn frame_path(path: &str) -> bool {
        let segment = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        };
        let parts: Vec<_> = path.split('/').collect();
        match parts.as_slice() {
            ["", "board", id, version, ""] => segment(id) && segment(version),
            ["", "preview", token, ""] => segment(token),
            _ => false,
        }
    }

    /// The same policy for the page's own `<meta>` (where `sandbox` isn't
    /// allowed).
    fn meta_csp(origin: Option<&str>) -> String {
        match origin.filter(|o| plain_origin(o)) {
            Some(o) => CSP.replace("'self'", &format!("'self' {o}")),
            None => CSP.to_string(),
        }
    }

    fn plain_origin(o: &str) -> bool {
        let Some(host) = o
            .strip_prefix("http://")
            .or_else(|| o.strip_prefix("https://"))
        else {
            return false;
        };
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    }

    macro_rules! lib {
        ($path:literal, $mime:literal) => {
            (
                $path,
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../assets/board-lib/",
                    $path
                ))
                .as_slice(),
                $mime,
            )
        };
    }

    /// Libraries a board can use, served under `/board-lib/`.
    const LIBS: &[(&str, &[u8], &str)] = &[
        lib!("three.module.js", "text/javascript"),
        lib!("three-addons/controls/OrbitControls.js", "text/javascript"),
        lib!(
            "three-addons/controls/TrackballControls.js",
            "text/javascript"
        ),
        lib!("three-addons/renderers/CSS2DRenderer.js", "text/javascript"),
        lib!("three-addons/renderers/CSS3DRenderer.js", "text/javascript"),
        lib!("d3.js", "text/javascript"),
    ];

    /// A bundled library by its path under `/board-lib/`.
    pub fn lib(path: &str) -> Option<(&'static [u8], &'static str)> {
        LIBS.iter()
            .find(|(p, ..)| *p == path)
            .map(|(_, b, m)| (*b, *m))
    }

    /// The page for a board's HTML: the policy (also sent as a header, see
    /// [`csp`]), an import map for `three`, and the app's colours as CSS
    /// variables.
    pub fn document(html: &str, dark: bool, origin: Option<&str>) -> String {
        let csp = meta_csp(origin);
        let (bg, fg, muted, accent, surface) = if dark {
            ("#141312", "#ece6dc", "#9b948a", "#d4ad5e", "#1d1b19")
        } else {
            ("#f6f2ea", "#211e1a", "#6f685f", "#9a7431", "#ece6da")
        };
        let head = format!(
            r#"<meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="{csp}"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="referrer" content="no-referrer"><script type="importmap">{{"imports":{{"three":"/board-lib/three.module.js","three/addons/":"/board-lib/three-addons/"}}}}</script><style>:root{{color-scheme:{scheme};--tessera-bg:{bg};--tessera-fg:{fg};--tessera-muted:{muted};--tessera-accent:{accent};--tessera-surface:{surface}}}html,body{{margin:0;background:var(--tessera-bg);color:var(--tessera-fg);font:15px/1.5 system-ui,sans-serif}}</style>"#,
            scheme = if dark { "dark" } else { "light" },
        );
        let lower = html.to_ascii_lowercase();
        // Put ours first so it applies before anything the page declares.
        if let Some(i) = lower.find("<head>") {
            let at = i + "<head>".len();
            format!("{}{head}{}", &html[..at], &html[at..])
        } else if let Some(i) = lower.find("<html") {
            let at = i + lower[i..].find('>').map_or(0, |j| j + 1);
            format!("{}<head>{head}</head>{}", &html[..at], &html[at..])
        } else {
            format!("<!doctype html><html><head>{head}</head><body>{html}</body></html>")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_board_and_preview_routes_are_frame_pages() {
            for path in [
                "/board/wb_abc/3/",
                "/board/wb_abc/token-123/",
                "/preview/token_123/",
            ] {
                assert!(frame_path(path), "{path}");
            }
            for path in [
                "/",
                "/api/health",
                "/board/../x/",
                "/board/x/1/extra",
                "/board/x/1",
                "/preview//",
                "/preview/%2e%2e/",
                "/board-lib/d3.js",
            ] {
                assert!(!frame_path(path), "{path}");
            }
        }

        #[test]
        fn pages_get_the_policy_first_and_the_libraries_resolve() {
            for html in [
                "<p>hi</p>",
                "<html><body>x</body></html>",
                "<!doctype html><html lang=en><head><title>t</title></head><body></body></html>",
            ] {
                let d = document(html, true, None);
                let csp = d.find("Content-Security-Policy").unwrap();
                assert!(csp < d.find("<title>").unwrap_or(usize::MAX));
                assert!(csp < d.find("<body").unwrap(), "{d}");
                assert!(d.contains("connect-src 'none'"));
            }
            assert!(
                lib("three.module.js")
                    .is_some_and(|(b, m)| b.len() > 100_000 && m == "text/javascript")
            );
            assert!(lib("three-addons/controls/OrbitControls.js").is_some());
            assert!(lib("../Cargo.toml").is_none());
        }

        #[test]
        fn the_page_names_its_own_origin() {
            let c = meta_csp(Some("http://127.0.0.1:4000"));
            assert!(c.contains("script-src 'self' http://127.0.0.1:4000 'unsafe-inline'"));
            assert!(c.contains("connect-src 'none'") && c.contains("default-src 'none'"));
            let d = document("<p>x</p>", false, Some("http://127.0.0.1:4000"));
            assert!(d.contains(&format!("content=\"{c}\"")));
            for bad in [
                "http://a; connect-src *",
                "http://a b",
                "javascript:x",
                "http://",
                "http://a\"x",
            ] {
                assert_eq!(meta_csp(Some(bad)), CSP, "{bad}");
            }
            assert_eq!(meta_csp(None), CSP);
        }

        #[test]
        fn the_header_sandboxes_the_page_wherever_it_opens() {
            let h = csp(Some("http://127.0.0.1:4000"));
            assert!(h.starts_with(&meta_csp(Some("http://127.0.0.1:4000"))));
            assert!(h.ends_with("; sandbox allow-scripts"), "{h}");
            // Not in the page's own copy, where browsers ignore it.
            assert!(!document("<p>x</p>", true, None).contains("sandbox"));
        }
    }
}
