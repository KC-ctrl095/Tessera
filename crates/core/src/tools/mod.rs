//! Tool framework and registry.

pub mod admission;
pub mod bash;
pub mod board;
pub mod browser;
pub mod desktop;
pub mod docs;
pub(crate) mod file_object;
pub mod filecache;
pub mod files;
pub mod images;
pub mod linemap;
pub mod lsp;
pub mod machines;
pub mod meta;
pub mod note;
pub mod notify;
pub mod patch;
pub mod policy;
pub mod quick;
pub mod redaction;
pub mod search;
pub mod secrets;
pub mod settings;
pub mod share;
pub mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;
use tessera_proto::{ContentPart, PermissionMode, TaskLedger};
use tokio_util::sync::CancellationToken;

use crate::llm::ToolSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// No side effects.
    Read,
    /// Modifies files.
    Write,
    /// Runs programs.
    Exec,
    /// Harness state (ledger, memory, skills, jobs, subagents).
    Meta,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: Vec<ContentPart>,
    pub is_error: bool,
    pub details: Option<Value>,
}

impl ToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::text(s)],
            is_error: false,
            details: None,
        }
    }
    pub fn error(s: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::text(s)],
            is_error: true,
            details: None,
        }
    }
    pub fn with_details(mut self, d: Value) -> Self {
        self.details = Some(d);
        self
    }
}

/// Outcome of the policy check that runs before a tool executes.
#[derive(Debug, Clone, PartialEq)]
pub enum Risk {
    None,
    /// Needs user approval. `key` identifies what "allow for this session" covers.
    Ask {
        reason: String,
        key: String,
    },
    /// Never allowed, whatever the permission mode.
    Block {
        reason: String,
    },
}

/// The things one question's `key` covers: a command that both pushes and
/// uses a password asks once, with a key of `bash:…` and `secret:…` parts
/// joined by `+`, and "allow for this session" allows each part; the
/// question is skipped only when every part is allowed. Any other key
/// (a path, a URL) is a single thing, however it is spelled.
pub fn key_parts(key: &str) -> Vec<String> {
    let plain = |p: &str| {
        ["bash:", "secret:"].iter().any(|pre| {
            p.strip_prefix(pre).is_some_and(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
        })
    };
    let parts: Vec<&str> = key.split('+').collect();
    if parts.len() > 1 && parts.iter().all(|p| plain(p)) {
        parts.into_iter().map(str::to_string).collect()
    } else {
        vec![key.to_string()]
    }
}

/// One permission assessment for native dispatch and retained CLI built-ins.
pub fn permission_risk(tool: &dyn Tool, args: &Value, ctx: &ToolCtx, mode: PermissionMode) -> Risk {
    let risk = tool.assess(args, ctx);
    let kind = tool.action_kind(args);
    match (&risk, mode) {
        (Risk::Block { .. }, _) => risk,
        (_, PermissionMode::ReadOnly)
            if matches!(kind, ToolKind::Write | ToolKind::Exec) =>
        {
            Risk::Block {
                reason: "This session is read-only; the user can change its mode to allow changes or commands.".into(),
            }
        }
        (Risk::Ask { .. }, _) if files::startup_write(tool.name(), args, ctx) => risk,
        (_, PermissionMode::Ask)
            if kind == ToolKind::Write && matches!(tool.name(), "note" | "memory" | "skill_manage" | "schedule") =>
        {
            Risk::Ask {
                reason: format!("`{}` changes saved information or future work.", tool.name()),
                key: approval_key(tool.name(), args, ctx),
            }
        }
        (_, PermissionMode::Auto) => Risk::None,
        (Risk::None, PermissionMode::Ask)
            if matches!(kind, ToolKind::Write | ToolKind::Exec) =>
        {
            Risk::Ask {
                reason: format!("`{}` changes files or runs programs.", tool.name()),
                key: approval_key(tool.name(), args, ctx),
            }
        }
        _ => risk,
    }
}

/// Ask-mode grants for otherwise ordinary file/command calls cover only
/// this path or exact command, not every future use of the whole tool.
pub fn approval_key(tool: &str, args: &Value, ctx: &ToolCtx) -> String {
    if tool == "bash" {
        return format!(
            "exec:{}:{}",
            canonical(
                &args
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(|p| ctx.resolve(p))
                    .unwrap_or_else(|| ctx.cwd.clone())
            )
            .display(),
            args.get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
    }
    if matches!(tool, "write" | "edit")
        && let Some(path) = args.get("path").and_then(Value::as_str)
    {
        return format!("write:{}", canonical(&ctx.resolve(path)).display());
    }
    if matches!(tool, "note" | "memory" | "skill_manage" | "schedule") {
        return format!("change:{tool}:{args}");
    }
    format!("tool:{tool}")
}
/// Work a bot took on in the background (`ToolHost::hand_to_bot`).
#[derive(Debug, Clone)]
pub struct HandedOff {
    /// The bot's conversation, where it works.
    pub session_id: String,
    /// `@handle`.
    pub bot: String,
}

/// Services the engine provides to tools.
pub trait ToolHost: Send + Sync {
    fn store(&self) -> &crate::store::Store;
    /// Memory of whoever is acting in the session (a bot's own, or the shared one).
    fn memory(&self, session_id: &str) -> Arc<crate::memory::Memory>;
    /// Bot acting in the session, if any.
    fn session_bot(&self, session_id: &str) -> Option<String>;
    /// Send a private message to another bot and wait for its reply.
    fn message_bot(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>>;
    /// Whether this session may hand work to a bot in the background (not
    /// subagents, jobs, rooms or bots answering bots: they wait).
    fn may_hand_off_in_background(&self, _session_id: &str) -> bool {
        false
    }
    /// Hand work to another bot and return at once; its reply arrives in
    /// `caller` as a message when it is done.
    fn hand_to_bot(
        &self,
        _caller: &str,
        _target: &str,
        _message: &str,
        _call_id: &str,
    ) -> Result<HandedOff, String> {
        Err("bots aren't available here".into())
    }
    fn skills(&self, cwd: &Path) -> Vec<crate::skills::Skill>;
    /// The `skill` tool loaded `name` (counted, so a capped skills index
    /// lists the most used first).
    fn skill_used(&self, _name: &str) {}
    /// What the recent conversation shows about a skill about to be saved.
    fn skill_evidence(&self, session_id: &str) -> crate::skills::Evidence;
    /// Skills this session's agent created since the user's last message.
    fn skills_made_this_turn(&self, session_id: &str) -> usize;
    fn user_skills_dir(&self) -> PathBuf;
    /// The subagent types available in `cwd` (the project's, the user's,
    /// the built-ins).
    fn agent_defs(&self, cwd: &Path) -> Vec<crate::agents::AgentDef>;
    /// Where the user's own subagent types are kept (`agents define`).
    fn user_agents_dir(&self) -> PathBuf;
    /// This chat's configured helper ceilings (steps, minutes; u32::MAX: unlimited).
    fn helper_limits(&self, _session_id: &str) -> (u32, u32) {
        (40, 15)
    }
    /// A read-only helper's commands may use the network (its agent type
    /// says `network: true`).
    fn helper_network(&self, _session_id: &str) -> bool {
        false
    }
    fn ledger(&self, session_id: &str) -> Option<TaskLedger>;
    fn set_ledger(&self, session_id: &str, ledger: TaskLedger) -> anyhow::Result<()>;
    fn run_subagents(
        &self,
        parent: &str,
        call_id: &str,
        tasks: Vec<meta::SubTask>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Vec<crate::engine::SubResult>>;
    /// Start subagents in the background.
    fn spawn_subagents(
        &self,
        parent: &str,
        call_id: &str,
        tasks: Vec<meta::SubTask>,
    ) -> Result<Vec<meta::Started>, String>;
    fn subagent_status(&self, parent: &str) -> String;
    /// Stop background subagents (all, or one by name); returns how many.
    fn stop_subagents(&self, parent: &str, name: Option<&str>) -> usize;
    /// The full report of one of `parent`'s own subagents (name or session id).
    fn subagent_report(&self, parent: &str, which: &str) -> Result<String, String>;
    /// Continue one of `parent`'s finished subagents, or apply or discard an
    /// isolated one's changes.
    fn subagent_control(
        &self,
        parent: &str,
        action: meta::SubAction,
    ) -> BoxFuture<'static, Result<String, String>>;
    /// Create (no `id`) or update a whiteboard: a card in the chat, and
    /// with `tile` its tile beside the chat too.
    fn board_show(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: tessera_proto::BoardContent,
        tile: bool,
    ) -> Result<tessera_proto::Whiteboard, String>;
    /// The same; a new board's web page runs on `host` when given (`Some(None)`:
    /// this server), not wherever it would be put now.
    fn board_show_on(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: tessera_proto::BoardContent,
        tile: bool,
        _host: Option<Option<String>>,
    ) -> Result<tessera_proto::Whiteboard, String> {
        self.board_show(session_id, id, title, content, tile)
    }
    /// Ask the user through a whiteboard and wait for the answer (in the
    /// chat; `tile` also puts the board on screen).
    #[allow(clippy::too_many_arguments)]
    fn board_ask(
        &self,
        session_id: &str,
        id: Option<String>,
        title: Option<String>,
        prompt: String,
        input: tessera_proto::AskInput,
        tile: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<tessera_proto::AskAnswer, String>>;
    /// The whiteboards an agent here can see: this conversation's (including
    /// its subagents') and any the user put on screen next to it.
    fn board_list(&self, session_id: &str) -> Vec<board::BoardView>;
    /// `GET` a visible `Local` board's page the way its tile loads it.
    fn board_local_get(
        &self,
        session_id: &str,
        id: &str,
    ) -> BoxFuture<'static, Result<board::LocalPage, String>>;
    /// What a board's tile shows on a device right now (a snapshot one of
    /// them takes), within a few seconds.
    fn board_look(
        &self,
        session_id: &str,
        id: &str,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<tessera_proto::SnapshotImage, String>>;
    /// The notes the agent in `session_id` may see, newest first (a bot with
    /// its own memory: its knowledge and notes from its conversations).
    fn notes(&self, session_id: &str) -> Vec<tessera_proto::NoteInfo>;
    fn note(&self, session_id: &str, id: &str) -> Option<tessera_proto::Note>;
    /// Whether the agent in `session_id` may read `other`'s history (never a
    /// private chat or helper session; a bot with its own memory, only its
    /// own conversations).
    fn may_recall(&self, session_id: &str, other: &str) -> bool;
    /// Why `session_id` may not read `other` (which rule, in plain words).
    fn recall_refusal(&self, session_id: &str, other: &str) -> Option<String> {
        (!self.may_recall(session_id, other)).then(|| "You can't read that conversation.".into())
    }
    /// Create (`id` None) or replace a note as the agent of `session_id`; a
    /// stale `base_version` fails ("changed").
    fn note_save(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: &str,
        content: String,
        base_version: Option<u64>,
    ) -> Result<tessera_proto::Note, String>;
    /// Open a note's tile beside this chat.
    fn note_show(&self, session_id: &str, id: &str) -> Result<(), String>;
    /// A picture a note the agent may see uses (`assets/<name>`): its type and bytes.
    fn note_picture(
        &self,
        _session_id: &str,
        _note_id: &str,
        _name: &str,
    ) -> Option<(&'static str, Vec<u8>)> {
        None
    }
    /// Keep a picture for notes; its name for `![alt](assets/<name>)`.
    fn note_picture_put(&self, _session_id: &str, _bytes: &[u8]) -> Result<String, String> {
        Err("pictures in notes aren't available here".into())
    }
    /// The session's model takes images in tool results.
    fn sees_images(&self, _session_id: &str) -> bool {
        false
    }
    fn board_delete(&self, session_id: &str, id: &str) -> Result<(), String>;
    /// Put this conversation's board `id` back on screen (its tile beside
    /// the chat) when the user closed it.
    fn board_tile(&self, _session_id: &str, _id: &str) -> Result<(), String> {
        Err("whiteboards aren't available here".into())
    }
    /// Save a signature (base64 PNG) where this chat's tools can read it.
    fn board_save_signature(&self, session_id: &str, png: &str) -> Result<PathBuf, String>;
    fn jobs(&self) -> Vec<tessera_proto::Job>;
    fn save_job(&self, job: tessera_proto::Job) -> anyhow::Result<tessera_proto::Job>;
    fn delete_job(&self, id: &str) -> anyhow::Result<bool>;
    /// From a scheduled job's run: its goal is met, end its schedule.
    fn end_job_schedule(&self, _session_id: &str, _reason: &str) -> Result<String, String> {
        Err("scheduled jobs aren't available here".into())
    }
    fn web_config(&self) -> crate::config::WebConfig;
    /// Headers for the search backend (credentials; never shown to models).
    fn search_headers(&self) -> Vec<(String, String)>;
    fn http(&self) -> reqwest::Client;
    fn secrets(&self) -> Vec<String>;
    /// The stored secrets `command` names that the agent of `session_id` may
    /// use at `place` (who can use each and where are checked here, for
    /// every engine), and the environment for exactly those.
    fn secret_env(
        &self,
        session_id: &str,
        command: &str,
        place: crate::auth::SecretPlace,
    ) -> crate::auth::SecretUse;
    fn mark_secrets_used(&self, names: &[String]);
    /// `bot` (id or handle) may use stored secrets the agent of
    /// `session_id` may not (asking it is then a way to use them).
    fn bot_has_own_secrets(&self, _session_id: &str, _bot: &str) -> bool {
        false
    }
    /// Whether this session may hand work to bots (`message_bot`): a
    /// bot's always may (its own list decides whom); an ordinary chat as
    /// the user set (Settings → Privacy & safety).
    fn bot_handoff(&self, _session_id: &str) -> tessera_proto::BotHandoff {
        tessera_proto::BotHandoff::On
    }
    /// What no tool may read or touch (Tessera's own credentials and settings).
    fn protected_paths(&self) -> Protected;
    /// Where file tools may write without asking (Settings → Privacy & safety).
    fn write_access(&self) -> WriteAccess {
        WriteAccess::default()
    }
    /// Managed helper cwd authority: verified object and original canonical
    /// location, never fresh authority obtained by following its pathname.
    fn authorized_cwd(&self, _session_id: &str) -> Result<Option<PathBuf>, String> {
        Ok(None)
    }
    /// Clone the verified helper identity before entering a filesystem commit
    /// gate; checking this value must not reacquire host/state locks.
    fn authorized_cwd_scope(
        &self,
        _session_id: &str,
    ) -> Result<Option<file_object::DirectoryScope>, String> {
        Ok(None)
    }
    /// Pin the helper's original cwd object, then any command-selected
    /// subfolder. Ordinary chats have no persisted helper identity to check.
    fn process_directory(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<file_object::ProcessDirectory, String> {
        self.authorized_cwd(session_id)?;
        file_object::ProcessDirectory::open(cwd)
    }
    /// How to confine shell commands: what to hide, and whether running
    /// without the sandbox is refused. `None` = don't sandbox.
    /// Automatic cannot safely confine commands on this host. Every
    /// command needs one exact, single-use approval until explicit Off.
    fn unconfined_commands_ask(&self) -> bool {
        false
    }
    fn sandbox(&self) -> Option<(crate::sandbox::Spec, bool)>;
    /// The same for the commands of one chat (and its subagents): only its
    /// own attached files, and its own process namespace where commands
    /// share one.
    fn session_sandbox(&self, _session_id: &str) -> Option<(crate::sandbox::Spec, bool)> {
        self.sandbox()
    }
    /// `path` holds files attached to another chat.
    fn other_chats_upload(&self, _session_id: &str, _path: &Path) -> bool {
        false
    }
    fn bash_timeout(&self) -> u64;
    /// Explicit user-selected environment names, never values.
    fn pass_env(&self) -> Vec<String> {
        Vec::new()
    }
    /// Syntax checks after `write`/`edit` with installed tools
    /// (`tools/quick.rs`, `agent.quick_checks`).
    fn quick_checks(&self) -> bool {
        false
    }
    fn max_tool_output(&self) -> usize;
    /// Bytes of one result the model in `session_id` is shown: less than
    /// `max_tool_output` for small context windows (`context::tool_output_limit`).
    fn tool_output_limit(&self, _session_id: &str) -> usize {
        self.max_tool_output()
    }
    /// Permission mode the acting session runs with.
    fn permission(&self, session_id: &str) -> PermissionMode;
    /// Permission mode a bot (by id or handle) runs with.
    fn bot_permission(&self, bot: &str) -> Option<PermissionMode>;
    /// API key of a search provider (`brave`, `tavily`); never shown to models.
    fn search_key(&self, _provider: &str) -> Option<String> {
        None
    }
    /// Whether several [`utility_complete`](Self::utility_complete) calls
    /// may run at once (a hosted model; not a server at home with one slot).
    fn utility_parallel(&self, _session_id: &str) -> bool {
        false
    }
    /// Small model mode or a lean start: turn the session's other tools on
    /// from its next step. `Some(true)`: turned on now; `Some(false)`: they
    /// were on already; `None`: the session is in neither.
    fn more_tools(&self, _session_id: &str) -> Option<bool> {
        None
    }
    /// One tool-less completion on the session's utility model (a private
    /// chat's own model), e.g. to extract an answer from a long page.
    fn utility_complete(
        &self,
        _session_id: &str,
        _system: String,
        _prompt: String,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Err("no model available".to_string()) })
    }
    /// Language servers (`None` when `[lsp] enabled = false`).
    fn lsp(&self) -> Option<Arc<crate::lsp::LspManager>> {
        None
    }
    /// What each session was shown of each file: edit rebase and the
    /// seen-lines rule (`None` disables both).
    fn file_cache(&self) -> Option<&filecache::FileCache> {
        None
    }
    /// The session's user-turn counter (caps harness notes per turn).
    fn turn(&self, _session_id: &str) -> u32 {
        0
    }
    /// The browsers web views run in (this server's and machines'; web
    /// view tabs and the `browser` tool).
    fn browser(&self) -> Option<crate::browser::Browsers> {
        None
    }
    /// Show a web page in a tile beside the chat whose browser runs on
    /// `host` (a machine's id; none: this server), already checked with
    /// `Browsers::place`.
    fn web_board(
        &self,
        _session_id: &str,
        _title: Option<&str>,
        _url: &str,
        _host: Option<String>,
    ) -> Result<tessera_proto::Whiteboard, String> {
        Err("whiteboards aren't available here".into())
    }
    /// Whether `save_secret` may store this, and how the user wants
    /// credentials from chats saved (asked first, or this chat only).
    fn check_chat_secret(
        &self,
        _session_id: &str,
        _secret: &secrets::ChatSecret,
    ) -> Result<crate::config::SecretSaving, String> {
        Err("saving secrets is not available here".into())
    }
    /// Move a credential the user typed in this chat into the secret store
    /// (or this chat's own, `chat_only`) and out of the stored conversation
    /// (`save_secret`).
    fn save_chat_secret(
        &self,
        _session_id: &str,
        _secret: secrets::ChatSecret,
    ) -> Result<String, String> {
        Err("saving secrets is not available here".into())
    }
    /// The saved passwords and keys `session_id` may use, as its prompt
    /// would list them all (names and variables, never values): the
    /// `list` action of `save_secret`.
    fn list_secrets(&self, _session_id: &str) -> String {
        "No saved passwords or keys.".into()
    }
    /// Tessera's settings for the `settings` tool (no credentials), or a
    /// change to one after the user approved it.
    fn settings(&self, _session_id: &str) -> Result<String, String> {
        Err("settings are not available here".into())
    }
    fn change_setting(
        &self,
        _session_id: &str,
        _key: &[String],
        _value: &Value,
    ) -> Result<String, String> {
        Err("settings are not available here".into())
    }
    /// Ask the user about something a running call is about to do (found
    /// only while running, e.g. a password field). The permission mode and
    /// "allow for this session" (`key`) apply as for a tool's own risk;
    /// `always` asks even in auto mode. True if allowed.
    fn approve(&self, _req: Approve) -> BoxFuture<'static, bool> {
        Box::pin(async { false })
    }
    /// Like `approve`, retaining why it was not approved for the tool result.
    fn approve_reason(&self, req: Approve) -> BoxFuture<'static, Result<(), String>> {
        let tool = req.tool.clone();
        let answer = self.approve(req);
        Box::pin(async move {
            if answer.await {
                Ok(())
            } else {
                // A boolean-only host has no evidence the user tapped Deny.
                Err(crate::approval::Outcome::Dismissed.message(&tool))
            }
        })
    }
    /// Send (or schedule) a notification to the user's devices (`notify`).
    fn notify(&self, _session_id: &str, _n: notify::Outgoing) -> Result<notify::Sent, String> {
        Err("notifications are not available here".into())
    }
    /// Cancel a scheduled notification.
    fn cancel_notification(&self, _session_id: &str, _id: u64) -> Result<(), String> {
        Err("notifications are not available here".into())
    }
    /// Copy a file into the chat as a download (`share_file`; the tool has
    /// checked the path). Blocking: call it off the async runtime.
    /// `held_ok`: the user agreed to share a file holding their saved
    /// passwords or keys ([`share::Refusal::Holds`]).
    fn share_file(
        &self,
        _session_id: &str,
        _path: &Path,
        _file: &std::fs::File,
        _name: &str,
        _held_ok: bool,
    ) -> Result<tessera_proto::SharedFile, share::Refusal> {
        Err(share::Refusal::Failed(
            "sharing files is not available here".into(),
        ))
    }
    /// The user's machines (`machines` tool).
    fn machines(&self) -> Vec<tessera_proto::MachineInfo> {
        Vec::new()
    }
    /// Run one action on a machine and wait for its answer.
    fn machine_call(
        &self,
        _call: machines::MachineCall,
    ) -> BoxFuture<'static, Result<tessera_proto::NodeResult, String>> {
        Box::pin(async { Err("machines are not available here".to_string()) })
    }
    /// May this session use the machine (its "who can use it": any chat,
    /// or only certain bots), and how far (a bot's own access: read only,
    /// or as the machine allows)? `Err` says why not.
    fn machine_access(
        &self,
        _session_id: &str,
        _machine: &tessera_proto::MachineInfo,
    ) -> Result<tessera_proto::MachineAccess, String> {
        Err("machines are not available here".into())
    }
    /// A machine `bot` may do more on than this session's own bot, by name
    /// (asking that bot is a way to act there).
    fn bot_more_machines(&self, _session_id: &str, _bot: &str) -> Option<String> {
        None
    }
    /// Why nothing more runs for this session's turn: the user stopped
    /// Tessera on the computer this server runs on since it began (an
    /// approval answered meanwhile allows nothing).
    fn stopped_since_turn(&self, _session_id: &str) -> Option<String> {
        None
    }
    /// Tell a machine about an action refused (or declined) before it was
    /// sent, for its own log.
    fn machine_refused(
        &self,
        _session_id: &str,
        _machine: &str,
        _action: tessera_proto::NodeAction,
        _reason: &str,
    ) {
    }
}

/// A question for [`ToolHost::approve`].
pub struct Approve {
    pub session_id: String,
    pub call_id: String,
    pub tool: String,
    /// One line: what is about to happen.
    pub summary: String,
    pub reason: String,
    /// What "allow for this session" covers.
    pub key: String,
    pub always: bool,
    /// Asked every time, whatever was allowed for the session before, and
    /// never remembered (a machine that asks before every action, or before
    /// risky ones: its own rule, not the chat's).
    pub once: bool,
    pub cancel: CancellationToken,
    /// Shown with the approval (`ApprovalRequest::arguments`).
    pub arguments: Value,
}

/// The language servers and this session's standing with them, when on.
pub fn lsp_env(ctx: &ToolCtx) -> Option<(Arc<crate::lsp::LspManager>, crate::lsp::SessionEnv)> {
    let lsp = ctx.host.lsp()?;
    let may_start = lsp.may_start(ctx.host.permission(&ctx.session_id));
    let env = crate::lsp::SessionEnv {
        session: ctx.session_id.clone(),
        cwd: ctx.cwd.clone(),
        turn: ctx.host.turn(&ctx.session_id),
        sandbox: ctx.host.sandbox(),
        may_start,
    };
    Some((lsp, env))
}

/// Append diagnostics that arrived after an earlier edit's result.
pub fn attach_late_diagnostics(ctx: &ToolCtx, out: &mut ToolOutput) {
    let Some(lsp) = ctx.host.lsp() else { return };
    let turn = ctx.host.turn(&ctx.session_id);
    if let Some(text) = lsp.take_late(&ctx.session_id, turn, &ctx.cwd) {
        out.content.push(ContentPart::text(text));
    }
}

/// How much a permission mode lets through without asking.
pub fn permission_rank(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::ReadOnly => 0,
        PermissionMode::Ask => 1,
        PermissionMode::Default => 2,
        PermissionMode::Auto => 3,
    }
}

/// `mode`, but never more than `ceiling`: work a session starts or hands off
/// must not run with more freedom than the session itself has.
pub fn cap_permission(mode: PermissionMode, ceiling: PermissionMode) -> PermissionMode {
    if permission_rank(mode) <= permission_rank(ceiling) {
        mode
    } else {
        ceiling
    }
}

pub fn permission_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read-only",
        PermissionMode::Ask => "ask",
        PermissionMode::Default => "default",
        PermissionMode::Auto => "auto",
    }
}

#[derive(Clone)]
pub struct ToolCtx {
    pub session_id: String,
    pub call_id: String,
    /// Seq of the assistant entry that requested this call.
    pub seq: u64,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub host: Arc<dyn ToolHost>,
    pub progress: Arc<dyn Fn(&str) + Send + Sync>,
    /// Subagents may not spawn further subagents or change schedules.
    pub depth: u8,
    /// The approval key (`Risk::Ask`) this call was cleared for, by the user
    /// or the permission mode, so `run` can check it still covers the target.
    pub cleared: Option<String>,
    /// Captured before approval/queue waits; checked at each side effect.
    pub admission: admission::ActionAdmission,
}

impl ToolCtx {
    /// Recheck durable mutations in the host-side implementation too, not
    /// just while dispatching. Session bookkeeping deliberately does not use it.
    pub(crate) fn authorize_mutation(&self, tool: &dyn Tool, args: &Value) -> Result<(), String> {
        if tool.action_kind(args) != ToolKind::Write {
            return Ok(());
        }
        match permission_risk(tool, args, self, self.host.permission(&self.session_id)) {
            Risk::Block { reason } => return Err(reason),
            Risk::Ask { key, .. } if self.cleared.as_deref() != Some(&key) => {
                return Err("This change needs approval. Submit the exact change again.".into());
            }
            _ => {}
        }
        self.admission.check()
    }

    pub fn resolve(&self, p: &str) -> PathBuf {
        let p = crate::util::expand_home(p.trim());
        if p.is_absolute() { p } else { self.cwd.join(p) }
    }

    /// Path shown back to the model: relative to cwd when inside it.
    pub fn display(&self, p: &Path) -> String {
        p.strip_prefix(&self.cwd)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| p.display().to_string())
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> String;
    fn schema(&self) -> Value;
    fn kind(&self) -> ToolKind;
    /// A mixed tool may offer read actions while keeping mutating actions
    /// behind the same command/write permission floor.
    fn action_kind(&self, _args: &Value) -> ToolKind {
        self.kind()
    }
    /// Safe to run concurrently with other calls in the same batch.
    fn concurrent(&self, _args: &Value) -> bool {
        self.kind() == ToolKind::Read
    }
    /// Policy check (dangerous commands, writes outside the workspace…).
    fn assess(&self, _args: &Value, _ctx: &ToolCtx) -> Risk {
        Risk::None
    }
    /// One-line human summary for UIs and approvals.
    fn summary(&self, args: &Value) -> String {
        args.as_object()
            .and_then(|o| {
                o.values()
                    .find_map(|v| v.as_str().map(|s| crate::util::one_line(s, 80)))
            })
            .unwrap_or_default()
    }
    /// What an approval shows: everything the call would do, where the
    /// one-line summary leaves part of it out (a command's other lines).
    fn approval_summary(&self, args: &Value) -> String {
        self.summary(args)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput>;
}

pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    pub fn builtin() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(files::Read),
            Arc::new(files::Write),
            Arc::new(files::Edit),
            Arc::new(search::Grep),
            Arc::new(search::Glob),
            Arc::new(bash::Bash),
            Arc::new(bash::ReadOnlyBash),
            Arc::new(bash::Job),
            Arc::new(web::WebFetch),
            Arc::new(web::WebSearch),
            Arc::new(meta::Task),
            Arc::new(meta::MemoryTool),
            Arc::new(meta::SkillView),
            Arc::new(meta::SkillManage),
            Arc::new(meta::Recall),
            Arc::new(meta::Agent),
            Arc::new(meta::Agents),
            Arc::new(meta::Schedule),
            Arc::new(meta::MessageBot),
            Arc::new(board::Whiteboard),
            Arc::new(browser::BrowserTool),
            Arc::new(note::Note),
            Arc::new(notify::Notify),
            Arc::new(share::ShareFile),
            Arc::new(docs::Docs),
            Arc::new(secrets::SaveSecret),
            Arc::new(settings::Settings),
            // Offered only once the server knows a machine.
            Arc::new(machines::Machines),
            // Offered only while a machine that can do it has desktop control on.
            Arc::new(desktop::Desktop),
            // Offered only where a language server matched the session's
            // directory (see `Inner::tool_specs`).
            Arc::new(lsp::Lsp),
        ];
        Self { tools }
    }

    pub fn add(&mut self, tool: Arc<dyn Tool>) {
        self.tools.retain(|t| t.name() != tool.name());
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    /// Tools offered to the model for a session. The set is fixed for the
    /// session's lifetime so provider prompt caches stay valid.
    pub fn specs(&self, mode: PermissionMode, depth: u8, memory: bool) -> Vec<ToolSpec> {
        self.available(mode, depth, memory)
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description(),
                parameters: t.schema(),
            })
            .collect()
    }

    pub fn available(
        &self,
        mode: PermissionMode,
        depth: u8,
        memory: bool,
    ) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter().filter(move |t| {
            if mode == PermissionMode::ReadOnly
                && matches!(t.kind(), ToolKind::Write | ToolKind::Exec)
            {
                return false;
            }
            if depth > 0
                && matches!(
                    t.name(),
                    "agent"
                        | "agents"
                        | "schedule"
                        | "memory"
                        | "skill_manage"
                        | "save_secret"
                        | "settings"
                )
            {
                return false;
            }
            if !memory && t.name() == "memory" {
                return false;
            }
            // Read-only helpers' command runner, in place of `bash`.
            if t.name() == bash::READ_ONLY_BASH && (mode != PermissionMode::ReadOnly || depth == 0)
            {
                return false;
            }
            // The restricted helper runner cannot start kept jobs; do not
            // advertise a controller for work it cannot own.
            if mode == PermissionMode::ReadOnly && depth > 0 && t.name() == "job" {
                return false;
            }
            true
        })
    }
}

/// Required string argument.
pub fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolOutput::error(format!("Missing required string argument `{key}`.")))
}

pub fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

pub fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key)
        .and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
}

/// Tessera's own credential and settings locations, with the exceptions
/// inside them that tools may still use (e.g. the skills directory).
#[derive(Debug, Clone, Default)]
pub struct Protected {
    pub roots: Vec<PathBuf>,
    pub except: Vec<PathBuf>,
}

impl Protected {
    /// `path` is inside a protected location (and not an exception).
    pub fn covers(&self, path: &Path) -> bool {
        self.covers_on(path, cfg!(windows))
    }

    fn covers_on(&self, path: &Path, windows: bool) -> bool {
        let under = |root: &Path, fold_case| {
            if windows {
                let norm = |p: &Path| {
                    crate::platform::windows_policy_path(&p.to_string_lossy(), fold_case)
                };
                Path::new(&norm(path)).starts_with(norm(root))
            } else {
                path.starts_with(root)
            }
        };
        // Protect all case spellings on Windows, but never widen exceptions:
        // Windows can opt a directory into case-sensitive names.
        self.roots.iter().any(|r| under(r, true)) && !self.except.iter().any(|e| under(e, false))
    }

    /// A directory walk should not enter `dir` (nothing allowed is inside).
    pub fn skips(&self, dir: &Path) -> bool {
        self.covers(dir) && !self.except.iter().any(|e| e.starts_with(dir))
    }

    /// The same locations with symlinks resolved, to compare against
    /// canonical paths.
    pub fn canonical(&self) -> Self {
        Self {
            roots: self.roots.iter().map(|p| canonical(p)).collect(),
            except: self.except.iter().map(|p| canonical(p)).collect(),
        }
    }

    /// A shell command line that names a protected location.
    pub fn mentioned_in(&self, cmd: &str) -> bool {
        if cfg!(windows) {
            let vars: Vec<(&str, String)> = ["APPDATA", "LOCALAPPDATA", "USERPROFILE"]
                .into_iter()
                .filter_map(|v| Some((v, std::env::var(v).ok().filter(|s| !s.is_empty())?)))
                .collect();
            return self.mentioned_in_windows(cmd, &vars);
        }
        let home = dirs::home_dir();
        let forms = |p: &Path| {
            let mut v = vec![p.to_string_lossy().to_string()];
            if let Some(r) = home.as_ref().and_then(|h| p.strip_prefix(h).ok()) {
                v.push(format!("~/{}", r.display()));
                v.push(format!("$HOME/{}", r.display()));
                v.push(format!("${{HOME}}/{}", r.display()));
            }
            v
        };
        let mut text = cmd.to_string();
        for e in &self.except {
            for f in forms(e) {
                // A readable subtree is not permission to traverse back
                // into its protected parent. Do not erase such spellings.
                let mut at = 0;
                while let Some(i) = text[at..].find(&f) {
                    let start = at + i;
                    let end = start + f.len();
                    let tail = text[end..]
                        .split(|c: char| c.is_whitespace() || "\"';|&()<>=`".contains(c))
                        .next()
                        .unwrap_or("");
                    if tail.split('/').any(|part| part == "..") {
                        at = end;
                    } else {
                        text.replace_range(start..end, "");
                        at = start;
                    }
                }
            }
        }
        // The variable itself is withheld from commands; using it is still
        // a sign of reaching for the store (naming it, e.g. in a grep, is not).
        text.contains("$TESSERA_HOME")
            || text.contains("${TESSERA_HOME")
            || self
                .roots
                .iter()
                .any(|r| forms(r).iter().any(|f| names_path(&text, f)))
    }
}

impl Protected {
    /// [`mentioned_in`](Self::mentioned_in) on Windows, where paths ignore
    /// letter case, take either slash, and come in many spellings: `C:\…`,
    /// `C:/…`, Git Bash's `/c/…`, `%APPDATA%\…`, `$env:APPDATA\…`, `~\…`.
    /// `vars`: the folders those variables name (APPDATA, LOCALAPPDATA,
    /// USERPROFILE).
    pub fn mentioned_in_windows(&self, cmd: &str, vars: &[(&str, String)]) -> bool {
        let norm = |s: &str| s.to_lowercase().replace('/', "\\");
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_lowercase(), norm(v).trim_end_matches('\\').to_string()))
            .collect();
        let forms = |p: &Path| {
            let full = norm(&p.to_string_lossy());
            let mut v = vec![full.clone()];
            let b = full.as_bytes();
            if b.len() > 2 && b[1] == b':' {
                // Git Bash: /c/Users/…
                v.push(format!("\\{}{}", &full[..1], &full[2..]));
            }
            for (k, base) in &vars {
                if let Some(rest) = full
                    .strip_prefix(base.as_str())
                    .filter(|r| r.starts_with('\\'))
                {
                    v.push(format!("%{k}%{rest}"));
                    v.push(format!("$env:{k}{rest}"));
                    v.push(format!("${{env:{k}}}{rest}"));
                    if k == "userprofile" {
                        v.push(format!("~{rest}"));
                        v.push(format!("$home{rest}"));
                    }
                }
            }
            v
        };
        let mut text = norm(cmd);
        for e in &self.except {
            for f in forms(e) {
                text = text.replace(&f, "");
            }
        }
        text.contains("$env:tessera_home")
            || text.contains("%tessera_home%")
            || text.contains("$tessera_home")
            || self
                .roots
                .iter()
                .any(|r| forms(r).iter().any(|f| names_path(&text, f)))
    }
}

/// `text` mentions `path` itself (or something inside it), not a sibling
/// that merely starts the same (`~/.local/share/tessera-toolchain`).
fn names_path(text: &str, path: &str) -> bool {
    text.match_indices(path).any(|(i, _)| {
        !text[i + path.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
    })
}

/// Where file tools may write (Settings → Privacy & safety).
#[derive(Debug, Clone)]
pub struct WriteAccess {
    /// Folder rules (canonical paths): `true` = writable without asking,
    /// `false` = read-only. The most specific one applies.
    pub folders: Vec<(PathBuf, bool)>,
    /// Ask before writing outside the working directory and `folders`.
    pub ask_outside: bool,
}

impl Default for WriteAccess {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            ask_outside: true,
        }
    }
}

impl WriteAccess {
    /// The rule of the most specific folder holding `path` (canonical).
    pub fn rule(&self, path: &Path) -> Option<bool> {
        self.folders
            .iter()
            .filter(|(f, _)| path.starts_with(f))
            .max_by_key(|(f, _)| f.components().count())
            .map(|(_, w)| *w)
    }
}

/// Replace known secret values, and anything that looks like a credential,
/// before tool output can reach a model.
pub fn redact(text: &str, secrets: &[String]) -> String {
    scrub_patterns(&redact_known(text, secrets))
}

/// Replace only the values Tessera knows are secret.
///
/// Values of [`WHOLE_TOKEN_BELOW`] characters or fewer are replaced only
/// where they stand as a whole token: a short PIN or password must not eat
/// parts of ordinary words or numbers ("anon" stays in "anonymous"). Longer
/// values (keys, tokens, passwords and their encodings) are replaced
/// wherever they appear, even glued to other text.
pub fn redact_known(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    // Longest first, so a value that contains another is still caught whole.
    let mut secrets: Vec<&String> = secrets.iter().filter(|s| !s.is_empty()).collect();
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    for s in secrets {
        if !out.contains(s.as_str()) {
            continue;
        }
        out = if s.chars().count() < WHOLE_TOKEN_BELOW {
            replace_whole_tokens(&out, s, "[REDACTED]").unwrap_or(out)
        } else {
            out.replace(s.as_str(), "[REDACTED]")
        };
    }
    out
}

/// Known secret values shorter than this are matched as whole tokens only.
/// Encoded forms are never this short (see `auth::encoded_forms`).
pub const WHOLE_TOKEN_BELOW: usize = 8;

/// `text` with every whole-token occurrence of `value` replaced by `with`,
/// or `None` if there is none. A value that starts (ends) with a letter,
/// digit or `_` must not continue a longer word on that side.
pub fn replace_whole_tokens(text: &str, value: &str, with: &str) -> Option<String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if value.is_empty() {
        return None;
    }
    let first = value.chars().next().is_some_and(word);
    let last = value.chars().last().is_some_and(word);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut changed = false;
    for (i, _) in text.match_indices(value) {
        if i < at {
            continue;
        }
        let before = text[..i].chars().last();
        let after = text[i + value.len()..].chars().next();
        if (first && before.is_some_and(word)) || (last && after.is_some_and(word)) {
            continue;
        }
        out.push_str(&text[at..i]);
        out.push_str(with);
        at = i + value.len();
        changed = true;
    }
    out.push_str(&text[at..]);
    changed.then_some(out)
}

/// High-confidence credential formats. Values are replaced; names are kept so
/// the agent still understands the structure of a config file.
pub fn scrub_patterns(text: &str) -> String {
    use std::sync::OnceLock;
    static RES: OnceLock<[regex::Regex; 4]> = OnceLock::new();
    let [private_key, tokens, jwt, assign] = RES.get_or_init(|| {
        [
            regex::Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----").unwrap(),
            regex::Regex::new(&secrets::token_pattern()).unwrap(),
            regex::Regex::new(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b").unwrap(),
            regex::Regex::new(
                r#"(?im)^(\s*(?:export\s+)?["']?[A-Za-z0-9_.-]*(?:secret|token|passw(?:or)?d|pwd|api[_-]?key|apikey|private[_-]?key|access[_-]?key|client[_-]?secret|auth)[A-Za-z0-9_.-]*["']?\s*[=:]\s*)(["']?(?:(?:Bearer|Basic|Token)\s+)?)([^\s"'#]{6,})"#,
            )
            .unwrap(),
        ]
    });
    let out = private_key.replace_all(text, "[REDACTED PRIVATE KEY]");
    let out = tokens.replace_all(&out, "[REDACTED]");
    let out = jwt.replace_all(&out, "[REDACTED]");
    let out = assign.replace_all(&out, |c: &regex::Captures| {
        let v = &c[3];
        if v.contains("REDACTED")
            || v.starts_with('$')
            || v.starts_with("${")
            || v == "true"
            || v == "false"
            || v.chars().all(|ch| ch.is_ascii_digit())
        {
            c[0].to_string()
        } else {
            format!("{}{}[REDACTED]", &c[1], &c[2])
        }
    });
    out.into_owned()
}

/// Paths that hold credentials of other programs: reading them needs approval.
pub fn credential_file(p: &std::path::Path) -> bool {
    credential_file_on(p, cfg!(windows), dirs::home_dir().as_deref())
}

fn credential_file_on(p: &Path, windows: bool, home: Option<&Path>) -> bool {
    let normalize = |p: &Path| {
        let s = p.to_string_lossy();
        if windows {
            crate::platform::windows_policy_path(&s, true)
        } else {
            s.into_owned()
        }
    };
    let s = normalize(p);
    let normalized = Path::new(&s);
    let name = normalized
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    (s.contains("/.ssh/") && (name.starts_with("id_") && !name.ends_with(".pub")))
        || s.ends_with("/.aws/credentials")
        || s.ends_with("/.netrc")
        || s.ends_with("/.config/gh/hosts.yml")
        || s.ends_with("/.docker/config.json")
        || s.ends_with("/.kube/config")
        || s.contains("/.gnupg/")
        || s.ends_with("/.git-credentials")
        || s.ends_with("/.codex/auth.json")
        || s.contains("/.password-store/")
        // Claude Code's sign-in: `~/.claude`'s, and each other account's own
        // folder's (`CLAUDE_CONFIG_DIR`).
        || name == ".credentials.json"
        || s.ends_with("/.pypirc")
        || s.ends_with("/.pgpass")
        || s.ends_with("/.cargo/credentials.toml")
        || s.ends_with("/.cargo/credentials")
        || s.contains("/.config/rclone/")
        // A project's own .npmrc usually holds settings; the one in the
        // home folder holds registry tokens.
        || (windows && (s.ends_with("/_netrc") || s.ends_with("/gh/hosts.yml") || s.ends_with("/rclone/rclone.conf")))
        || (name == ".npmrc"
            && home.is_some_and(|h| normalized.parent() == Some(Path::new(&normalize(h)))))
}

/// Claude's login is never an agent capability. Check obvious spellings
/// before resolving paths, without opening anything in its account folder.
pub fn claude_state_path(path: &Path) -> bool {
    claude_state_path_on(path, cfg!(windows), dirs::home_dir().as_deref())
}

pub(crate) fn claude_state_path_on(path: &Path, windows: bool, home: Option<&Path>) -> bool {
    let normalize = |p: &Path| {
        if windows {
            PathBuf::from(crate::platform::windows_policy_path(
                &p.to_string_lossy(),
                true,
            ))
        } else {
            p.to_path_buf()
        }
    };
    let path = normalize(path);
    path.file_name().is_some_and(|n| n == ".credentials.json")
        || home.is_some_and(|home| {
            path.strip_prefix(normalize(home)).is_ok_and(|p| {
                p.components()
                    .next()
                    .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with(".claude"))
            })
        })
}

/// Blocks tools from the harness's own credential store and asks before
/// other credential files are read.
pub fn path_risk(path: &std::path::Path, ctx: &ToolCtx) -> Risk {
    if claude_state_path(path) {
        return Risk::Block { reason: "Refused: Claude Code owns its sign-in and account files; Tessera tools cannot access them.".into() };
    }
    let canon = canonical(path);
    if claude_state_path(&canon) {
        return Risk::Block { reason: "Refused: Claude Code owns its sign-in and account files; Tessera tools cannot access them.".into() };
    }
    let protected = ctx.host.protected_paths();
    if protected.covers(path) || protected.canonical().covers(&canon) {
        return Risk::Block {
            reason: "Refused: that is Tessera's own credential, session and settings store. Use the `settings` tool for its settings.".into(),
        };
    }
    if ctx.host.other_chats_upload(&ctx.session_id, &canon) {
        return Risk::Block {
            reason: "Refused: those are files the user attached to another chat.".into(),
        };
    }
    if credential_file(&canon) {
        return Risk::Ask {
            reason: format!("reads a credential file: {}", canon.display()),
            key: format!("cred:{}", canon.display()),
        };
    }
    Risk::None
}

/// An opened object's resolved location. Do not resolve its name again: a
/// replaced symlink is a different object and cannot change this authority.
pub(crate) fn opened_path_risk(path: &Path, ctx: &ToolCtx) -> Risk {
    risk_for_opened_location(
        path,
        &ctx.host.protected_paths(),
        ctx.host.other_chats_upload(&ctx.session_id, path),
    )
}

fn risk_for_opened_location(path: &Path, protected: &Protected, other_upload: bool) -> Risk {
    if claude_state_path(path) {
        return Risk::Block { reason: "Refused: Claude Code owns its sign-in and account files; Tessera tools cannot access them.".into() };
    }
    if protected.covers(path) || protected.canonical().covers(path) {
        return Risk::Block { reason: "Refused: that is Tessera's own credential, session and settings store. Use the `settings` tool for its settings.".into() };
    }
    if other_upload {
        return Risk::Block {
            reason: "Refused: those are files the user attached to another chat.".into(),
        };
    }
    if credential_file(path) {
        return Risk::Ask {
            reason: format!("reads a credential file: {}", path.display()),
            key: format!("cred:{}", path.display()),
        };
    }
    Risk::None
}
/// Canonical form of a path that may not exist yet (resolves the longest
/// existing prefix, so symlinked parents can't dodge the checks).
pub fn canonical(path: &Path) -> PathBuf {
    if let Ok(c) = crate::platform::canonicalize(path) {
        return c;
    }
    let mut rest = Vec::new();
    let mut cur = path.to_path_buf();
    while let Some(name) = cur.file_name().map(|n| n.to_os_string()) {
        rest.push(name);
        if !cur.pop() {
            break;
        }
        if let Ok(c) = crate::platform::canonicalize(&cur) {
            let mut out = c;
            for n in rest.iter().rev() {
                out.push(n);
            }
            return out;
        }
    }
    path.to_path_buf()
}

/// Store oversized output as an artifact and return a head/tail view.
pub fn fit_output(text: &str, ctx: &ToolCtx, label: &str) -> (String, Option<String>) {
    // Artifacts are persisted before the engine's final output pass.
    let clean = redact_known(text, &ctx.host.secrets());
    let text = clean.as_str();
    let max = ctx.host.tool_output_limit(&ctx.session_id);
    if text.len() <= max {
        return (text.to_string(), None);
    }
    let artifact = ctx
        .host
        .store()
        .put_artifact(Some(&ctx.session_id), label, text.as_bytes())
        .ok();
    let (view, _) = crate::util::head_tail(text, max, 0.3);
    let note = match &artifact {
        Some(id) => format!(
            "\n\n[Output truncated. Full output ({} bytes) saved as artifact `{id}`; use recall with artifact=\"{id}\" and offset/grep to see more.]",
            text.len()
        ),
        None => String::new(),
    };
    (format!("{view}{note}"), artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn opened_location_authority_does_not_follow_a_replaced_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".netrc");
        let other = dir.path().join("ordinary.txt");
        std::fs::write(&path, "checked notes").unwrap();
        std::fs::write(&other, "ordinary notes").unwrap();
        let opened = file_object::Opened::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(matches!(
            risk_for_opened_location(&opened.path, &Protected::default(), false),
            Risk::Ask { .. }
        ));
        assert!(matches!(
            risk_for_opened_location(&opened.path, &Protected::default(), true),
            Risk::Block { .. }
        ));
        let protected = Protected {
            roots: vec![opened.path.clone()],
            except: Vec::new(),
        };
        assert!(matches!(
            risk_for_opened_location(&opened.path, &protected, false),
            Risk::Block { .. }
        ));
        assert_ne!(canonical(&path), opened.path);
    }
    #[test]
    fn windows_protected_roots_cannot_hide_behind_case_or_long_path_prefixes() {
        let protected = Protected {
            roots: vec![PathBuf::from(r"C:\Users\Ann\AppData\Roaming\tessera")],
            except: vec![PathBuf::from(
                r"C:\Users\Ann\AppData\Roaming\tessera\skills",
            )],
        };
        for path in [
            r"\\?\C:\Users\Ann\AppData\Roaming\tessera\auth.json",
            r"\\.\c:\users\ANN\APPDATA\roaming\TESSERA\auth.json",
        ] {
            assert!(protected.covers_on(Path::new(path), true), "{path}");
        }
        let long = format!(
            r"\\?\C:\Users\Ann\AppData\Roaming\tessera\{}\secret",
            "x".repeat(300)
        );
        assert!(protected.covers_on(Path::new(&long), true));
        assert!(!protected.covers_on(
            Path::new(r"\\?\C:\Users\Ann\AppData\Roaming\tessera\skills\test.md"),
            true
        ));
        assert!(
            protected.covers_on(
                Path::new(r"C:\Users\Ann\AppData\Roaming\tessera\SKILLS\secret"),
                true
            ),
            "never widen a case-sensitive exception"
        );
        assert!(!protected.covers_on(
            Path::new(r"C:\Users\Ann\AppData\Roaming\tessera-other\file"),
            true
        ));
        let unc = Protected {
            roots: vec![PathBuf::from(r"\\nas\share\tessera")],
            except: vec![],
        };
        for path in [
            r"\\?\UnC\nas\share\tessera\auth.json",
            r"\\.\UNC\nas\share\tessera\auth.json",
        ] {
            assert!(unc.covers_on(Path::new(path), true));
        }
    }
    #[test]
    fn windows_credentials_use_both_slashes_and_ignore_letter_case() {
        let home = Path::new(r"C:\Users\Ann");
        for path in [
            r"C:\Users\Ann\.SSH\ID_ED25519",
            r"C:/Users/Ann/.AWS/CREDENTIALS",
            r"C:\Users\Ann\_NETRC",
            r"C:\Users\Ann\.GIT-CREDENTIALS",
            r"C:\Users\Ann\.CARGO\CREDENTIALS.TOML",
            r"C:\Users\Ann\.CODEX\AUTH.JSON",
            r"C:\Users\Ann\AppData\Roaming\gh\hosts.yml",
            r"C:\Users\Ann\AppData\Roaming\rclone\rclone.conf",
            r"C:\Users\Ann\.CLAUDE\.CREDENTIALS.JSON",
            r"c:\USERS\ann\.NPMRC",
        ] {
            assert!(
                credential_file_on(Path::new(path), true, Some(home)),
                "{path}"
            );
        }
        for path in [
            r"C:\Users\Ann\.ssh\id_ed25519.PUB",
            r"C:\Users\Ann\project\.npmrc",
            r"C:\Users\Ann\notes.txt",
        ] {
            assert!(
                !credential_file_on(Path::new(path), true, Some(home)),
                "{path}"
            );
        }
        assert!(claude_state_path_on(
            Path::new(r"c:\USERS\ann\.CLAUDE\settings.json"),
            true,
            Some(home)
        ));
        assert!(claude_state_path_on(
            Path::new(r"D:\Claude Work\.CREDENTIALS.JSON"),
            true,
            Some(home)
        ));
        assert!(!claude_state_path_on(
            Path::new(r"C:\Users\Ann2\.claude\settings.json"),
            true,
            Some(home)
        ));
        assert!(claude_state_path_on(
            Path::new(r"\\?\C:\Users\Ann\.Claude\settings.json"),
            true,
            Some(home)
        ));
        assert!(claude_state_path_on(
            Path::new(r"\\.\C:\Users\Ann\.Claude\settings.json"),
            true,
            Some(home)
        ));
        assert!(!claude_state_path_on(
            Path::new("/tmp/.CREDENTIALS.JSON"),
            false,
            None
        ));
        // Linux keeps literal backslashes and case significant.
        assert!(!credential_file_on(
            Path::new(r"/tmp/.ssh\id_ed25519"),
            false,
            None
        ));
        assert!(!credential_file_on(
            Path::new("/tmp/.SSH/ID_ED25519"),
            false,
            None
        ));
    }

    /// Claude Code's sign-in, the usual account's and each other account's
    /// (its own `CLAUDE_CONFIG_DIR`), reads like any other program's.
    #[test]
    fn claude_sign_ins_are_credentials() {
        use std::path::Path;
        assert!(credential_file(Path::new(
            "/home/a/.claude/.credentials.json"
        )));
        assert!(credential_file(Path::new(
            "/home/a/.local/share/tessera-claude/claude-code-work/.credentials.json"
        )));
        assert!(!credential_file(Path::new(
            "/home/a/.local/share/tessera-claude/claude-code-work/settings.json"
        )));
        assert!(credential_file(Path::new("/home/a/.netrc")));
    }

    #[test]
    fn windows_spellings_of_tesseras_folder_are_caught() {
        let p = Protected {
            roots: vec![PathBuf::from(r"C:\Users\Ann Lee\AppData\Roaming\tessera")],
            except: vec![PathBuf::from(
                r"C:\Users\Ann Lee\AppData\Roaming\tessera\uploads",
            )],
        };
        let vars = [
            ("APPDATA", r"C:\Users\Ann Lee\AppData\Roaming".to_string()),
            ("USERPROFILE", r"C:\Users\Ann Lee".to_string()),
        ];
        for cmd in [
            r#"type "C:\Users\Ann Lee\AppData\Roaming\tessera\auth.json""#,
            r"Get-Content c:/users/ann lee/appdata/roaming/TESSERA/auth.json",
            r"cat /c/Users/Ann Lee/AppData/Roaming/tessera/auth.json",
            r"type %APPDATA%\tessera\auth.json",
            r"Get-Content $env:APPDATA\tessera\auth.json",
            r"Get-Content ${env:AppData}/tessera/tessera.db",
            r"Get-ChildItem ~\AppData\Roaming\tessera",
            r"Get-Content $HOME\AppData\Roaming\tessera\auth.json",
            r"dir $env:TESSERA_HOME",
        ] {
            assert!(p.mentioned_in_windows(cmd, &vars), "{cmd}");
        }
        for cmd in [
            r"type %APPDATA%\tessera\uploads\s1\notes.txt",
            r"dir %APPDATA%\tessera-backup",
            r"cargo build",
            r"Get-Content $env:APPDATA\Code\settings.json",
        ] {
            assert!(!p.mentioned_in_windows(cmd, &vars), "{cmd}");
        }
    }

    #[test]
    fn a_question_covering_several_things_is_split_but_paths_never_are() {
        assert_eq!(key_parts("bash:sudo"), ["bash:sudo"]);
        assert_eq!(
            key_parts("bash:git-push+bash:sudo+secret:GH_TOKEN"),
            ["bash:git-push", "bash:sudo", "secret:GH_TOKEN"]
        );
        for whole in [
            "write:/home/u/a+write:/home/u/.bashrc",
            "bash:sudo+write:/x",
            "secret:A+B",
            "write-dir:/tmp/c++",
        ] {
            assert_eq!(key_parts(whole), [whole]);
        }
    }

    #[test]
    fn credentials_are_scrubbed_but_structure_survives() {
        let text = "\
OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz0123
export GITHUB_TOKEN=\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"
aws = AKIAIOSFODNN7EXAMPLE
db_password: testpw2testpw2
jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJleGFtcGxlLXVzZXIifQ.EXAMPLE_TEST_SIGNATURE_NOT_REAL
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAA
-----END OPENSSH PRIVATE KEY-----
PORT=8080
DEBUG=true
TOKEN_URL=$AUTH_URL
Authorization = \"Bearer abcdef0123456789xyz\"
";
        let out = scrub_patterns(text);
        for secret in [
            "sk-proj-abc",
            "ghp_abc",
            "AKIAIOSFODNN7",
            "testpw2",
            "eyJhbGci",
            "b3BlbnNzaC1",
            "abcdef0123456789xyz",
        ] {
            assert!(!out.contains(secret), "{secret} survived:\n{out}");
        }
        // Names, harmless values and references stay readable.
        for kept in [
            "OPENAI_API_KEY=",
            "export GITHUB_TOKEN=",
            "db_password:",
            "PORT=8080",
            "DEBUG=true",
            "TOKEN_URL=$AUTH_URL",
            "[REDACTED PRIVATE KEY]",
            "Authorization = \"Bearer [REDACTED]",
        ] {
            assert!(out.contains(kept), "{kept} missing:\n{out}");
        }
        assert_eq!(
            redact_known("a value-123456 b", &["value-123456".into()]),
            "a [REDACTED] b"
        );
    }

    #[test]
    fn short_values_are_redacted_only_as_whole_tokens() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Never inside a longer word or number.
        let short = s(&["anon", "root", "4821", "pw!x"]);
        for kept in [
            "the canonical state; anonymous access",
            "systemd-fsckroot rooted 48210 x4821",
            "pw!xy",
        ] {
            assert_eq!(redact_known(kept, &short), kept);
        }
        assert_eq!(
            redact_known("uid=0(root) pin:4821 [anon] pw!x.", &short),
            "uid=0([REDACTED]) pin:[REDACTED] [[REDACTED]] [REDACTED]."
        );
        // A value ending in a symbol may be followed by anything.
        assert_eq!(redact_known("a=pw!xyz", &s(&["pw!"])), "a=[REDACTED]xyz");
        // Long values go wherever they are, glued to other text or not.
        let long = s(&["tok-9f8e7d6c", "zz12"]);
        assert_eq!(
            redact_known("Xtok-9f8e7d6cY zz12 zz123", &long),
            "X[REDACTED]Y [REDACTED] zz123"
        );
        assert_eq!(
            redact_known("same", &s(&["", "same", "same"])),
            "[REDACTED]"
        );
    }

    #[test]
    fn protected_locations_and_exceptions() {
        let p = Protected {
            roots: vec![
                "/h/.local/share/tessera".into(),
                "/h/.config/tessera".into(),
            ],
            except: vec!["/h/.config/tessera/skills".into()],
        };
        assert!(p.covers(Path::new("/h/.local/share/tessera/auth.json")));
        assert!(p.covers(Path::new("/h/.config/tessera/config.toml")));
        assert!(!p.covers(Path::new("/h/.config/tessera/skills/x/SKILL.md")));
        assert!(!p.covers(Path::new("/h/.local/share/other")));
        // Walks skip hidden trees but still enter one that holds an exception.
        assert!(p.skips(Path::new("/h/.local/share/tessera")));
        assert!(!p.skips(Path::new("/h/.config/tessera")));
        assert!(p.mentioned_in("cat /h/.config/tessera/config.toml"));
        assert!(!p.mentioned_in("ls /h/.config/tessera/skills/x"));
        assert!(p.mentioned_in("cat /h/.config/tessera/skills/../config.toml | head"));
        assert!(p.mentioned_in("cat '/h/.config/tessera/skills/demo/../../config.toml'"));
        assert!(p.mentioned_in(
            "ls /h/.config/tessera/skills/x; cat /h/.config/tessera/skills/../config.toml"
        ));
        // Using the variable counts; naming it (a grep, a test run) doesn't.
        assert!(p.mentioned_in("cat $TESSERA_HOME/auth.json"));
        assert!(p.mentioned_in("ls ${TESSERA_HOME:-x}"));
        assert!(!p.mentioned_in("TESSERA_HOME=/tmp/x cargo test; grep -rn TESSERA_HOME src"));
        // A sibling that merely starts the same is not the store.
        assert!(!p.mentioned_in("source /h/.local/share/tessera-toolchain/env.sh"));
        assert!(p.mentioned_in("ls /h/.local/share/tessera; echo"));
        assert!(p.mentioned_in("tar czf x.tgz '/h/.local/share/tessera'"));
    }
}
