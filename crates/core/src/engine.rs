//! The engine: owns sessions and runs agent turns. Every client — in-process
//! TUI, WebSocket server, tests — drives it through `handle(Request)` and
//! observes it through `subscribe()`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use anyhow::{Context as _, Result, anyhow};
use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use tessera_proto::*;
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::approval::{self, Answer, Outcome as ApprovalOutcome};
use crate::auth::{AuthStore, CODEX_PROVIDER, Credential};
use crate::config::{Config, ModelConfig, Paths, ProviderConfig};
use crate::context;
use crate::llm::{ErrorKind, LlmEvent, LlmRequest, LlmResponse, Msg, retry};
use crate::memory::Memory;
use crate::providers::Providers;
use crate::store::{SessionInternal, StopMarks, Store};
use crate::tools::{
    Registry, Risk, ToolCtx, ToolHost, ToolOutput,
    meta::{SubTask, is_check_ack},
};
use crate::util::{new_id, now_ms, one_line};

mod accounts;
mod attempts;
mod boards;
mod calls;
mod claude_code;
mod compaction;
mod deltas;
mod devices;
mod eviction;
mod handoffs;
mod helper_approvals;
mod helper_limits;
mod inbox;
mod loops;
mod machines;
mod memory;
mod notes;
mod notify;
mod permissions;
mod private;
mod queues;
mod rewind;
mod security;
mod small;
mod snapshot;
mod subagents;
mod terminals;
mod title;
mod turn_end;
mod usage;
mod verify;
mod voice;
mod web_access;
mod webview;

#[cfg(test)]
mod efficiency_tests;
#[cfg(test)]
mod lock_order_tests;
#[cfg(test)]
pub(crate) mod test_support;

use loops::{Guardrails, LoopLimits};
pub use machines::{NodeConn, is_node_request};
pub use memory::{ConnStats, MemoryReport};
pub(crate) use notes::{MAX_ASSET as NOTE_PICTURE_MAX, asset_refs as note_asset_refs};
pub use terminals::{TermConn, is_ordered_terminal_request, is_terminal_request};

#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    /// Run scheduled jobs (only one process per data directory should).
    pub scheduler: bool,
}

#[derive(Clone)]
pub struct Engine(Arc<Inner>);

/// The user's Stops on this computer at one look (`Inner::here_now`).
struct HereNow {
    stopped: bool,
    /// Its Stop file's count.
    file: u32,
    /// This computer's own machines: id and Stop count.
    machines: Vec<(String, u32)>,
}

impl HereNow {
    /// No Stop here since a turn from `marks` was accepted (a machine it
    /// didn't know of then counts from 0).
    fn since(&self, marks: &StopMarks) -> bool {
        self.file <= marks.here
            && self
                .machines
                .iter()
                .all(|(id, n)| *n <= marks.machines.get(id).copied().unwrap_or(0))
    }
}

/// A tool call refused across a Stop on this computer (`Inner::running_here_since`).
const STOPPED_SINCE: &str = "Tessera was stopped on this computer while this was on its way (or is stopped now), so it was not done. Don't retry; tell the user (a new message after they resume works as usual).";

pub(crate) struct Inner {
    paths: Paths,
    config: RwLock<Config>,
    store: Store,
    auth: Arc<AuthStore>,
    providers: Providers,
    registry: RwLock<Registry>,
    memory: Arc<Memory>,
    bots: RwLock<Vec<Bot>>,
    bot_memories: Mutex<HashMap<String, Arc<Memory>>>,
    events: broadcast::Sender<Event>,
    notification_presence: Mutex<HashMap<String, (std::time::Instant, Vec<String>)>>,
    /// When each source's agent notifications paged in the last hour (engine/notify.rs).
    notification_pages: Mutex<HashMap<String, Vec<u64>>>,
    notification_gate: Mutex<()>,
    /// Devices revoked while running: their open connections close at once.
    revoked: broadcast::Sender<String>,
    // Readers must clone Arcs and release this map before locking Rt::st.
    // A state holder may look up its parent or insert a new helper here.
    sessions: Mutex<HashMap<String, Arc<Rt>>>,
    loading: Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    next_eviction: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    lock_order_hooks: lock_order_tests::Hooks,
    #[cfg(debug_assertions)]
    final_admission_gates: Mutex<HashMap<String, Arc<FinalAdmissionGate>>>,
    approvals: Mutex<HashMap<String, Pending>>,
    /// Helpers working at once: each chat's limit and the server's
    /// capacity (see `engine/helper_limits.rs`).
    helper_gates: helper_limits::HelperGates,
    /// Address the server actually listens on (`--bind` may differ from config).
    listen: RwLock<Option<String>>,
    /// The server has the web app for "Use Tessera in a web browser".
    web_app: std::sync::atomic::AtomicBool,
    /// Background subagents, by parent session.
    background: Mutex<HashMap<String, Vec<BgAgent>>>,
    /// Serialises mosaic layout saves.
    mosaics_lock: Mutex<()>,
    /// The notes library: one save at a time, preview pages.
    notes: notes::NotesState,
    /// Questions agents are waiting on in whiteboards, by ask id.
    board_asks: Mutex<HashMap<String, boards::PendingAsk>>,
    /// `look`s waiting for a device's snapshot, by request id.
    snapshots: Mutex<HashMap<String, boards::SnapshotTx>>,
    /// Connected clients (see [`Engine::client_connected`]).
    clients: std::sync::atomic::AtomicUsize,
    /// The server is stopping on purpose (see [`Engine::begin_shutdown`]).
    stopping: std::sync::atomic::AtomicBool,
    /// Sessions this start found cut off (marked interrupted just now).
    cut_off: Mutex<HashSet<String>>,
    /// cloudflared, when Tessera runs the tunnel itself.
    tunnel: Mutex<Option<crate::remote::Supervisor>>,
    background_batches: std::sync::atomic::AtomicU64,
    /// A secure-entry card is consumed once, even across concurrent app requests.
    secret_entry: Mutex<()>,
    /// Language servers (`[lsp]`); `None` when disabled.
    lsp: RwLock<Option<Arc<crate::lsp::LspManager>>>,
    /// What each session was shown of each file (edit rebase, seen lines).
    files: crate::tools::filecache::FileCache,
    /// The browsers web views run in: this server's, and machines' ("Web
    /// views here"; web view tiles and the `browser` tool).
    browser: crate::browser::Browsers,
    /// Prices from the public catalog (`[pricing] catalog`).
    catalog: Arc<crate::pricing::Catalog>,
    /// Computers running `tessera node` (see `engine/machines.rs`).
    machines: machines::Machines,
    /// The user's own terminals, here and on machines (`engine/terminals.rs`).
    terminals: terminals::Terminals,
    /// Told whenever a session's run ends (see [`Inner::when_idle`]).
    turn_ended: tokio::sync::Notify,
    /// Wakes the scheduler early (a job was saved, a device connected).
    sched: tokio::sync::Notify,
    job_admissions: Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    /// Subagents on a server that answers one request at a time take turns
    /// there, by provider (see `engine/subagents.rs`).
    lanes: Mutex<HashMap<String, Arc<Semaphore>>>,
    /// Claude Code processes kept open between turns, and its subagents the
    /// user stopped alone (see `engine/claude_code.rs`).
    claude: claude_code::ClaudeProcs,
    /// The server's own speech engines (`crate::voice`, `engine/voice.rs`).
    voice: Arc<crate::voice::Voice>,
    me: Weak<Inner>,
}

struct Pending {
    shown_on: String,
    acting_on: String,
    request: ApprovalRequest,
    deadline: tokio::time::Instant,
    tx: oneshot::Sender<Answer>,
    /// A helper's: what the chat that started it would need to answer it
    /// (`agents approve`), checked when it answers.
    authority: Option<helper_approvals::ParentAuthority>,
}

/// A subagent running in the background for a parent session.
struct BgAgent {
    name: String,
    session_id: String,
    started: std::time::Instant,
    cancel: CancellationToken,
    batch: u64,
    done: bool,
    /// Stopped by the user (no automatic follow-up turn for the parent).
    stopped: bool,
}

/// When a subagent must stop exploring and write its report.
struct Budget {
    max_steps: u32,
    /// Tool calls it had made before this budget started (continued subagents).
    base: u32,
    /// Calls admitted atomically, including ones still executing.
    used: u32,
    /// Claude's hook, permission callback and MCP dispatch share one call.
    calls: HashMap<String, helper_limits::StepCall>,
    /// None: no time limit.
    soft_deadline: Option<std::time::Instant>,
    wrapped: bool,
    /// Normal terminal completion, distinct from a report/retry boundary.
    closed: bool,
}

impl Budget {
    fn persisted(&self) -> crate::agents::HelperRunBudget {
        crate::agents::HelperRunBudget {
            max_steps: self.max_steps,
            base: self.base,
            used: self.used,
            calls: self.calls.clone(),
            wrapped: self.wrapped,
            closed: self.closed,
            soft_deadline_ms: self.soft_deadline.map(|d| {
                now_ms().saturating_add(
                    d.saturating_duration_since(std::time::Instant::now())
                        .as_millis() as u64,
                )
            }),
        }
    }
    fn restored(mut saved: crate::agents::HelperRunBudget) -> Self {
        // A restart cancels any permission-stage claim that never dispatched.
        // Dispatched calls keep their consumed unit and cannot replay even if
        // the process died before their result arrived.
        let provisional = saved.calls.values().filter(|c| !c.dispatched).count() as u32;
        saved.calls.retain(|_, c| c.dispatched);
        if saved.max_steps != helper_limits::UNLIMITED {
            saved.used = saved.used.saturating_sub(provisional);
        }
        for c in saved.calls.values_mut() {
            c.pending = false;
        }
        Self {
            max_steps: saved.max_steps,
            base: saved.base,
            used: saved.used,
            calls: saved.calls,
            wrapped: saved.wrapped,
            closed: saved.closed,
            soft_deadline: saved.soft_deadline_ms.map(|d| {
                std::time::Instant::now()
                    + std::time::Duration::from_millis(d.saturating_sub(now_ms()))
            }),
        }
    }
}
/// What a subagent produced.
pub struct SubResult {
    pub session_id: Option<String>,
    pub result: Result<String, String>,
    /// A line for the parent beside the report (e.g. worktree changes).
    pub note: Option<String>,
}

struct Rt {
    used: std::sync::atomic::AtomicU64,
    st: Mutex<RtState>,
}

type HandoffReceipt = Arc<Mutex<Option<crate::store::Attempt>>>;

struct RtState {
    info: SessionInfo,
    permission_ceiling: Arc<permissions::PermissionCeiling>,
    /// Shared with the live parent, which stays cached while a child holds it.
    parent_ceiling: Option<(String, Arc<permissions::PermissionCeiling>)>,
    /// Whether the cached native prompt offered helpers.
    prompt_helpers: Option<bool>,
    internal: SessionInternal,
    running: bool,
    cancel: Option<CancellationToken>,
    /// The user's Stop counts when the turn was accepted (here and on each
    /// machine): a turn from before a Stop runs no tool after it, even once
    /// resumed (kept with the session, `SessionInternal::turn_marks`).
    turn_stops: StopMarks,
    /// What the next turn belongs to, fixed when its message was accepted
    /// (or its helper started); else it takes the counts when it starts.
    next_turn_stops: Option<StopMarks>,
    streaming: Option<StreamingMessage>,
    steer: Vec<Waiting>,
    followups: Vec<Waiting>,
    /// "Send now": the running turn was stopped for these; they start the
    /// next one (see the end of `run`).
    send_now: Vec<Waiting>,
    /// Selected queue messages not yet committed to history.
    delivering: Vec<(Queue, Waiting)>,
    /// Subagents only: when to ask for the final report.
    budget: Option<Budget>,
    depth: u8,
    approvals_on: String,
    /// Restricts the tool set (background review sessions).
    tools_only: Option<Vec<&'static str>>,
    /// The model may call no tool now (a subagent writing its report). Its
    /// tools stay listed where the provider takes `tool_choice: none`, so
    /// the prompt cache holds.
    no_tool_calls: bool,
    /// Bot speaking in the current turn of a multi-bot channel.
    speaker: Option<String>,
    /// Nesting of bot-to-bot conversations (message_bot chains).
    bot_depth: u8,
    /// Captured once under final admission, shared with a synchronous caller.
    bot_receipt: Option<HandoffReceipt>,
    /// The `agent` tool's description with this session's agent types,
    /// fixed once computed (prompt caching).
    agent_list: std::sync::OnceLock<String>,
    /// The next checkpoint, written ahead of time (engine/compaction.rs).
    pre: Option<compaction::PreCheckpoint>,
    /// The last reply was cut off because it kept repeating itself
    /// (`stream_retrying`; the turn loop takes it).
    repeated: bool,
    /// This run already notified about a Claude plan wait; finalisation
    /// must not add a second notification for the same limit episode.
    claude_limit_notified: bool,
    claude_limit_window: Option<(String, u64)>,
    /// Long work still going on after its turn (its own helpers at work, a
    /// queued message next): when it began (clock, ms, last seq). The
    /// "Finished" notification waits for the end of all of it.
    work_span: Option<(std::time::Instant, u64, u64)>,
    /// A rewind is under way: what its kept Claude Code starts by itself
    /// doesn't go on (see `engine/rewind.rs`).
    rewinding: bool,
}

enum Outcome {
    Done,
    Aborted,
    Failed(String),
}

const FAILED_REPLY_NOTICE: &str = "The model did not finish its reply successfully. Partial text was kept. Send a message to try again.";

type ApiResult = Result<Value, ApiError>;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, msg)
}
fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}
fn to_value<T: serde::Serialize>(v: T) -> ApiResult {
    serde_json::to_value(v).map_err(internal)
}

/// A per-engine, debug-build-only integration-test barrier at final admission.
#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Default)]
pub struct FinalAdmissionGate {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(debug_assertions)]
impl FinalAdmissionGate {
    pub async fn reached(&self) {
        self.reached.notified().await;
    }
    pub fn release(&self) {
        self.release.notify_one();
    }
}

impl Engine {
    /// Hold exactly one final admission, after the loop has returned Done.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn hold_final_admission_for_test(&self, session_id: &str) -> Arc<FinalAdmissionGate> {
        let gate = Arc::new(FinalAdmissionGate::default());
        self.0
            .final_admission_gates
            .lock()
            .insert(session_id.into(), gate.clone());
        gate
    }

    /// Stop what the server runs beside itself (the tunnel's cloudflared,
    /// the web view browser) before the process exits, so a server stopped
    /// by hand leaves nothing behind.
    pub async fn shutdown(&self) {
        self.begin_shutdown();
        let tunnel = self.0.tunnel.lock().take();
        if let Some(t) = tunnel {
            t.stop();
            for _ in 0..40 {
                if !t.state().running {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        self.0.browser.shutdown().await;
        self.0.terminals.shutdown();
    }

    /// The server is about to stop (an update, `systemctl restart`): note
    /// what is working so it continues after the restart, before anything
    /// else stops. Turns that end from here on were cut off by the stop, not
    /// failed: they stay marked running for the next start. Idempotent.
    pub fn begin_shutdown(&self) {
        use std::sync::atomic::Ordering;
        if !self.0.stopping.swap(true, Ordering::SeqCst) {
            self.0.note_planned_restart();
        }
    }

    pub async fn open(paths: Paths, opts: EngineOptions) -> Result<Self> {
        crate::util::raise_open_files_limit();
        paths.ensure()?;
        let config = Config::load(&paths)?;
        let store = Store::open(&paths.db_file())?;
        let auth = Arc::new(AuthStore::load(paths.auth_file())?);
        // Provider credentials and prompts must never follow HTTP redirects.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build()
            .context("building HTTP client")?;
        let (events, _) = broadcast::channel(memory::event_capacity());
        let providers = Providers::new(http, auth.clone(), &config.providers)
            .remembering(paths.data_dir.join("model-caps.json"), events.clone())
            .keeping_limits(paths.data_dir.join("plan-limits.json"))
            .with_claude_accounts(paths.claude_accounts_dir());
        if let Some(levels) = store
            .kv_get(MODEL_THINKING_KEY)?
            .and_then(|v| serde_json::from_str(&v).ok())
        {
            providers.set_chosen_thinking(levels);
        }
        let memory = Arc::new(Memory::new(
            paths.memory_dir(),
            config.memory.memory_chars,
            config.memory.user_chars,
        ));
        let bots = store.bots()?;
        let helper_gates = helper_limits::HelperGates::new(helper_limits::capacity(&config));
        let lsp = config
            .lsp
            .enabled
            .then(|| crate::lsp::LspManager::new(config.lsp.clone()));
        let (browser_events, data_dir) = (events.clone(), paths.data_dir.clone());
        let machines = machines::Machines::load(&store);
        let terminal_config = config.terminal.clone();
        let inner = Arc::new_cyclic(|me| Inner {
            terminals: terminals::Terminals::new(&paths, &terminal_config, me.clone()),
            paths,
            config: RwLock::new(config),
            store,
            auth,
            providers,
            registry: RwLock::new(Registry::builtin()),
            memory,
            bots: RwLock::new(bots),
            bot_memories: Mutex::new(HashMap::new()),
            events,
            revoked: broadcast::channel(64).0,
            sessions: Mutex::new(HashMap::new()),
            loading: Mutex::new(HashMap::new()),
            next_eviction: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            lock_order_hooks: Default::default(),
            #[cfg(debug_assertions)]
            final_admission_gates: Default::default(),
            approvals: Mutex::new(HashMap::new()),
            helper_gates,
            turn_ended: tokio::sync::Notify::new(),
            sched: tokio::sync::Notify::new(),
            job_admissions: Mutex::new(HashMap::new()),
            notification_presence: Default::default(),
            notification_pages: Default::default(),
            notification_gate: Default::default(),
            lanes: Mutex::new(HashMap::new()),
            claude: Default::default(),
            listen: RwLock::new(None),
            web_app: Default::default(),
            background: Mutex::new(HashMap::new()),
            mosaics_lock: Mutex::new(()),
            notes: Default::default(),
            board_asks: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            clients: Default::default(),
            stopping: Default::default(),
            cut_off: Default::default(),
            tunnel: Mutex::new(None),
            background_batches: std::sync::atomic::AtomicU64::new(1),
            secret_entry: Mutex::new(()),
            lsp: RwLock::new(lsp),
            files: Default::default(),
            browser: crate::browser::Browsers::new(
                me.clone() as Weak<dyn crate::browser::Host>,
                browser_events,
                &data_dir,
            ),
            catalog: Arc::new(crate::pricing::Catalog::new(&data_dir)),
            machines,
            voice: Arc::new(crate::voice::Voice::new(&data_dir)),
            me: me.clone(),
        });
        inner.claude_code_restore();
        // Checkouts of isolated subagents a crash left behind (their work is
        // kept as a patch first).
        inner.cleanup_worktrees().await;
        // Only the serving process (the one running jobs) resumes unattended
        // work; it claims those sessions now so the scheduler can't start a
        // second run of the same job meanwhile.
        let resume = inner.mark_interrupted(opts.scheduler)?;
        let resume = inner.claim_for_resume(resume);
        let first_run = inner.config.read().providers.is_empty()
            && inner.store.kv_get("autodetected")?.is_none();
        let mcp_servers = inner.config.read().mcp_servers.clone();
        if !mcp_servers.is_empty() || !resume.is_empty() {
            let bg = inner.clone();
            tokio::spawn(async move {
                if !mcp_servers.is_empty() {
                    let (tools, errors) =
                        crate::mcp::connect_all(&mcp_servers, bg.providers.http().clone()).await;
                    if !tools.is_empty() {
                        {
                            let mut reg = bg.registry.write();
                            for t in tools {
                                reg.add(t);
                            }
                        }
                        bg.mcp_connected();
                    }
                    for (name, e) in errors {
                        let _ = bg.events.send(Event::Notice {
                            session_id: None,
                            level: NoticeLevel::Warning,
                            message: format!("MCP server `{name}` is unavailable: {e}"),
                        });
                    }
                }
                // After the MCP tools are back, so resumed work finds them.
                bg.resume_interrupted(resume);
            });
        }
        if first_run {
            // Finish before any session is created so the default model is right.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                inner.autodetect_local(false),
            )
            .await;
        }
        let bg = inner.clone();
        tokio::spawn(async move {
            bg.providers.discover(None).await;
            let _ = bg.events.send(Event::ProvidersChanged);
        });
        // Every engine, serving or not (a terminal's own engine too).
        tokio::spawn(Inner::watch_stop_here(Arc::downgrade(&inner)));
        if opts.scheduler {
            tokio::spawn(crate::activity::monitor(
                &crate::activity::SCHEDULER,
                inner.clone().scheduler(),
            ));
            // Serving: bring a Tessera-managed Cloudflare tunnel back up.
            inner.resume_tunnel();
        }
        Ok(Self(inner))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    /// Ids of devices revoked from now on (a connection of one must close).
    pub fn revocations(&self) -> broadcast::Receiver<String> {
        self.0.revoked.subscribe()
    }

    /// Count a remote client (one per connection) while the guard lives, so
    /// a `look` stops waiting once every client has answered.
    pub fn client_connected(&self) -> ClientGuard {
        self.0
            .clients
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Someone to show models to: the scheduler looks at offline ones.
        self.0.sched.notify_one();
        ClientGuard(self.clone())
    }

    /// Remote clients connected now.
    pub fn connected_clients(&self) -> usize {
        self.0.clients.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A whiteboard's HTML, if `token` is its page token (see `BoardLink`).
    pub fn board_page(&self, id: &str, token: &str) -> Option<String> {
        self.0.board_page(id, token)
    }

    /// A preview page's HTML, while `token` is good (see `PreviewPage`).
    pub fn preview_page(&self, token: &str) -> Option<String> {
        self.0.preview_page(token)
    }

    pub fn paths(&self) -> &Paths {
        &self.0.paths
    }

    pub fn store(&self) -> &Store {
        &self.0.store
    }

    /// Whether a tool of this name is registered (MCP servers' tools arrive
    /// once they connect, in the background).
    pub fn has_tool(&self, name: &str) -> bool {
        self.0.registry.read().get(name).is_some()
    }

    /// Remind the user's devices that remote access's Cloudflare key runs
    /// out soon (or ran out), each reminder once; the scheduler asks often.
    pub fn remind_remote_key(&self) {
        self.0.remind_remote_key()
    }

    /// How the user's devices reach this server, header values included:
    /// only for the user's own apps over their end-to-end encrypted
    /// connection (the server decides who gets it; see `get_server_route`).
    pub fn server_route(&self) -> tessera_proto::ServerRoute {
        self.0.server_route()
    }

    pub fn config(&self) -> Config {
        self.0.config.read().clone()
    }

    pub fn auth(&self) -> &AuthStore {
        &self.0.auth
    }

    /// The stored secrets `command` in `session_id` would get at `place`,
    /// exactly as every tool that passes secrets on asks for them.
    pub fn secret_env(
        &self,
        session_id: &str,
        command: &str,
        place: crate::auth::SecretPlace,
    ) -> crate::auth::SecretUse {
        self.0.secret_env(session_id, command, place)
    }

    /// Record where `tessera serve` listens, for pairing links.
    pub fn set_listen_addr(&self, bind: &str) {
        *self.0.listen.write() = Some(bind.to_string());
    }

    /// Wait until discovery triggered at startup has had a chance to run.
    pub async fn refresh_models(&self) {
        self.0.providers.discover(None).await;
    }

    pub async fn handle(&self, req: Request, device: Option<&str>) -> ApiResult {
        let label = match &req {
            Request::ListSessions { .. } | Request::ListSessionPage { .. } => {
                &crate::activity::LIST
            }
            Request::GetSession { .. } | Request::GetSessionPage { .. } => {
                &crate::activity::SNAPSHOT
            }
            Request::Send { .. } => &crate::activity::SEND,
            Request::UpdateSession { .. } => &crate::activity::UPDATE,
            Request::CreateSession { .. } => &crate::activity::Label("request:CreateSession"),
            Request::DeleteSession { .. } => &crate::activity::Label("request:DeleteSession"),
            Request::ForkSession { .. } => &crate::activity::Label("request:ForkSession"),
            Request::Rewind { .. } => &crate::activity::Label("request:Rewind"),
            Request::Compact { .. } => &crate::activity::Label("request:Compact"),
            Request::Search { .. } => &crate::activity::Label("request:Search"),
            Request::ListProviders { .. } => &crate::activity::Label("request:ListProviders"),
            Request::SaveProvider { .. } => &crate::activity::Label("request:SaveProvider"),
            Request::SetMemory { .. } => &crate::activity::Label("request:SetMemory"),
            Request::SaveMosaics { .. } => &crate::activity::Label("request:SaveMosaics"),
            Request::GetUsage { .. } => &crate::activity::Label("request:GetUsage"),
            Request::ListJobs => &crate::activity::Label("request:ListJobs"),
            Request::SaveJob { .. } => &crate::activity::Label("request:SaveJob"),
            Request::RunJob { .. } => &crate::activity::Label("request:RunJob"),
            _ => &crate::activity::REQUEST,
        };
        // The combined dispatcher future is large. Keep it off callers' async
        // frames (including the standard 2 MiB Rust test thread stack), while
        // retaining the same monitored await and cancellation lifetime.
        crate::activity::monitor(label, Box::pin(self.0.clone().handle(req, device))).await
    }

    /// Block until the session is idle (used by one-shot CLI runs and tests).
    pub async fn wait_idle(&self, session_id: &str) {
        let mut rx = self.subscribe();
        loop {
            let running = self
                .0
                .live_session(session_id)
                .map(|rt| rt.st.lock().running)
                .unwrap_or(false);
            if !running {
                return;
            }
            if let Ok(Err(broadcast::error::RecvError::Closed)) =
                tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
            {
                return;
            }
        }
    }
}

/// A connected client; see [`Engine::client_connected`].
pub struct ClientGuard(Engine);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0
            .0
            .clients
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

// ---- request handling ---------------------------------------------------------

impl Inner {
    async fn handle(self: Arc<Self>, req: Request, device: Option<&str>) -> ApiResult {
        self.maintain_sessions();
        // Another device's private chat (or what belongs to it) is as if it didn't exist.
        self.check_private(&req, device)?;
        // Which device the user is using each chat on (web views go there).
        self.note_presence(&req, device);
        match req {
            Request::Hello { .. } => to_value(self.server_info()),
            Request::ListSessions { archived } => {
                let me = self.clone();
                let device = device.map(str::to_owned);
                tokio::task::spawn_blocking(move || {
                    let _activity = crate::activity::enter(&crate::activity::LIST);
                    let mut list = me.list_sessions(archived).map_err(internal)?;
                    // Another device's private chats are never listed.
                    list.retain(|s| me.device_sees(&s.id, device.as_deref()));
                    to_value(list)
                })
                .await
                .map_err(internal)?
            }
            Request::ListSessionPage {
                archived,
                limit,
                before,
                include_ids,
            } => {
                let include_ids: Vec<_> = include_ids.into_iter().take(128).collect();
                let me = self.clone();
                let device = device.map(str::to_owned);
                tokio::task::spawn_blocking(move || {
                    let _activity = crate::activity::enter(&crate::activity::LIST);
                    let mut page = me
                        .store
                        .list_session_page(
                            archived,
                            limit.unwrap_or(100),
                            before.as_ref(),
                            &include_ids,
                        )
                        .map_err(internal)?;
                    let first = before.is_none();
                    // Copy Arcs before state locks (the same lock order as the
                    // legacy list). Live badges/private chats override disk.
                    for rt in me.live_sessions() {
                        let st = rt.st.lock();
                        let i = &st.info;
                        let existing = page.sessions.iter().position(|s| s.id == i.id);
                        let essential = first
                            && (include_ids.iter().any(|id| id == &i.id)
                                || (i.archived == archived
                                    && (i.pinned
                                        || i.private
                                        || st.running
                                        || !matches!(
                                            i.status,
                                            SessionStatus::Idle | SessionStatus::Error
                                        ))));
                        if existing.is_none() && !essential {
                            continue;
                        }
                        let mut summary = i.clone();
                        summary.task = None;
                        if let Some(at) = existing {
                            page.sessions[at] = summary;
                        } else {
                            page.sessions.push(summary);
                        }
                    }
                    page.sessions
                        .retain(|s| me.device_sees(&s.id, device.as_deref()));
                    to_value(page)
                })
                .await
                .map_err(internal)?
            }
            Request::CreateSession {
                cwd,
                model,
                title,
                bot_id,
                private,
                lean,
            } => {
                if private {
                    // Only the device that makes it ever sees it.
                    let rt = self
                        .create_session_ext(
                            None,
                            model,
                            title,
                            SessionKind::Chat,
                            None,
                            None,
                            None,
                            true,
                            false,
                            false,
                            device,
                        )
                        .map_err(|e| bad(e.to_string()))?;
                    return to_value(rt.st.lock().info.clone());
                }
                let bot = match bot_id.as_deref().filter(|b| !b.is_empty()) {
                    Some(id) => Some(self.find_bot(id).ok_or_else(|| {
                        ApiError::new(ErrorCode::NotFound, format!("no bot `{id}`"))
                    })?),
                    None => None,
                };
                let rt = self
                    .create_session_ext(
                        cwd,
                        model,
                        title,
                        SessionKind::Chat,
                        None,
                        None,
                        bot.as_ref(),
                        false,
                        false,
                        lean,
                        None,
                    )
                    .map_err(|e| bad(e.to_string()))?;
                to_value(rt.st.lock().info.clone())
            }
            Request::GetSession {
                session_id,
                since_seq,
            } => {
                let me = self.clone();
                tokio::task::spawn_blocking(move || {
                    let _activity = crate::activity::enter(&crate::activity::SNAPSHOT);
                    to_value(me.snapshot(&session_id, since_seq.unwrap_or(0))?)
                })
                .await
                .map_err(internal)?
            }
            Request::GetSessionPage {
                session_id,
                before_seq,
                limit,
            } => {
                let me = self.clone();
                tokio::task::spawn_blocking(move || {
                    let _activity = crate::activity::enter(&crate::activity::SNAPSHOT);
                    to_value(me.snapshot_with_page(
                        &session_id,
                        0,
                        Some((before_seq, limit.unwrap_or(120))),
                    )?)
                })
                .await
                .map_err(internal)?
            }
            Request::UpdateSession {
                session_id,
                title,
                model,
                thinking,
                permission,
                pinned,
                archived,
                cwd,
                members,
                fast,
                subagents,
            } => {
                let rt = self.rt(&session_id)?;
                let members = members.map(|m| self.resolve_members(&m)).transpose()?;
                let mut picked = None;
                {
                    let mut st = rt.st.lock();
                    if let Some(t) = title {
                        st.info.title = one_line(&t, 120);
                        st.internal.titled = true;
                    }
                    if let Some(m) = model {
                        let m = self.resolve_ref(&m).map_err(bad)?;
                        if !self.providers.exists(&m.provider) {
                            return Err(bad(format!("unknown provider `{}`", m.provider)));
                        }
                        if let Some(info) = self.providers.model_info(&m) {
                            st.info.context.window = self.working_window(&m, info.context_window);
                            // The level last picked for the new model, if any.
                            let want = info
                                .last_thinking
                                .filter(|_| user_chat(&st.info))
                                .unwrap_or(st.info.thinking);
                            st.info.thinking =
                                crate::thinking::clamp(want, &info.thinking, info.default_thinking);
                        }
                        st.info.model = m;
                        if !self.providers.daybreak_available(&st.info.model) {
                            st.info.daybreak = false;
                        }
                    }
                    if let Some(t) = thinking {
                        st.info.thinking = match self.providers.model_info(&st.info.model) {
                            Some(info) => {
                                crate::thinking::clamp(t, &info.thinking, info.default_thinking)
                            }
                            None => t,
                        };
                        if user_chat(&st.info) {
                            picked = Some((st.info.model.clone(), st.info.thinking));
                        }
                    }
                    if let Some(p) = permission {
                        st.info.permission = p;
                        self.publish_permission(&st);
                    }
                    if let Some(f) = fast {
                        st.info.fast = f;
                    }
                    if let Some(a) = subagents.filter(|a| *a != st.info.subagents) {
                        st.info.subagents = a;
                        // Native prompts and each room bot's Claude context must refresh.
                        st.internal.system_prompt = None;
                        st.internal.bot_prompts.clear();
                        st.internal.claude_secure_entry = false;
                        claude_code::renew_claude_context(&mut st.internal);
                        st.internal.claude_context_epoch =
                            st.internal.claude_context_epoch.wrapping_add(1);
                    }
                    if let Some(p) = pinned {
                        st.info.pinned = p;
                    }
                    if let Some(a) = archived {
                        st.info.archived = a;
                    }
                    if let Some(m) = members {
                        st.info.members = m;
                        st.internal.bot_prompts.clear();
                        claude_code::renew_claude_context(&mut st.internal);
                    }
                    if let Some(c) = cwd {
                        let p = crate::util::expand_home(&c);
                        if !p.is_dir() {
                            return Err(bad(format!("{} is not a directory", p.display())));
                        }
                        st.info.cwd = p.display().to_string();
                        st.internal.system_prompt = None;
                    }
                    st.info.updated_at = now_ms();
                }
                self.save(&rt);
                // New chats on that model start at the level picked here.
                if let Some((m, level)) = picked
                    && let Some(all) = self.providers.choose_thinking(&m, level)
                {
                    let saved = serde_json::to_string(&all)
                        .map_err(anyhow::Error::from)
                        .and_then(|v| self.store.kv_set(MODEL_THINKING_KEY, &v));
                    if let Err(e) = saved {
                        tracing::warn!("could not save the thinking level: {e}");
                    }
                    let _ = self.events.send(Event::ProvidersChanged);
                }
                to_value(rt.st.lock().info.clone())
            }
            Request::SetSessionDaybreak {
                session_id,
                enabled,
            } => {
                let rt = self.rt(&session_id)?;
                let model = rt.st.lock().info.model.clone();
                if enabled {
                    let _ = self.providers.discover(Some(&model.provider)).await;
                    if !self.providers.daybreak_available(&model) {
                        return Err(bad(
                            "OpenAI has not confirmed Daybreak Blue access for this model.",
                        ));
                    }
                }
                {
                    let mut st = rt.st.lock();
                    if st.info.model != model {
                        return Err(bad(
                            "The model changed. Try the Daybreak Blue choice again.",
                        ));
                    }
                    st.info.daybreak = enabled;
                    st.info.updated_at = now_ms();
                }
                self.save(&rt);
                to_value(rt.st.lock().info.clone())
            }
            Request::DeleteSession { session_id } => {
                self.remove_session(session_id)?;
                Ok(Value::Null)
            }
            Request::ForkSession { session_id, at_seq } => {
                if self.store.is_private(&session_id) {
                    return Err(bad("private chats can't be forked"));
                }
                to_value(self.fork(&session_id, at_seq)?)
            }
            Request::Rewind {
                session_id,
                to_seq,
                files,
                conversation,
                dry_run,
            } => to_value(self.rewind(&session_id, to_seq, files, conversation, dry_run)?),
            Request::SetGoal { session_id, goal } => to_value(self.set_goal(&session_id, &goal)?),
            Request::Dequeue { session_id, index } => {
                let rt = self.rt(&session_id)?;
                {
                    let mut st = rt.st.lock();
                    self.update_queue(&mut st, |st| {
                        // `index` counts only what the user sees (their own messages).
                        let (list, i) = queued_target(st, index).ok_or_else(no_longer_queued)?;
                        st.queue(list).remove(i);
                        Ok(())
                    })?;
                    self.queue_changed(&st);
                }
                Ok(Value::Null)
            }
            Request::EditQueued {
                session_id,
                index,
                text,
            } => {
                let mut text = text.trim().to_string();
                if !text.is_empty() {
                    let mut parts = vec![ContentPart::text(text)];
                    let notes = self.capture_secrets(&session_id, &mut parts)?;
                    self.post_notes(&session_id, notes);
                    // The user's own words: notes they link become readable here.
                    self.grant_mentioned_notes(&session_id, &parts);
                    text = plain_text(&parts);
                }
                let rt = self.rt(&session_id)?;
                {
                    let mut st = rt.st.lock();
                    self.update_queue(&mut st, |st| {
                        let (list, i) = queued_target(st, index).ok_or_else(no_longer_queued)?;
                        let item = &mut st.queue(list)[i];
                        // Keep images and attachment notes; replace what they typed
                        // (with attachments it may be left empty).
                        let keep = |p: &ContentPart| match p {
                            ContentPart::Text { text } => text.starts_with("[Attached "),
                            _ => true,
                        };
                        if text.is_empty() && !item.content.iter().any(keep) {
                            return Err(bad("the message is empty; remove it instead"));
                        }
                        let kept: Vec<ContentPart> = item.content.drain(..).filter(keep).collect();
                        item.content = (!text.is_empty())
                            .then(|| ContentPart::text(text))
                            .into_iter()
                            .chain(kept)
                            .collect();
                        Ok(())
                    })?;
                    self.queue_changed(&st);
                }
                Ok(Value::Null)
            }
            Request::SteerQueued { session_id, index } => {
                let rt = self.rt(&session_id)?;
                {
                    let mut st = rt.st.lock();
                    self.update_queue(&mut st, |st| {
                        // Steering or being sent now already: it stays as it is.
                        if let (Queue::FollowUp, i) =
                            queued_target(st, index).ok_or_else(no_longer_queued)?
                        {
                            let item = st.followups.remove(i);
                            st.steer.push(item);
                        }
                        Ok(())
                    })?;
                    self.queue_changed(&st);
                }
                Ok(Value::Null)
            }
            Request::InterruptQueued { session_id, index } => {
                let rt = self.rt(&session_id)?;
                // What a turn starting now belongs to (read before locking).
                let stops = self.stops_here();
                let start = {
                    let mut st = rt.st.lock();
                    let start = self.update_queue(&mut st, |st| {
                        let (list, i) = queued_target(st, index).ok_or_else(no_longer_queued)?;
                        let mut item = st.queue(list).remove(i);
                        // Explicit Send now is a new admission for this item,
                        // not authority for any other work already queued.
                        item.stops = stops.clone();
                        let start = interrupt(st, item);
                        if let Some(w) = &start {
                            st.delivering.push((Queue::Now, w.clone()));
                            st.next_turn_stops = Some(w.stops.clone());
                        }
                        Ok(start)
                    })?;
                    self.queue_changed(&st);
                    start
                };
                if let Some(w) = start {
                    self.start_turn(rt, vec![w], Vec::new())?;
                }
                Ok(Value::Null)
            }
            Request::ListBoards { .. }
            | Request::GetBoard { .. }
            | Request::CreateBoard { .. }
            | Request::DeleteBoard { .. }
            | Request::AnswerBoard { .. }
            | Request::BoardFetch { .. }
            | Request::BoardSnapshot { .. }
            | Request::BoardLink { .. } => self.clone().board_request(req, device).await,
            Request::ListNotes
            | Request::GetNote { .. }
            | Request::SaveNote { .. }
            | Request::DeleteNote { .. }
            | Request::SetNoteBots { .. }
            | Request::SetNoteAgents { .. }
            | Request::NoteHistory { .. }
            | Request::GetNoteVersion { .. }
            | Request::PutNoteAsset { .. }
            | Request::ReadNoteAsset { .. }
            | Request::NoteBacklinks { .. }
            | Request::PreviewPage { .. } => self.note_request(req),
            Request::SetDeviceNotify { .. }
            | Request::ListNotifications
            | Request::ListNotificationInbox
            | Request::GetNotificationPolicy
            | Request::SetNotificationPolicy { .. }
            | Request::SetNotificationSource { .. }
            | Request::TestNotification { .. }
            | Request::NotificationDeliveries { .. }
            | Request::ReportNotificationDelivery { .. }
            | Request::SendNotification { .. }
            | Request::PendingNotifications
            | Request::PendingNotificationsV2
            | Request::NegotiateNotifications
            | Request::NotificationPresence { .. }
            | Request::AckNotifications { .. }
            | Request::SeenNotifications { .. }
            | Request::CancelNotification { .. }
            | Request::ClearNotifications { .. }
            | Request::ListSharedFiles { .. }
            | Request::ReadSharedFile { .. } => self.notify_request(req, device),
            Request::GetAppUpdate { .. } | Request::ReadAppUpdate { .. } => {
                crate::updates::Updates::new(&self.paths.data_dir).handle(req)
            }
            Request::GetUsage { .. }
            | Request::SetSubscription { .. }
            | Request::SetModelPrice { .. } => self.usage_request(req).await,
            Request::GetGuide { name } => match crate::guides::get(&name) {
                Some((title, markdown)) => to_value(Guide {
                    name,
                    title: title.into(),
                    markdown: markdown.into(),
                }),
                None => Err(ApiError::new(
                    ErrorCode::NotFound,
                    format!("no guide `{name}`"),
                )),
            },
            Request::Send {
                session_id,
                mut content,
                delivery,
                attachments,
            } => {
                if self.rt(&session_id).is_ok_and(|rt| {
                    let st = rt.st.lock();
                    st.info.parent_call.is_some() && st.internal.subagent.is_none()
                }) {
                    return Err(bad(
                        "This helper works on what the main chat gave it and takes no messages. Write in the main chat instead.",
                    ));
                }
                if !attachments.is_empty() {
                    self.attach(&session_id, &mut content, attachments)?;
                }
                if self.rt(&session_id)?.st.lock().info.private {
                    content = snapshot::checked_private_content(content)?;
                }
                if content.is_empty()
                    || content
                        .iter()
                        .all(|p| matches!(p, ContentPart::Text { text } if text.trim().is_empty()))
                {
                    return Err(bad("message is empty"));
                }
                let notes = self.capture_secrets(&session_id, &mut content)?;
                // Notes the user brings into the chat become readable there.
                self.grant_mentioned_notes(&session_id, &content);
                self.send(&session_id, content, delivery, notes)?;
                Ok(Value::Null)
            }
            Request::Abort { session_id } => {
                let rt = self.rt(&session_id)?;
                // One of Claude Code's subagents: its turn stops it alone.
                let native = {
                    let st = rt.st.lock();
                    st.info.parent_call.is_some() && st.internal.subagent.is_none()
                };
                if native {
                    self.claude_stop_subagent(&rt);
                    return Ok(Value::Null);
                }
                let helpers: Vec<String> = self
                    .subagents_of(&session_id)
                    .into_iter()
                    .filter(|i| {
                        matches!(
                            i.status,
                            SessionStatus::Running | SessionStatus::AwaitingApproval
                        ) || self.rt(&i.id).is_ok_and(|child| child.st.lock().running)
                    })
                    .map(|i| crate::util::one_line(i.title.trim_start_matches("↳ "), 80))
                    .collect();
                if !helpers.is_empty() {
                    self.notice(
                        &rt,
                        NoticeLevel::Info,
                        format!(
                            "Stopping this chat, including {} {}: {}.",
                            helpers.len(),
                            if helpers.len() == 1 {
                                "helper"
                            } else {
                                "helpers"
                            },
                            helpers.join(", ")
                        ),
                    );
                }
                // Record an actual Stop separately from an unanswered card.
                let stopped: Vec<_> = {
                    let mut all = self.approvals.lock();
                    let ids: Vec<_> = all
                        .iter()
                        .filter(|(_, p)| p.shown_on == session_id || p.acting_on == session_id)
                        .map(|(id, _)| id.clone())
                        .collect();
                    ids.into_iter().filter_map(|id| all.remove(&id)).collect()
                };
                for p in stopped {
                    let _ = p.tx.send(Answer {
                        outcome: ApprovalOutcome::Stopped,
                        who: device.unwrap_or("local-client").into(),
                    });
                }
                {
                    let mut st = rt.st.lock();
                    // What was being sent now waits first in the queue
                    // instead, then the steering (as Stop leaves it).
                    if !st.send_now.is_empty() {
                        let mut first = std::mem::take(&mut st.send_now);
                        first.append(&mut st.steer);
                        st.followups.splice(0..0, first);
                        self.queue_changed(&st);
                    }
                    if let Some(c) = &st.cancel {
                        c.cancel();
                        if st.info.kind == SessionKind::Subagent
                            && st.info.status == SessionStatus::Idle
                        {
                            st.info.status = SessionStatus::Interrupted;
                        }
                    }
                }
                // A Tessera helper may be queued, before its agent loop starts.
                if let Some(parent) = rt.st.lock().info.parent_id.clone() {
                    self.stop_background(&parent, Some(&session_id));
                }
                // Stop means stop: including subagents this chat left running,
                // and bots working for it.
                let stopped =
                    self.stop_background(&session_id, None) + self.stop_bot_handoffs(&session_id);
                // A job's run that only waited for them is over.
                self.job_stopped_while_waiting(&rt, stopped);
                // And the Claude Code kept for its next turn: nothing it
                // starts by itself goes on in a chat the user just stopped.
                self.claude.close_for(&session_id);
                Ok(Value::Null)
            }
            Request::Compact {
                session_id,
                instructions,
            } => {
                let rt = self.rt(&session_id)?;
                if rt.st.lock().running {
                    return Err(ApiError::new(
                        ErrorCode::Busy,
                        "the session is running; compact when it is idle",
                    ));
                }
                // Claude Code compacts its own conversation: ask it to now.
                if self.runs_claude_code(&rt.st.lock()) {
                    if rt.st.lock().info.private {
                        return Err(bad(
                            "A private chat on Claude Code keeps no conversation to compact",
                        ));
                    }
                    let text = match instructions.as_deref().map(str::trim) {
                        Some(i) if !i.is_empty() => format!("/compact {i}"),
                        _ => "/compact".to_string(),
                    };
                    self.send(
                        &session_id,
                        vec![ContentPart::text(text)],
                        Delivery::Auto,
                        Vec::new(),
                    )?;
                    return Ok(Value::Null);
                }
                let me = self.clone();
                tokio::spawn(async move {
                    let cancel = CancellationToken::new();
                    let live = me.stops_here();
                    {
                        let mut st = rt.st.lock();
                        st.running = true;
                        // Stop works here too (nothing is saved then).
                        st.cancel = Some(cancel.clone());
                        st.turn_stops = st.next_turn_stops.take().unwrap_or(live);
                    }
                    me.set_status(&rt, SessionStatus::Compacting);
                    let outcome = match me
                        .compact(&rt, true, instructions.as_deref(), &cancel)
                        .await
                    {
                        Err(_) if cancel.is_cancelled() => {
                            me.notice(&rt, NoticeLevel::Info, "Stopped.");
                            Outcome::Aborted
                        }
                        Err(e) => {
                            me.notice(&rt, NoticeLevel::Error, format!("Compaction failed: {e}"));
                            Outcome::Failed(e.to_string())
                        }
                        Ok(()) => Outcome::Done,
                    };
                    // Compact has the same admission/Stop race as a model turn.
                    let here = me.here_now();
                    let next = {
                        let mut st = rt.st.lock();
                        let next = st.finish_admission(&outcome, SessionStatus::Idle, &here);
                        (!next.is_empty()).then_some(next)
                    };
                    match next {
                        // Steering is read at its first step.
                        Some(first) => {
                            me.open_turn(&rt, first);
                            me.queue_changed(&rt.st.lock());
                            me.clone().run(rt, CancellationToken::new()).await;
                        }
                        None => {
                            me.set_status(&rt, SessionStatus::Idle);
                            me.queue_changed(&rt.st.lock());
                        }
                    }
                });
                Ok(Value::Null)
            }
            Request::RespondApproval {
                request_id,
                decision,
                ..
            } => {
                let pending = self.approvals.lock().remove(&request_id);
                match pending {
                    Some(p) => {
                        if tokio::time::Instant::now() >= p.deadline {
                            let _ = p.tx.send(Answer::rule(ApprovalOutcome::Expired));
                            return Err(ApiError::new(
                                ErrorCode::NotFound,
                                "that approval expired; this action was not approved",
                            ));
                        }
                        let outcome = match decision {
                            ApprovalDecision::Allow | ApprovalDecision::AllowSession => {
                                ApprovalOutcome::Allowed(decision)
                            }
                            ApprovalDecision::Deny => ApprovalOutcome::Declined,
                        };
                        let _ = p.tx.send(Answer {
                            outcome,
                            who: device.unwrap_or("local-client").into(),
                        });
                        Ok(Value::Null)
                    }
                    None => Err(ApiError::new(
                        ErrorCode::NotFound,
                        "that approval is no longer pending",
                    )),
                }
            }
            Request::Search { query, limit } => to_value(
                self.store
                    .search(&query, None, limit.unwrap_or(30).min(100))
                    .map_err(internal)?,
            ),
            Request::ListProviders { refresh } => {
                if refresh {
                    let (errors, ()) =
                        futures::join!(self.providers.discover(None), self.claude_code_refresh());
                    for (id, e) in errors {
                        tracing::warn!("model discovery for {id} failed: {e}");
                    }
                } else if self.providers.any_offline() {
                    // Someone looks at the models: a server that was off may
                    // be back (clients hear of it when it is).
                    let me = self.clone();
                    tokio::spawn(async move {
                        let gap = std::time::Duration::from_secs(10);
                        if me.providers.recheck_offline(gap).await {
                            let _ = me.events.send(Event::ProvidersChanged);
                        }
                    });
                }
                to_value(self.with_small_modes(self.providers.infos()))
            }
            Request::SetModelSmall {
                provider,
                model,
                mode,
            } => self.set_model_small(&provider, &model, mode),
            Request::SaveProvider { spec } => to_value(self.save_provider(spec).await?),
            Request::RemoveProvider { id } => {
                self.providers.remove(&id);
                let _ = self.auth.remove(&id);
                self.persist_providers().map_err(internal)?;
                let _ = self.events.send(Event::ProvidersChanged);
                Ok(Value::Null)
            }
            Request::StartOauth { provider } => to_value(self.start_oauth(&provider).await?),
            Request::CompleteOauth { provider, input } => {
                if provider == CODEX_PROVIDER && input.trim() == "import-codex-cli" {
                    let cred = crate::auth::import_codex_cli().map_err(|e| bad(e.to_string()))?;
                    self.auth.set(CODEX_PROVIDER, cred).map_err(internal)?;
                    self.providers.discover(Some(CODEX_PROVIDER)).await;
                    let _ = self.events.send(Event::ProvidersChanged);
                    return to_value(
                        self.providers
                            .infos()
                            .into_iter()
                            .find(|p| p.id == provider),
                    );
                }
                Err(bad(
                    "this provider signs in with a device code; the flow completes automatically",
                ))
            }
            Request::Logout { provider } => {
                self.auth.remove(&provider).map_err(internal)?;
                let _ = self.events.send(Event::ProvidersChanged);
                Ok(Value::Null)
            }
            Request::SetDefaultModel { model } => {
                {
                    let mut c = self.config.write();
                    c.default_model = Some(model.to_string());
                    // An explicit choice replaces a `[models] default` too.
                    c.models.roles.remove("default");
                    c.save(&self.paths).map_err(internal)?;
                }
                let _ = self.events.send(Event::ProvidersChanged);
                Ok(Value::Null)
            }
            Request::GetModelRoles => to_value(self.model_roles()),
            Request::SetModelRole { role, model } => {
                if let Some(m) = model
                    .as_deref()
                    .and_then(|m| m.trim().parse::<ModelRef>().ok())
                    && m.as_role().is_none()
                    && !self.providers.exists(&m.provider)
                {
                    return Err(bad(format!("unknown provider `{}`", m.provider)));
                }
                {
                    let mut c = self.config.write();
                    c.set_role(&role, model.as_deref()).map_err(bad)?;
                    c.save(&self.paths).map_err(internal)?;
                }
                let _ = self.events.send(Event::ProvidersChanged);
                to_value(self.model_roles())
            }
            Request::ListBots => to_value(self.all_bots()),
            Request::SaveBot { bot } => to_value(self.save_bot(bot)?),
            Request::DeleteBot { id } => {
                if !self.store.delete_bot(&id).map_err(internal)? {
                    return Err(ApiError::new(ErrorCode::NotFound, "no such bot"));
                }
                self.bots.write().retain(|b| b.id != id);
                self.auth.forget(&id);
                // Its duties stop with it (kept, switched off): run without
                // the bot they would have had a plain chat's reach.
                for mut job in self.store.jobs().map_err(internal)? {
                    if job.bot_id.as_deref() == Some(id.as_str()) && job.enabled {
                        job.enabled = false;
                        self.store.put_job(&job).map_err(internal)?;
                        let _ = self.events.send(Event::JobUpdated { job });
                    }
                }
                let _ = self.events.send(Event::BotRemoved { id });
                Ok(Value::Null)
            }
            Request::CreateChannel {
                title,
                members,
                cwd,
            } => {
                let members = self.resolve_members(&members)?;
                if members.is_empty() {
                    return Err(bad("a channel needs at least one bot"));
                }
                let title = if title.trim().is_empty() {
                    "Channel".to_string()
                } else {
                    one_line(&title, 80)
                };
                let rt = self
                    .create_session(
                        cwd,
                        None,
                        Some(title),
                        SessionKind::Channel,
                        None,
                        None,
                        None,
                    )
                    .map_err(|e| bad(e.to_string()))?;
                rt.st.lock().info.members = members;
                self.save(&rt);
                to_value(rt.st.lock().info.clone())
            }
            Request::GetSecurity => self.security_request(req),
            Request::SetSecurity { .. } => {
                let r = self.security_request(req);
                // Strict now: full-access terminals end (engine/terminals.rs).
                self.terminals_follow_sandbox();
                r
            }
            Request::ListSecrets
            | Request::SaveSecret { .. }
            | Request::SaveChatSecret { .. }
            | Request::SetSecret { .. }
            | Request::SetSecretTitle { .. }
            | Request::SuggestSecretTitles
            | Request::DeleteSecret { .. }
            | Request::DecideChatSecret { .. } => self.secret_request(req),
            Request::GetRemoteAccess => to_value(self.remote_info()),
            // Header values are credentials: given out only by the server's
            // connection handling, to the user's apps on an encrypted link.
            Request::GetServerRoute => Err(ApiError::new(
                ErrorCode::Unauthorized,
                "only the Tessera apps' own connections get the server's addresses",
            )),
            Request::RetireLogin { token } => {
                self.retire_login(&token, device)?;
                Ok(Value::Null)
            }
            Request::GetMosaics => to_value(self.mosaics()),
            Request::SaveMosaics { layout } => to_value(self.save_mosaics(layout, device)?),
            Request::SetupStatus => to_value(self.setup_status()),
            Request::CompleteSetup => {
                self.store
                    .kv_set("setup_completed", "1")
                    .map_err(internal)?;
                Ok(Value::Null)
            }
            Request::DetectLocalModels => {
                // Asked for: this machine and the home network.
                for id in self.autodetect_local(true).await {
                    self.providers.discover(Some(&id)).await;
                }
                let _ = self.events.send(Event::ProvidersChanged);
                to_value(self.providers.infos())
            }
            Request::ClaudeCodeStatus { refresh, provider } => to_value(
                self.claude_code_status(refresh, provider.as_deref())
                    .await?,
            ),
            Request::AddAccount { kind, name } => to_value(self.add_account(kind, &name).await?),
            Request::RenameProvider { id, name } => to_value(self.rename_provider(&id, &name)?),
            Request::GetAccounts => to_value(self.account_settings()),
            Request::SetAccounts { continue_on_next } => {
                {
                    let mut c = self.config.write();
                    c.accounts.continue_on_next = continue_on_next;
                    c.save(&self.paths).map_err(internal)?;
                }
                to_value(self.account_settings())
            }
            Request::CloudflareZones { token } => to_value(
                crate::remote::Cloudflare::new(self.providers.http().clone(), &token)
                    .zones()
                    .await
                    .map_err(|e| bad(e.to_string()))?,
            ),
            Request::CloudflareSetup {
                token,
                zone_id,
                hostname,
            } => to_value(
                self.clone()
                    .cloudflare_setup(&token, &zone_id, &hostname)
                    .await?,
            ),
            Request::CloudflareRemove { token } => to_value(self.cloudflare_remove(&token).await?),
            Request::CloudflareRenew { token } => {
                to_value(self.cloudflare_renew(token.as_deref()).await?)
            }
            Request::RemoteStatus => to_value(self.remote_status()),
            Request::GetWebSearch => to_value(self.web_search_info()),
            Request::SetSearchKey { provider, key } => {
                let provider = provider.trim().to_ascii_lowercase();
                if !crate::tools::web::KEYED.contains(&provider.as_str()) {
                    return Err(bad(format!(
                        "search keys are for {}",
                        crate::tools::web::KEYED.join(" or ")
                    )));
                }
                let id = crate::tools::web::key_id(&provider);
                match key.trim() {
                    "" => self.auth.remove(&id),
                    k => self
                        .auth
                        .set(&id, Credential::ApiKey { key: k.to_string() }),
                }
                .map_err(internal)?;
                to_value(self.web_search_info())
            }
            Request::SaveWebSearch {
                search,
                searxng_url,
                headers,
            } => {
                let search = search.trim().to_ascii_lowercase();
                if !matches!(
                    search.as_str(),
                    "auto" | "duckduckgo" | "searxng" | "brave" | "tavily"
                ) {
                    return Err(bad(
                        "search must be `auto`, `duckduckgo`, `searxng`, `brave` or `tavily`",
                    ));
                }
                let url = searxng_url
                    .map(|u| u.trim().trim_end_matches('/').to_string())
                    .filter(|u| !u.is_empty());
                if search == "searxng" {
                    match url.as_deref().map(url::Url::parse) {
                        Some(Ok(u)) if matches!(u.scheme(), "http" | "https") => {}
                        _ => return Err(bad("SearXNG needs an http(s) URL")),
                    }
                }
                if let Some(h) = headers {
                    self.auth
                        .set_search_headers(
                            h.into_iter()
                                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                                .filter(|(k, _)| !k.is_empty())
                                .collect(),
                        )
                        .map_err(internal)?;
                }
                {
                    let mut c = self.config.write();
                    c.web.search = search;
                    c.web.searxng_url = url;
                    c.save(&self.paths).map_err(internal)?;
                }
                to_value(self.web_search_info())
            }
            Request::ServerKey => {
                let key =
                    tessera_secure::Identity::public_at(&self.paths.data_dir.join("identity.key"))
                        .ok_or_else(|| {
                            ApiError::new(
                                ErrorCode::NotFound,
                                "this server has no key yet (run `tessera serve`)",
                            )
                        })?;
                to_value(tessera_proto::ServerKeyInfo {
                    key: tessera_secure::b64(&key),
                    fingerprint: tessera_secure::fingerprint(&key),
                })
            }
            Request::SetServerBind { bind } => {
                let addr: std::net::SocketAddr = bind
                    .trim()
                    .parse()
                    .map_err(|_| bad("use an address like 127.0.0.1:7878 or 0.0.0.0:7878"))?;
                if addr.port() == 0 {
                    return Err(bad("choose a port"));
                }
                {
                    let mut c = self.config.write();
                    c.server.bind = addr.to_string();
                    c.save(&self.paths).map_err(internal)?;
                }
                to_value(self.setup_status())
            }
            Request::SetWebAccess { on, token } => to_value(self.set_web_access(on, token).await?),
            Request::SetRemoteAccess {
                urls,
                headers,
                add_headers,
                remove_headers,
            } => {
                let mut remote = self.auth.remote();
                remote.urls = urls
                    .into_iter()
                    .map(|u| u.trim().trim_end_matches('/').to_string())
                    .filter(|u| !u.is_empty())
                    .collect();
                let clean = |h: std::collections::BTreeMap<String, String>| {
                    h.into_iter()
                        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                        .filter(|(k, _)| !k.is_empty())
                        .collect::<std::collections::BTreeMap<_, _>>()
                };
                if let Some(h) = headers {
                    remote.headers = clean(h);
                }
                if let Some(h) = add_headers {
                    remote.headers.extend(clean(h));
                }
                for k in remove_headers {
                    remote.headers.remove(k.trim());
                }
                self.auth
                    .set_remote(remote)
                    .map_err(|e| bad(e.to_string()))?;
                self.route_changed();
                to_value(self.remote_info())
            }
            Request::GetVoice => to_value(self.voice_config()),
            Request::SaveVoice { config, api_key } => {
                to_value(self.save_voice(config, api_key).await?)
            }
            Request::Transcribe {
                audio,
                mime,
                engine,
                language,
                partial,
                operation: _,
            } => to_value(
                self.transcribe(
                    &audio,
                    &mime,
                    engine.as_deref(),
                    partial,
                    language.as_deref(),
                )
                .await?,
            ),
            Request::Speak { text, engine, .. } => {
                to_value(self.speak(&text, engine.as_deref()).await?)
            }
            Request::CancelVoice { .. } => {
                Err(bad("Voice operations belong to their device's connection."))
            }
            Request::InstallVoice => to_value(self.install_voice()?),
            Request::DeleteVoiceRecordings => to_value(self.delete_voice_recordings()?),
            Request::ListJobs => to_value(self.store.jobs().map_err(internal)?),
            Request::SaveJob { job } => {
                to_value(self.save_job(job).map_err(|e| bad(e.to_string()))?)
            }
            Request::DeleteJob { id } => {
                let gate = self.job_gate(&id);
                let _admission = gate.lock();
                self.store.delete_job(&id).map_err(internal)?;
                let _ = self.store.forget_job_runs(&id);
                let _ = self.events.send(Event::JobRemoved { id });
                Ok(Value::Null)
            }
            Request::ListJobRuns { id } => to_value(self.store.job_runs(&id).map_err(internal)?),
            Request::RunJob { id } => {
                let job = self
                    .store
                    .jobs()
                    .map_err(internal)?
                    .into_iter()
                    .find(|j| j.id == id)
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such job"))?;
                let info = self.clone().run_job(job).map_err(internal)?;
                to_value(info)
            }
            Request::GetMemory => to_value(self.memory.state()),
            Request::SetMemory { target, content } => {
                self.memory
                    .set(target, &content)
                    .map_err(|e| bad(e.to_string()))?;
                to_value(self.memory.state())
            }
            Request::ListSkills { cwd } => {
                let cwd = cwd
                    .map(|c| crate::util::expand_home(&c))
                    .unwrap_or_else(|| self.default_cwd());
                to_value(
                    self.skills_for(&cwd)
                        .iter()
                        .map(|s| s.info())
                        .collect::<Vec<_>>(),
                )
            }
            Request::ReadSkill { name, cwd } => {
                let cwd = cwd
                    .map(|c| crate::util::expand_home(&c))
                    .unwrap_or_else(|| self.default_cwd());
                let skill = self
                    .skills_for(&cwd)
                    .into_iter()
                    .find(|s| s.name == name)
                    .ok_or_else(|| {
                        ApiError::new(ErrorCode::NotFound, format!("no skill `{name}`"))
                    })?;
                let text = std::fs::read_to_string(&skill.file).map_err(internal)?;
                to_value(crate::util::truncate(&text, 20_000))
            }
            Request::DeleteSkill { name, cwd } => {
                let cwd = cwd
                    .map(|c| crate::util::expand_home(&c))
                    .unwrap_or_else(|| self.default_cwd());
                let (roots, skill) = {
                    let cfg = self.config.read();
                    let roots = crate::skills::roots(
                        &cwd,
                        &self.paths.skills_dir(),
                        &cfg.skills.extra_dirs,
                    );
                    let skill = crate::skills::discover_protected(
                        &roots,
                        &cfg.skills.disabled,
                        &self.protected_paths(),
                    )
                    .into_iter()
                    .find(|s| s.name == name);
                    (roots, skill)
                };
                let skill = skill.ok_or_else(|| {
                    ApiError::new(ErrorCode::NotFound, format!("no skill `{name}`"))
                })?;
                crate::skills::remove(&skill, &roots).map_err(bad)?;
                // Chats already running keep the prompt they have; the next
                // new chat (or compaction) no longer lists it.
                Ok(Value::Null)
            }
            Request::ListDevices | Request::DeviceHello { .. } | Request::RenameDevice { .. } => {
                self.devices_request(req, device)
            }
            Request::RevokeDevice { id } => {
                self.revoke_device(&id)?;
                Ok(Value::Null)
            }
            Request::ListMachines
            | Request::SetMachinePaused { .. }
            | Request::RevokeMachine { .. } => self.machines_request(req),
            Request::NodeHello { .. }
            | Request::NodeScope { .. }
            | Request::NodeProgress { .. }
            | Request::NodeReply { .. }
            | Request::NodeAttestation { .. }
            | Request::NodeBrowser { .. }
            | Request::NodeTerminal { .. } => Err(bad(
                "only a machine's own connection (`tessera node`) can send this",
            )),
            Request::GetHelpers => to_value(self.helper_settings()),
            Request::SetHelpers {
                at_once,
                enabled,
                steps,
                minutes,
            } => {
                self.set_helpers(at_once, enabled, steps, minutes)?;
                to_value(self.helper_settings())
            }
            Request::SetChatHelpers { session_id, limits } => {
                to_value(self.set_chat_helpers(&session_id, limits)?)
            }
            Request::TerminalStatus => to_value(self.terminal_status(device, false)),
            Request::SetTerminalSettings { settings } => {
                self.set_terminal_settings(settings, device)?;
                to_value(self.terminal_status(device, false))
            }
            // Only a paired device's own connection gets a terminal
            // (`Engine::terminal_request`): never a chat, tool or job.
            Request::TerminalOpen { .. }
            | Request::TerminalAttach { .. }
            | Request::TerminalDetach { .. }
            | Request::TerminalInput { .. }
            | Request::TerminalResize { .. }
            | Request::TerminalAck { .. }
            | Request::TerminalClose { .. } => Err(ApiError::new(
                ErrorCode::Unauthorized,
                "terminals open only from one of your paired devices",
            )),
            Request::MachineLogin => {
                // The connection checked it is a device's own login.
                let id = device
                    .ok_or_else(|| bad("only a paired device can ask for its machine's login"))?;
                let token = self
                    .store
                    .machine_login(id)
                    .map_err(internal)?
                    .ok_or_else(|| {
                        ApiError::new(
                            ErrorCode::Unauthorized,
                            "only one of your devices can ask for a machine login",
                        )
                    })?;
                tracing::info!("device {id} got a new machine login");
                to_value(MachineLoginGrant {
                    device_id: id.to_string(),
                    token,
                })
            }
            Request::CreatePairing => {
                let (code, expires_at) = self
                    .store
                    .create_pairing_code(10 * 60_000)
                    .map_err(internal)?;
                // The key file belongs to `tessera serve` (tessera-server).
                let fingerprint = tessera_secure::Identity::fingerprint_at(
                    &self.paths.data_dir.join("identity.key"),
                );
                let link = self
                    .pair_link(&code, fingerprint.clone())
                    .map(|l| l.to_link());
                to_value(PairingCode {
                    code,
                    expires_at,
                    fingerprint,
                    link,
                })
            }
            Request::ListDir { path } => to_value(list_dir(&path).map_err(|e| bad(e.to_string()))?),
            Request::CreateDir { parent, name } => {
                let made = create_dir(&parent, &name, &self.protected_paths())?;
                tracing::info!(
                    "folder {} made from the folder picker (device {})",
                    made.path,
                    device.unwrap_or("-")
                );
                to_value(made)
            }
            Request::MemoryReport => to_value(self.memory_report().await),
            Request::BrowserAttach { .. }
            | Request::BrowserDetach { .. }
            | Request::BrowserInput { .. }
            | Request::BrowserNav { .. }
            | Request::BrowserPage { .. }
            | Request::BrowserNewTab { .. }
            | Request::BrowserUpload { .. }
            | Request::BrowserAck { .. }
            | Request::BrowserDialogAnswer { .. }
            | Request::BrowserChooseFiles { .. }
            | Request::BrowserToServer { .. }
            | Request::BrowserShowOnScreen { .. }
            | Request::BrowserWatch { .. } => self.browser_request(req, None).await,
            Request::GetBrowser => to_value(self.browser.info().await),
            Request::SetBrowser {
                enabled,
                headless,
                quality,
            } => {
                let was = self.config.read().browser.clone();
                {
                    let mut c = self.config.write();
                    if let Some(e) = enabled {
                        c.browser.enabled = e;
                    }
                    if let Some(h) = headless {
                        c.browser.headless = h;
                    }
                    if let Some(q) = quality {
                        c.browser.quality = q;
                    }
                    if enabled.is_some() || headless.is_some() || quality.is_some() {
                        c.save(&self.paths).map_err(internal)?;
                    }
                }
                if headless.is_some() || enabled == Some(false) {
                    // Not in use: stop it so the next start uses the new setting.
                    self.browser.stop_if_unwatched().await;
                } else if quality.is_some_and(|q| q != was.quality) {
                    self.browser.quality_changed().await;
                }
                to_value(self.browser.info().await)
            }
        }
    }

    fn server_info(&self) -> ServerInfo {
        // Copy what we need and release the lock before calling helpers that
        // take it again (parking_lot read locks are not re-entrant).
        let name = self.config.read().server.name.clone();
        ServerInfo {
            name,
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            session_list_pages: true,
            home: self.default_cwd().display().to_string(),
            default_model: self.default_model(),
            voice: self.voice_ready(),
            voice_engine: Some(self.voice_engine()),
        }
    }

    fn invalidate_prompts(&self) {
        self.invalidate_prompts_preserving_claude(None);
    }

    /// Secure app entry need not reset a safe Claude conversation. Other
    /// chats still refresh; native prompts always reflect the new vault.
    fn invalidate_prompts_preserving_claude(&self, keep: Option<&str>) {
        for rt in self.live_sessions() {
            let mut st = rt.st.lock();
            st.internal.system_prompt = None;
            st.internal.bot_prompts.clear();
            if keep != Some(st.info.id.as_str()) {
                st.internal.claude_secure_entry = false;
                claude_code::renew_claude_context(&mut st.internal);
                st.internal.claude_context_epoch = st.internal.claude_context_epoch.wrapping_add(1);
            }
        }
    }

    fn web_search_info(&self) -> tessera_proto::WebSearchInfo {
        let w = self.config.read().web.clone();
        tessera_proto::WebSearchInfo {
            search: w.search,
            searxng_url: w.searxng_url,
            header_names: self.auth.search_headers().into_keys().collect(),
            keys: crate::tools::web::KEYED
                .iter()
                .filter(|p| self.auth.get(&crate::tools::web::key_id(p)).is_some())
                .map(|p| p.to_string())
                .collect(),
        }
    }

    fn mosaics(&self) -> MosaicLayout {
        self.store
            .kv_get("mosaics")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(MosaicLayout {
                version: 0,
                mosaics: Vec::new(),
            })
    }

    /// Replace the shared mosaic layout (optimistic concurrency on `version`).
    fn save_mosaics(
        &self,
        mut layout: MosaicLayout,
        _device: Option<&str>,
    ) -> Result<MosaicLayout, ApiError> {
        let _g = self.mosaics_lock.lock();
        let current = self.mosaics();
        if layout.version != current.version {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "the mosaic layout changed on another device; fetch it and try again",
            ));
        }
        if layout.mosaics.len() > 32 || layout.mosaics.iter().any(|m| m.tiles.len() > 48) {
            return Err(bad("too many mosaics or tiles"));
        }
        for m in &mut layout.mosaics {
            m.name = one_line(&m.name, 60);
            // A tile of a whiteboard or note that no longer exists is dropped.
            m.tiles.retain(|t| {
                t.board_id
                    .as_deref()
                    .is_none_or(|b| self.store.board(b).ok().flatten().is_some())
                    && t.note_id
                        .as_deref()
                        .is_none_or(|n| self.store.note(n).ok().flatten().is_some())
            });
            // Private chats are never written to disk, not even as a tile.
            for t in &mut m.tiles {
                if t.session_id
                    .as_deref()
                    .is_some_and(|s| self.store.is_private(s))
                {
                    t.session_id = None;
                }
            }
            if m.geometry
                .as_ref()
                .is_some_and(|g| g.to_string().len() > 64 << 10)
            {
                m.geometry = None;
            }
        }
        layout.version = current.version + 1;
        self.store
            .kv_set(
                "mosaics",
                &serde_json::to_string(&layout).map_err(internal)?,
            )
            .map_err(internal)?;
        let _ = self.events.send(Event::MosaicsChanged {
            layout: layout.clone(),
        });
        Ok(layout)
    }

    fn setup_status(&self) -> SetupStatus {
        let cfg = self.config.read().clone();
        // Ready means it can answer: a fresh server falls back to the
        // built-in ChatGPT provider, which isn't signed in yet, and setup
        // must not say the AI is done then.
        let signed_in: HashSet<String> = self
            .providers
            .infos()
            .into_iter()
            .filter(|p| p.authenticated)
            .map(|p| p.id)
            .collect();
        let default_model = self
            .default_model()
            .filter(|m| signed_in.contains(&m.provider) && self.providers.model_info(m).is_some());
        let bind = self
            .listen
            .read()
            .clone()
            .unwrap_or(cfg.server.bind.clone());
        let lan = bind
            .parse::<std::net::SocketAddr>()
            .is_ok_and(|a| !a.ip().is_loopback());
        let permission = cfg.new_chat_permission();
        SetupStatus {
            completed: self
                .store
                .kv_get("setup_completed")
                .ok()
                .flatten()
                .is_some(),
            has_model: default_model.is_some(),
            default_model,
            web_search: cfg.web.search.clone(),
            voice: cfg.voice.engine() != "off",
            remote: !self.auth.remote().urls.is_empty(),
            devices: self
                .store
                .devices(None)
                .map(|d| {
                    d.iter()
                        .filter(|d| !d.name.starts_with("Terminal (this machine)"))
                        .count() as u32
                })
                .unwrap_or(0),
            sandbox: crate::sandbox::describe(),
            server_name: cfg.server.name,
            lan,
            restart_needed: bind != cfg.server.bind,
            bind: cfg.server.bind,
            e2ee_required: cfg.security.require_e2ee,
            permission,
        }
    }

    fn local_url(&self) -> String {
        let bind = self
            .listen
            .read()
            .clone()
            .unwrap_or(self.config.read().server.bind.clone());
        let port = bind
            .parse::<std::net::SocketAddr>()
            .map(|a| a.port())
            .unwrap_or(7878);
        format!("http://localhost:{port}")
    }

    fn remote_status(&self) -> RemoteStatus {
        let remote = self.auth.remote();
        let bind = self
            .listen
            .read()
            .clone()
            .unwrap_or(self.config.read().server.bind.clone());
        let tunnel = self.auth.tunnel().map(|t| {
            let st = self
                .tunnel
                .lock()
                .as_ref()
                .map(|s| s.state())
                .unwrap_or_default();
            TunnelStatus {
                hostname: t.hostname,
                tunnel_id: t.tunnel_id,
                running: st.running,
                connected: st.connected,
                error: st.error,
                key_expires_at: t.key_expires_at,
                renewable: true,
            }
        });
        RemoteStatus {
            lan_urls: crate::util::lan_urls(&bind),
            web: Some(self.web_access_status(&remote.urls)),
            urls: remote.urls,
            header_names: remote.headers.into_keys().collect(),
            tunnel,
        }
    }

    /// One-stop remote access through Cloudflare: tunnel, DNS, Access with a
    /// service token, cloudflared supervised by Tessera, and the address and
    /// headers recorded for pairing QR codes. The API token is not kept.
    async fn cloudflare_setup(
        self: Arc<Self>,
        token: &str,
        zone_id: &str,
        hostname: &str,
    ) -> Result<RemoteStatus, ApiError> {
        let progress = |step: &str| {
            let _ = self.events.send(Event::SetupProgress {
                task: "cloudflare".into(),
                step: step.into(),
                done: false,
                error: None,
            });
        };
        let fail = |e: anyhow::Error| {
            let msg = e.to_string();
            let _ = self.events.send(Event::SetupProgress {
                task: "cloudflare".into(),
                step: "failed".into(),
                done: true,
                error: Some(msg.clone()),
            });
            bad(msg)
        };
        // One setup at a time: two devices must not both create a tunnel.
        static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let Ok(_busy) = BUSY.try_lock() else {
            return Err(bad("remote access is already being set up"));
        };
        if self.auth.tunnel().is_some() {
            return Err(bad(
                "remote access through Cloudflare is already set up; remove it first to change it",
            ));
        }
        let cf = crate::remote::Cloudflare::new(self.providers.http().clone(), token);
        let zones = cf.zones().await.map_err(fail)?;
        let zone = zones
            .into_iter()
            .find(|z| z.id == zone_id)
            .ok_or_else(|| bad("that domain isn't available to this token"))?;
        // Get cloudflared first, so a failed download leaves nothing behind
        // in the Cloudflare account.
        progress("Installing cloudflared");
        let binary = crate::remote::ensure_cloudflared(&self.paths.data_dir, &progress)
            .await
            .map_err(fail)?;
        let record = cf
            .setup(&zone, hostname, &self.local_url(), &progress)
            .await
            .map_err(fail)?;
        self.auth
            .set_tunnel(Some(record.clone()))
            .map_err(internal)?;
        let mut remote = self.auth.remote();
        let url = format!("https://{}", record.hostname);
        if !remote.urls.contains(&url) {
            remote.urls.push(url);
        }
        remote
            .headers
            .insert("CF-Access-Client-Id".into(), record.client_id.clone());
        remote.headers.insert(
            "CF-Access-Client-Secret".into(),
            record.client_secret.clone(),
        );
        self.auth.set_remote(remote).map_err(internal)?;
        self.route_changed();
        progress("Starting the tunnel");
        self.start_tunnel(binary);
        let _ = self.events.send(Event::SetupProgress {
            task: "cloudflare".into(),
            step: "Remote access is ready".into(),
            done: true,
            error: None,
        });
        Ok(self.remote_status())
    }

    async fn cloudflare_remove(&self, token: &str) -> Result<RemoteStatus, ApiError> {
        // A second device must not skip the connector another removal is waiting on.
        static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let Ok(_busy) = BUSY.try_lock() else {
            return Err(bad("remote access is already being removed"));
        };
        let Some(record) = self.auth.tunnel() else {
            return Err(bad("remote access through Cloudflare isn't set up"));
        };
        let connector = self.tunnel.lock().take();
        if let Some(mut connector) = connector
            && let Err(e) = connector.stop_and_wait().await
        {
            *self.tunnel.lock() = Some(connector);
            return Err(bad(format!(
                "Remote access was not removed. {e}. Cloudflare protection is unchanged"
            )));
        }
        crate::remote::Cloudflare::new(self.providers.http().clone(), token)
            .remove(&record)
            .await
            .map_err(|e| bad(e.to_string()))?;
        self.auth.set_tunnel(None).map_err(internal)?;
        let mut remote = self.auth.remote();
        remote
            .urls
            .retain(|u| u != &format!("https://{}", record.hostname));
        remote.headers.remove("CF-Access-Client-Id");
        remote.headers.remove("CF-Access-Client-Secret");
        self.auth.set_remote(remote).map_err(internal)?;
        self.route_changed();
        Ok(self.remote_status())
    }

    /// Renew the key the devices carry through Cloudflare Access: with an
    /// API token (Cloudflare's "Refresh", used once and never stored), or
    /// `None` when the user did it in Cloudflare's dashboard themselves
    /// (a year from now). The key stays the same, so devices need nothing.
    async fn cloudflare_renew(&self, token: Option<&str>) -> Result<RemoteStatus, ApiError> {
        let Some(mut record) = self.auth.tunnel() else {
            return Err(bad("remote access through Cloudflare isn't set up"));
        };
        let expires = match token.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => crate::remote::Cloudflare::new(self.providers.http().clone(), t)
                .renew(&record)
                .await
                .map_err(|e| bad(e.to_string()))?,
            None => {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    + crate::remote::KEY_LIFETIME_SECS
            }
        };
        record.key_expires_at = Some(expires);
        self.auth.set_tunnel(Some(record)).map_err(internal)?;
        tracing::info!("remote access: the devices' Cloudflare key was renewed");
        Ok(self.remote_status())
    }

    /// The devices' Cloudflare key (the Access service token) lasts a
    /// year. From 30 days before it runs out, tell the user's devices: at
    /// 30, 7 and 1 day(s) left, and once it has run out, each once per
    /// date (a renewal starts afresh). Cheap: the scheduler asks often.
    pub(crate) fn remind_remote_key(&self) {
        let Some(record) = self.auth.tunnel() else {
            return;
        };
        let Some(expires) = record.key_expires_at else {
            return;
        };
        let now = now_ms() / 1000;
        let Some(stage) = crate::remote::key_reminder_stage(expires, now) else {
            return;
        };
        let kv = format!("remote_key_reminded:{expires}");
        let done: u8 = self
            .store
            .kv_get(&kv)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if stage <= done || self.store.kv_set(&kv, &stage.to_string()).is_err() {
            return;
        }
        let (title, body) = crate::remote::key_reminder_text(stage, expires.saturating_sub(now));
        let _ = self.send_notification(crate::store::NewNotification {
            title: title.into(),
            body,
            from: "Tessera".into(),
            session_id: None,
            urgent: stage >= 3,
            due_at: now_ms(),
            kind: "system".into(),
            ..Default::default()
        });
    }

    /// Run (and keep running) cloudflared for the recorded tunnel.
    fn start_tunnel(&self, binary: PathBuf) {
        let Some(record) = self.auth.tunnel() else {
            return;
        };
        let token_file = self.paths.data_dir.join("tunnel.token");
        if crate::config::write_atomic(&token_file, record.run_token.as_bytes(), true).is_err() {
            return;
        }
        let mut slot = self.tunnel.lock();
        if let Some(old) = slot.take() {
            old.stop();
        }
        *slot = Some(crate::remote::Supervisor::start(binary, token_file));
    }

    /// At server start: bring the tunnel back up if Tessera manages one.
    /// cloudflared gone (removed by hand or by an antivirus, or a data
    /// folder moved to another computer): download and check it again, as
    /// the setup did, rather than staying unreachable from outside.
    pub(crate) fn resume_tunnel(self: &Arc<Self>) {
        if self.auth.tunnel().is_none() {
            return;
        }
        if let Some(bin) = crate::remote::find_cloudflared(&self.paths.data_dir) {
            self.start_tunnel(bin);
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let got = crate::remote::ensure_cloudflared(&me.paths.data_dir, &|step: &str| {
                tracing::info!("remote access: {step}")
            })
            .await;
            match got {
                Ok(bin) => me.start_tunnel(bin),
                Err(e) => {
                    tracing::warn!("remote access is set up, but cloudflared is missing: {e}")
                }
            }
        });
    }

    fn remote_info(&self) -> tessera_proto::RemoteAccessInfo {
        let r = self.auth.remote();
        tessera_proto::RemoteAccessInfo {
            urls: r.urls,
            header_names: r.headers.into_keys().collect(),
        }
    }

    /// How the user's devices reach this server: its home addresses, then
    /// the remote ones, and the headers those need (Cloudflare Access). What
    /// a pairing code carries, and what paired apps keep up with
    /// (`get_server_route`).
    pub fn server_route(&self) -> tessera_proto::ServerRoute {
        let bind = self
            .listen
            .read()
            .clone()
            .unwrap_or_else(|| self.config.read().server.bind.clone());
        let remote = self.auth.remote();
        let mut urls = crate::util::lan_urls(&bind);
        for u in remote.urls {
            if !urls.contains(&u) {
                urls.push(u);
            }
        }
        tessera_proto::ServerRoute {
            urls,
            headers: remote.headers,
        }
    }

    /// How devices reach this server changed: paired apps fetch it again.
    fn route_changed(&self) {
        let _ = self.events.send(Event::ServerRouteChanged);
    }

    /// Sign a device out: its open connections close, a machine using it
    /// stops and its terminals end, at once.
    fn revoke_device(&self, id: &str) -> Result<(), ApiError> {
        self.store.revoke_device(id).map_err(internal)?;
        // Its open connections close, and a machine using it stops, at once.
        let _ = self.revoked.send(id.to_string());
        self.revoke_machine(id);
        // Its terminals end at once, here and on machines.
        self.terminals_signed_out(id);
        // Its private chats go with it: no other device may ever see them.
        self.private_signed_out(id);
        Ok(())
    }

    /// `retire_login`: `device` paired again and replaced its saved entry;
    /// the login it had before (`token`) goes, handing over the name the
    /// user gave it and whether it took notifications. Only an ordinary
    /// device's login, and never the caller's own.
    fn retire_login(&self, token: &str, device: Option<&str>) -> Result<(), ApiError> {
        let Some(me) = device else {
            return Err(bad("only a paired device can do this"));
        };
        let Some(old) = self.store.login(token.trim()).map_err(internal)? else {
            return Ok(()); // already gone
        };
        if old.machine || old.browser_key.is_some() || old.device == me {
            return Err(bad("that login isn't one of this device's earlier ones"));
        }
        // Its computer also serves as a machine under it: signing it out
        // would turn that machine off. It stays; the user can sign it out.
        let serves = self
            .store
            .has_machine_login(&old.device)
            .map_err(internal)?
            || self.machines.has(&old.device);
        if serves {
            return Ok(());
        }
        let devices = self.store.devices(None).map_err(internal)?;
        if let Some(prev) = devices.iter().find(|d| d.id == old.device) {
            if prev.named {
                let _ = self.store.rename_device(me, &prev.name);
            }
            if !prev.notify {
                let _ = self.store.set_device_notify(me, false);
            }
        }
        // The same device holds them: its private chats stay with it.
        for id in self.store.private_of(&old.device) {
            self.store.set_private_owner(&id, Some(me));
        }
        self.revoke_device(&old.device)?;
        tracing::info!("a device paired again; its earlier entry was signed out");
        Ok(())
    }

    /// Everything a new device needs, for a QR code. `None` when there is no
    /// address another device could use.
    pub fn pair_link(
        &self,
        code: &str,
        fingerprint: Option<String>,
    ) -> Option<tessera_proto::PairLink> {
        let route = self.server_route();
        if route.urls.is_empty() {
            return None;
        }
        Some(tessera_proto::PairLink {
            name: self.config.read().server.name.clone(),
            urls: route.urls,
            headers: route.headers.into_iter().collect(),
            code: code.to_string(),
            fingerprint,
        })
    }

    fn default_cwd(&self) -> PathBuf {
        self.config
            .read()
            .server
            .default_cwd
            .as_deref()
            .map(crate::util::expand_home)
            .filter(|p| p.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn default_model(&self) -> Option<ModelRef> {
        self.resolve_model("@default")
            .ok()
            .or_else(|| self.config.read().default_model())
            .or_else(|| self.providers.first_model())
    }

    /// The background memory review's model: `@memory`, else `@utility`;
    /// `None` means the chat's own.
    fn memory_model(&self) -> Option<ModelRef> {
        ["@memory", "@utility"]
            .into_iter()
            .find_map(|r| self.resolve_model(r).ok())
    }

    /// Tessera's own roles first, then the user's (`[models]`).
    fn model_roles(&self) -> ModelRoles {
        const BUILTIN: [(&str, &str, &str); 3] = [
            (
                "default",
                "Default",
                "New chats, and anything without a model of its own.",
            ),
            (
                "utility",
                "Utility",
                "Titles, summaries and page extracts (also @fast). Unset: each chat's own model.",
            ),
            (
                "memory",
                "Memory & learning",
                "The background review that saves memories and skills. Unset: @utility, then the chat's own model.",
            ),
        ];
        let cfg = self.config.read().clone();
        let mut roles: Vec<ModelRole> = BUILTIN
            .iter()
            .map(|(name, label, description)| ModelRole {
                name: name.to_string(),
                label: label.to_string(),
                description: description.to_string(),
                value: cfg.role_setting(name),
                resolved: match *name {
                    "default" => self.default_model(),
                    "utility" => self.resolve_model("@utility").ok(),
                    _ => self.memory_model(),
                },
                builtin: true,
            })
            .collect();
        for name in cfg.models.roles.keys() {
            if matches!(name.as_str(), "default" | "utility" | "fast" | "memory") {
                continue;
            }
            roles.push(ModelRole {
                name: name.clone(),
                label: name.clone(),
                description: format!("Your own role: use @{name} wherever a model is chosen."),
                value: cfg.role_setting(name),
                resolved: self.resolve_model(&format!("@{name}")).ok(),
                builtin: false,
            });
        }
        ModelRoles { roles }
    }

    /// `@role` → the model it names; any other reference as it is.
    fn resolve_ref(&self, m: &ModelRef) -> Result<ModelRef, String> {
        match m.as_role() {
            Some(role) => self.resolve_model(&format!("@{role}")),
            None => Ok(m.clone()),
        }
    }

    fn list_sessions(&self, archived: bool) -> Result<Vec<SessionInfo>> {
        let mut list = self.store.list_sessions(archived)?;
        // Live state wins over the stored copy. Do not hold the map while
        // waiting for a state: helpers look up their parent while holding it.
        let live = self.sessions.lock().clone();
        for s in list.iter_mut() {
            if let Some(rt) = live.get(&s.id) {
                #[cfg(test)]
                self.lock_order_hooks.before_list_state(&s.id);
                *s = rt.st.lock().info.clone();
            }
        }
        list.retain(|s| s.kind != SessionKind::Subagent);
        Ok(list)
    }

    /// Loaded sessions only; the map guard never follows an Arc to its state.
    fn live_session(&self, id: &str) -> Option<Arc<Rt>> {
        let rt = self.sessions.lock().get(id).cloned()?;
        rt.used
            .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        Some(rt)
    }

    fn live_sessions(&self) -> Vec<Arc<Rt>> {
        self.sessions.lock().values().cloned().collect()
    }

    fn rt(&self, id: &str) -> Result<Arc<Rt>, ApiError> {
        if let Some(rt) = self.live_session(id) {
            return Ok(rt);
        }
        let gate = self.load_gate(id);
        let _loading = gate.lock();
        // A concurrent loader may have published (and changed) this chat
        // while we waited. Never publish a pre-eviction stale state.
        if let Some(rt) = self.live_session(id) {
            return Ok(rt);
        }
        let (mut info, internal) = self
            .store
            .get_session(id)
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no session `{id}`")))?;
        let depth = if info.kind == SessionKind::Subagent {
            1
        } else {
            0
        };
        // Legacy bridged workers and CLI mirrors both had parent_call.
        // Recover ownership from the real starting call, not its model.
        if info.kind == SessionKind::Subagent
            && info.helper_owner.is_none()
            && let (Some(parent), Some(call)) = (&info.parent_id, &info.parent_call)
        {
            let native = self.store.entries(parent, 0).map_err(crate::engine::internal)?.iter().any(|e| {
                matches!(&e.body, EntryBody::Assistant { content, .. } if content.iter().any(|p| {
                    matches!(p, AssistantPart::ToolCall { id, name, arguments } if id == call
                        && matches!(name.as_str(), "agents" | "agent" | "subagent")
                        && (name != "agents" || arguments.get("action").and_then(Value::as_str) == Some("start")))
                }))
            });
            info.helper_owner = Some(if native {
                HelperOwner::Tessera
            } else {
                HelperOwner::ClaudeCode
            });
        }
        let cli_mirror = info.helper_owner == Some(HelperOwner::ClaudeCode);
        let approvals_on = info
            .parent_id
            .clone()
            .filter(|_| depth > 0)
            .unwrap_or_else(|| info.id.clone());
        // Loading follows ancestors before publishing any child state. Helpers
        // cannot nest, so a malformed self/helper parent fails closed.
        let parent_ceiling = info
            .parent_id
            .as_deref()
            .filter(|parent| info.kind == SessionKind::Subagent && *parent != id)
            .and_then(|parent| {
                let parent_info = self.store.get_session(parent).ok().flatten()?.0;
                if parent_info.kind == SessionKind::Subagent {
                    return None;
                }
                let rt = self.rt(parent).ok()?;
                let st = rt.st.lock();
                Some((parent.to_string(), st.permission_ceiling.clone()))
            });
        let permission_ceiling = permissions::PermissionCeiling::new(info.permission);
        let budget = internal.helper_budget.clone().map(Budget::restored);
        let rt = Arc::new(Rt {
            used: eviction::used_now(),
            st: Mutex::new(RtState {
                permission_ceiling,
                parent_ceiling,
                prompt_helpers: None,
                info,
                internal,
                running: false,
                cancel: None,
                turn_stops: StopMarks::default(),
                next_turn_stops: None,
                streaming: None,
                steer: Vec::new(),
                followups: Vec::new(),
                send_now: Vec::new(),
                delivering: Vec::new(),
                budget,
                depth,
                approvals_on,
                tools_only: cli_mirror.then(Vec::new),
                no_tool_calls: cli_mirror,
                speaker: None,
                bot_depth: 0,
                bot_receipt: None,
                agent_list: Default::default(),
                pre: None,
                repeated: false,
                claude_limit_notified: false,
                claude_limit_window: None,
                work_span: None,
                rewinding: false,
            }),
        });
        self.publish_permission(&rt.st.lock());
        self.restore_queue(&mut rt.st.lock())
            .map_err(crate::engine::internal)?;
        Ok(self
            .sessions
            .lock()
            .entry(id.to_string())
            .or_insert(rt)
            .clone())
    }

    /// Delete a session for good: stop its work, and drop its boards, files,
    /// kept passwords and tiles with it.
    fn remove_session(&self, session_id: String) -> Result<(), ApiError> {
        self.stop_background(&session_id, None);
        for child in self.store.children_of(&session_id).map_err(internal)? {
            if child.kind == SessionKind::Subagent && child.parent_call.is_some() {
                self.remove_session(child.id)?;
            }
        }
        self.stop_bot_handoffs(&session_id);
        self.delete_session_boards(&session_id);
        self.browser.session_removed(&session_id);
        // File contents it was shown and diagnostics waiting for it.
        self.files.forget_session(&session_id);
        if let Some(lsp) = self.lsp.read().as_ref() {
            lsp.forget_session(&session_id);
        }
        // Its uploaded and shared files go too (ids are plain alphanumerics).
        self.forget_session_files(&session_id);
        if !session_id.is_empty() && session_id.chars().all(|c| c.is_ascii_alphanumeric()) {
            let _ = std::fs::remove_dir_all(self.paths.uploads_dir().join(&session_id));
        }
        if let Ok(rt) = self.rt(&session_id) {
            let mut st = rt.st.lock();
            // A retained child ceiling must not retain a deleted parent's authority.
            st.permission_ceiling.revoke();
            // No message sent "now" starts another turn of it.
            st.send_now.clear();
            if let Some(c) = &st.cancel {
                c.cancel();
            }
        }
        self.sessions.lock().remove(&session_id);
        self.store.delete_session(&session_id).map_err(internal)?;
        // Passwords and keys kept for this chat alone go with it.
        self.auth.drop_chat_secrets(&session_id);
        let _ = self.events.send(Event::SessionRemoved { session_id });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn create_session(
        &self,
        cwd: Option<String>,
        model: Option<ModelRef>,
        title: Option<String>,
        kind: SessionKind,
        parent: Option<&RtState>,
        permission: Option<PermissionMode>,
        bot: Option<&Bot>,
    ) -> Result<Arc<Rt>> {
        self.create_session_ext(
            cwd, model, title, kind, parent, permission, bot, false, false, false, None,
        )
    }

    /// A new session. A private one is `owner`'s (the device that made it)
    /// alone; a session started under a private chat is private too, and
    /// that chat's device's.
    #[allow(clippy::too_many_arguments)]
    fn create_session_ext(
        &self,
        cwd: Option<String>,
        model: Option<ModelRef>,
        title: Option<String>,
        kind: SessionKind,
        parent: Option<&RtState>,
        permission: Option<PermissionMode>,
        bot: Option<&Bot>,
        private: bool,
        internal: bool,
        lean: bool,
        owner: Option<&str>,
    ) -> Result<Arc<Rt>> {
        let (private, owner) = match parent.filter(|p| p.info.private) {
            Some(p) => (true, self.store.private_owner(&p.info.id).flatten()),
            None => (private, owner.map(str::to_string)),
        };
        // A private chat has no working directory of its own (no file tools).
        let cwd = if private {
            dirs::home_dir().map(|h| h.display().to_string())
        } else {
            cwd
        };
        let cwd = cwd.or_else(|| {
            bot.and_then(|b| b.cwd.clone())
                .filter(|c| crate::util::expand_home(c).is_dir())
        });
        let cwd = match cwd.filter(|c| !c.trim().is_empty()) {
            Some(c) => {
                let p = crate::util::expand_home(&c);
                if !p.is_dir() {
                    return Err(anyhow!("{} is not a directory", p.display()));
                }
                p
            }
            None => parent
                .map(|p| PathBuf::from(&p.info.cwd))
                .unwrap_or_else(|| self.default_cwd()),
        };
        let cwd = crate::platform::canonicalize(&cwd).unwrap_or(cwd);
        // `@role`s resolve now: a session keeps one concrete model (prompt
        // caches), and a role changed in the config applies to new sessions.
        let model = model
            .map(|m| self.resolve_ref(&m).map_err(|e| anyhow!(e)))
            .transpose()?
            .or_else(|| {
                bot.and_then(|b| b.model.clone())
                    .and_then(|m| self.resolve_ref(&m).ok())
                    .filter(|m| self.providers.exists(&m.provider))
            })
            .or_else(|| parent.map(|p| p.info.model.clone()))
            .or_else(|| self.default_model())
            .ok_or_else(|| anyhow!("no model configured: add a provider or sign in first"))?;
        let minfo = self.providers.model_info(&model);
        let window = self.working_window(
            &model,
            minfo.as_ref().map(|m| m.context_window).unwrap_or(128_000),
        );
        // Start with the bot's or parent's choice, else (in the user's own
        // chats) the level last picked for this model, else the model's own
        // default, snapped to a level this model really supports.
        let remembered = (kind == SessionKind::Chat && parent.is_none() && !internal)
            .then(|| minfo.as_ref().and_then(|i| i.last_thinking))
            .flatten();
        let want = bot
            .and_then(|b| b.thinking)
            .or(parent.map(|p| p.info.thinking))
            .or(remembered)
            .or(minfo.as_ref().map(|i| i.default_thinking))
            .unwrap_or(ThinkingLevel::Auto);
        let thinking = minfo
            .map(|i| crate::thinking::clamp(want, &i.thinking, i.default_thinking))
            .unwrap_or(want);
        let now = now_ms();
        let cfg_perm = self.config.read().new_chat_permission();
        let info = SessionInfo {
            id: new_id(),
            revision: 0,
            title: title.clone().unwrap_or_else(|| {
                if private {
                    "Private chat".into()
                } else {
                    "New chat".into()
                }
            }),
            kind,
            cwd: cwd.display().to_string(),
            model,
            thinking,
            permission: permission
                .or(bot.map(|b| b.permission))
                .or(parent.map(|p| p.info.permission))
                .unwrap_or(cfg_perm),
            status: SessionStatus::Idle,
            parent_id: parent.map(|p| p.info.id.clone()),
            bot_id: bot.map(|b| b.id.clone()),
            members: Vec::new(),
            created_at: now,
            updated_at: now,
            last_seq: 0,
            usage: Usage::default(),
            context: ContextStats {
                tokens: 0,
                window,
                compactions: 0,
                source: Some("estimated".into()),
            },
            pinned: false,
            archived: false,
            preview: String::new(),
            task: None,
            private,
            steps: 0,
            internal,
            // Helpers never pay for priority processing.
            fast: !internal && parent.is_some_and(|p| p.info.fast),
            daybreak: !internal && parent.is_some_and(|p| p.info.daybreak),
            subagents: true,
            helper_limits: None,
            helper_queued: false,
            parent_call: None,
            helper_owner: None,
            role: None,
            // A terminal chat: never for bots, private chats or helpers.
            lean: lean && bot.is_none() && !private && !internal && kind == SessionKind::Chat,
            working_for: None,
        };
        // Decided once, so the tool set (and the prompt cache) stays fixed.
        let lsp = !private
            && self
                .lsp
                .read()
                .as_ref()
                .is_some_and(|l| !l.detect(&cwd).is_empty());
        let internal = SessionInternal {
            titled: title.is_some(),
            lsp,
            ..Default::default()
        };
        self.store.put_session(&info, &internal)?;
        // Before anyone hears of it: only its device may.
        if private {
            self.store.set_private_owner(&info.id, owner.as_deref());
        }
        let depth = parent.map(|p| p.depth + 1).unwrap_or(0);
        let approvals_on = parent
            .map(|p| p.approvals_on.clone())
            .unwrap_or_else(|| info.id.clone());
        let rt = Arc::new(Rt {
            used: eviction::used_now(),
            st: Mutex::new(RtState {
                permission_ceiling: permissions::PermissionCeiling::new(info.permission),
                parent_ceiling: parent
                    .filter(|_| kind == SessionKind::Subagent)
                    .map(|p| (p.info.id.clone(), p.permission_ceiling.clone())),
                prompt_helpers: None,
                info: info.clone(),
                internal,
                running: false,
                cancel: None,
                turn_stops: StopMarks::default(),
                next_turn_stops: None,
                streaming: None,
                steer: Vec::new(),
                followups: Vec::new(),
                send_now: Vec::new(),
                delivering: Vec::new(),
                budget: None,
                depth,
                approvals_on,
                tools_only: None,
                no_tool_calls: false,
                speaker: None,
                bot_depth: 0,
                bot_receipt: None,
                agent_list: Default::default(),
                pre: None,
                repeated: false,
                claude_limit_notified: false,
                claude_limit_window: None,
                work_span: None,
                rewinding: false,
            }),
        });
        #[cfg(test)]
        self.lock_order_hooks.before_session_insert();
        self.sessions.lock().insert(info.id.clone(), rt.clone());
        let _ = self.events.send(Event::SessionUpdated { session: info });
        Ok(rt)
    }

    fn snapshot(&self, id: &str, since: u64) -> Result<SessionSnapshot, ApiError> {
        self.snapshot_with_page(id, since, None)
    }

    fn snapshot_with_page(
        &self,
        id: &str,
        since: u64,
        page: Option<(Option<u64>, u32)>,
    ) -> Result<SessionSnapshot, ApiError> {
        let rt = self.rt(id)?;
        let st = rt.st.lock();
        let (entries, history_before, history_revision) = if let Some((before, limit)) = page {
            let page = self
                .store
                .entries_page(id, before, limit)
                .map_err(internal)?;
            (page.entries, page.before, page.revision)
        } else {
            (
                self.store.entries(id, since).map_err(internal)?,
                None,
                self.store.history_revision(id).map_err(internal)?,
            )
        };
        let entries = entries
            .into_iter()
            .filter(|e| !crate::tools::meta::is_protocol_entry(e))
            .collect();
        let approvals = self
            .approvals
            .lock()
            .values()
            .filter(|p| p.shown_on == id)
            .map(|p| p.request.clone())
            .collect();
        let snapshot = SessionSnapshot {
            history_revision,
            history_before,
            info: st.info.clone(),
            entries,
            streaming: st.streaming.clone().filter(|s| {
                st.internal.completion_pending.is_none()
                    && !crate::tools::meta::check_ack_parts(&s.content)
            }),
            approvals,
            queued: queued_messages(&st),
            children: Vec::new(),
        };
        drop(st);
        snapshot::bounded(SessionSnapshot {
            children: self.subagents_of(id),
            ..snapshot
        })
    }

    /// The subagents `parent` started (not the harness's own helpers), live
    /// state over the stored copy.
    fn subagents_of(&self, parent: &str) -> Vec<SessionInfo> {
        let mut out: Vec<SessionInfo> = self
            .store
            .children_of(parent)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.kind == SessionKind::Subagent && !i.internal)
            .collect();
        let live = self.sessions.lock().clone();
        for i in out.iter_mut() {
            if let Some(rt) = live.get(&i.id) {
                *i = rt.st.lock().info.clone();
            }
        }
        out.sort_by_key(|i| i.created_at);
        out
    }

    fn fork(&self, id: &str, at: Option<u64>) -> Result<SessionInfo, ApiError> {
        let title = format!("{} (fork)", self.rt(id)?.st.lock().info.title);
        self.fork_as(id, at, title)
    }

    /// Copy a session's history (up to and including `at`) and its file
    /// snapshots into a new chat; its `parent_id` is the session it came
    /// from, so forks form a tree.
    fn fork_as(&self, id: &str, at: Option<u64>, title: String) -> Result<SessionInfo, ApiError> {
        let rt = self.rt(id)?;
        let (info, int) = {
            let st = rt.st.lock();
            (st.info.clone(), st.internal.clone())
        };
        let entries = self.store.entries(id, 0).map_err(internal)?;
        let now = now_ms();
        let mut new_info = SessionInfo {
            id: new_id(),
            title,
            status: SessionStatus::Idle,
            created_at: now,
            updated_at: now,
            pinned: false,
            archived: false,
            parent_id: Some(id.to_string()),
            kind: SessionKind::Chat,
            internal: false,
            helper_queued: false,
            parent_call: None,
            helper_owner: None,
            role: None,
            working_for: None,
            ..info
        };
        let new_int = SessionInternal {
            titled: true,
            allowed: Vec::new(),
            subagent: None,
            ..int
        };
        self.store
            .put_session(&new_info, &new_int)
            .map_err(internal)?;
        for e in entries
            .into_iter()
            .filter(|e| at.is_none_or(|a| e.seq <= a))
        {
            let entry = self.store.append(&new_info.id, e.body).map_err(internal)?;
            new_info.last_seq = entry.seq;
        }
        self.store
            .copy_snapshots(id, &new_info.id, at)
            .map_err(internal)?;
        self.store
            .put_session(&new_info, &new_int)
            .map_err(internal)?;
        let _ = self.events.send(Event::SessionUpdated {
            session: new_info.clone(),
        });
        Ok(new_info)
    }

    fn capture_snapshot(st: &mut RtState) -> (SessionInfo, SessionInternal, u64) {
        if let Some(b) = &st.budget {
            st.internal.helper_budget = Some(b.persisted());
        }
        // Always capture public and internal state together. An H0→H1→H0
        // change cannot omit H0 merely because it matches a previous write.
        st.internal.revision = st
            .internal
            .revision
            .checked_add(1)
            .expect("session revision overflow");
        st.info.revision = st.internal.revision;
        (st.info.clone(), st.internal.clone(), st.internal.revision)
    }

    fn persist_snapshot(
        &self,
        info: &SessionInfo,
        internal: &SessionInternal,
        revision: u64,
    ) -> Result<crate::store::SaveOutcome> {
        if self.live_session(&info.id).is_none() {
            return Ok(crate::store::SaveOutcome::Missing);
        }
        let outcome = self.store.update_session(info, internal, revision)?;
        if outcome == crate::store::SaveOutcome::Saved {
            self.publish_snapshot(info);
        }
        Ok(outcome)
    }

    /// Hold the state lock through publication: an older completed write
    /// must not publish after the live snapshot has advanced.
    fn publish_snapshot(&self, info: &SessionInfo) {
        let Some(rt) = self.live_session(&info.id) else {
            return;
        };
        let st = rt.st.lock();
        if st.internal.revision == info.revision {
            let _ = self.events.send(Event::SessionUpdated {
                session: info.clone(),
            });
        }
    }
    fn save(&self, rt: &Rt) {
        let _activity = crate::activity::enter(&crate::activity::SAVE);
        let (info, internal, revision) = Self::capture_snapshot(&mut rt.st.lock());
        if let Err(e) = self.persist_snapshot(&info, &internal, revision) {
            tracing::error!("saving session {}: {e}", info.id);
        }
    }

    fn set_status(&self, rt: &Rt, status: SessionStatus) {
        {
            let mut st = rt.st.lock();
            if st.info.status == status {
                return;
            }
            st.info.status = status;
        }
        self.save(rt);
    }

    fn append(&self, rt: &Rt, mut body: EntryBody) -> Result<Entry> {
        self.redact_entry(&mut body);
        let ack = matches!(&body, EntryBody::Assistant { content, .. }
            if crate::tools::meta::check_ack_parts(content));
        if ack
            && let EntryBody::Assistant {
                content, author, ..
            } = &mut body
        {
            if content
                .iter()
                .any(|p| matches!(p, AssistantPart::ToolCall { .. }))
            {
                content.retain(|p| {
                    !matches!(
                        p,
                        AssistantPart::Text { .. } | AssistantPart::Thinking { .. }
                    )
                });
            } else {
                *author = Some(crate::tools::meta::CHECK_AUTHOR.into());
            }
        }
        let id = rt.st.lock().info.id.clone();
        let entry = self.store.append(&id, body)?;
        if ack {
            let mut st = rt.st.lock();
            if st.internal.completion_pending.take().is_some() {
                st.internal.completion_acknowledged = st
                    .info
                    .task
                    .as_ref()
                    .map(crate::tools::meta::completion_state);
            }
        } else if matches!(&entry.body, EntryBody::Assistant { content, .. }
            if !content.iter().any(|p| matches!(p, AssistantPart::ToolCall { .. })))
        {
            rt.st.lock().internal.completion_pending = None;
        }
        self.record_entry(rt, id, entry)
    }

    fn record_entry(&self, rt: &Rt, id: String, entry: Entry) -> Result<Entry> {
        {
            let mut st = rt.st.lock();
            // Kept in the store under the chat's lock by `append_or_steer`
            // and recorded here after: a later entry may be in already.
            st.info.last_seq = st.info.last_seq.max(entry.seq);
            st.info.updated_at = st.info.updated_at.max(entry.ts);
            if let EntryBody::User {
                author: Some(author),
                ..
            } = &entry.body
                && author.starts_with("subagent:")
            {
                st.internal.completion_acknowledged = None;
                st.internal.completion_pending = None;
            }
            let preview = match &entry.body {
                // What the harness tells the agent isn't the conversation's news.
                EntryBody::User { content, author } if author.as_deref() != Some("harness") => {
                    user_preview(content)
                }
                EntryBody::Assistant { content, .. }
                    if !crate::tools::meta::is_protocol_entry(&entry) =>
                {
                    content.iter().rev().find_map(|p| match p {
                        AssistantPart::Text { text }
                            if !text.trim().is_empty() && !is_check_ack(text) =>
                        {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                }
                _ => None,
            };
            if let Some(p) = preview {
                st.info.preview = one_line(&p, 140);
            }
        }
        self.secret_entry_needed(&id, &entry);
        // Omit protocol at the source so even clients predating the marker
        // cannot draw it. Sequence numbers still advance in session info.
        if !crate::tools::meta::is_protocol_entry(&entry) {
            let _ = self.events.send(Event::EntryAdded {
                session_id: id,
                entry: entry.clone(),
            });
        }
        self.save(rt);
        Ok(entry)
    }

    fn notice(&self, rt: &Rt, level: NoticeLevel, text: impl Into<String>) {
        let _ = self.append(
            rt,
            EntryBody::Notice {
                level,
                text: text.into(),
                learned: Vec::new(),
            },
        );
    }

    /// The file where a planned stop (an update, `systemctl restart`) lists
    /// the sessions it cut off, so the next start continues them.
    fn planned_restart_file(&self) -> std::path::PathBuf {
        self.paths.data_dir.join("resume-after-restart.json")
    }

    /// Stopping on purpose (not a crash): remember what was working, so it
    /// picks up again when the server is back.
    fn note_planned_restart(&self) {
        let running: Vec<String> = self
            .live_sessions()
            .into_iter()
            .filter_map(|rt| {
                let st = rt.st.lock();
                st.running.then(|| st.info.id.clone())
            })
            .collect();
        if running.is_empty() {
            return;
        }
        if let Ok(json) = serde_json::to_string(&running) {
            let _ = std::fs::write(self.planned_restart_file(), json);
        }
    }

    /// Mark sessions that were running when the harness stopped. Returns
    /// those that continue by themselves (`agent.resume_on_restart`, only
    /// when `resume` is allowed): scheduled jobs and chats running in `auto`
    /// permission by default. Interactive chats wait for the user.
    fn mark_interrupted(&self, resume: bool) -> Result<Vec<String>> {
        use crate::config::ResumeOnRestart;
        let mode = self.config.read().agent.resume_on_restart;
        // Sessions a planned stop cut off continue whatever the setting
        // (that is for crashes and reboots); read once, then forgotten.
        let planned: HashSet<String> = if resume {
            let file = self.planned_restart_file();
            let ids = std::fs::read_to_string(&file)
                .ok()
                .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                .unwrap_or_default();
            let _ = std::fs::remove_file(&file);
            ids.into_iter().collect()
        } else {
            HashSet::new()
        };
        let mut out = Vec::new();
        let mut interrupted_parents = HashSet::new();
        for id in self.store.all_session_ids()? {
            let Some((mut info, mut internal)) = self.store.get_session(&id)? else {
                continue;
            };
            // Pending attempts, including setup interrupted before Running,
            // get one immutable restart terminal and boundary.
            let mut closed = false;
            for a in &mut internal.attempts {
                closed |= a.finish(
                    crate::store::AttemptOutcome::Interrupted,
                    info.last_seq,
                    None,
                );
            }
            if closed {
                self.store.put_session(&info, &internal)?;
            }
            // A chat whose first turn never finished (stopped by a restart)
            // was never named: it takes its first message's words.
            if !internal.titled
                && info.kind == SessionKind::Chat
                && let Some(first) = self.store.entries(&id, 0).ok().and_then(|es| {
                    es.iter().find_map(|e| match &e.body {
                        EntryBody::User { content, .. } => user_preview(content),
                        _ => None,
                    })
                })
            {
                info.title = one_line(&first, if info.private { 48 } else { 60 });
                internal.titled = true;
                if !matches!(
                    info.status,
                    SessionStatus::Running
                        | SessionStatus::AwaitingApproval
                        | SessionStatus::Compacting
                ) {
                    self.store.put_session(&info, &internal)?;
                }
            }
            if internal.claude_dependencies {
                // CLI background commands have no child session and leave an
                // Idle parent. Their runtime roster cannot survive a restart.
                internal.claude_dependencies = false;
                internal.completion_acknowledged = None;
                internal.completion_pending = None;
                self.store.put_session(&info, &internal)?;
            }
            if matches!(
                info.status,
                SessionStatus::Running
                    | SessionStatus::AwaitingApproval
                    | SessionStatus::Compacting
            ) {
                info.status = SessionStatus::Interrupted;
                // No longer in any line: a helper that goes on queues again.
                info.helper_queued = false;
                if let Some(parent) = &info.parent_id {
                    interrupted_parents.insert(parent.clone());
                }
                internal.completion_acknowledged = None;
                internal.completion_pending = None;
                self.store.put_session(&info, &internal)?;
                if let Some(spec) = internal.subagent.as_mut() {
                    spec.interrupted_by_restart = true;
                    self.store.put_session(&info, &internal)?;
                }
                self.cut_off.lock().insert(id.clone());
                let unattended = info.kind == SessionKind::Job
                    || (info.kind == SessionKind::Chat && info.permission == PermissionMode::Auto);
                // A subagent at work when an update stopped the server goes on
                // too, and reports to its chat as it would have (not Claude
                // Code's: Claude Code resumes those itself; not a memory review).
                let helper = info.kind == SessionKind::Subagent
                    && (info.parent_id.is_none()
                        || (info.parent_call.is_some() && internal.subagent.is_none())
                        || info.internal);
                let auto = resume
                    && !info.private
                    && !is_room(&info)
                    && !helper
                    && (info.kind != SessionKind::Subagent || planned.contains(&id))
                    && (planned.contains(&id)
                        || match mode {
                            ResumeOnRestart::Off => false,
                            ResumeOnRestart::All => true,
                            ResumeOnRestart::Unattended => unattended,
                        });
                let text = if auto && planned.contains(&id) {
                    "The server restarted (an update) while this session was running, so it continues by itself; unfinished tool calls are reported to the agent."
                } else if auto {
                    "The harness stopped while this session was running. It runs unattended, so it continues by itself; unfinished tool calls are reported to the agent."
                } else {
                    "The harness stopped while this session was running. Send a message to continue; unfinished tool calls will be reported to the agent."
                };
                let _ = self.store.append(
                    &id,
                    EntryBody::Notice {
                        level: NoticeLevel::Warning,
                        text: text.into(),
                        learned: Vec::new(),
                    },
                );
                if auto {
                    out.push(id);
                } else if info.kind == SessionKind::Job {
                    self.job_run_finished(
                        &id,
                        "interrupted",
                        "The server stopped while it ran; open its chat to continue.",
                    );
                }
                if !auto && info.working_for.is_some() {
                    self.handoff_cut_off(&mut info, &internal);
                    self.store.put_session(&info, &internal)?;
                }
            }
        }
        // Even an idle parent loses the dependency its old mark relied on.
        for id in interrupted_parents {
            if let Some((info, mut internal)) = self.store.get_session(&id)? {
                internal.completion_acknowledged = None;
                internal.completion_pending = None;
                self.store.put_session(&info, &internal)?;
            }
        }
        Ok(out)
    }

    /// Load the sessions to resume and mark them running.
    fn claim_for_resume(&self, ids: Vec<String>) -> Vec<Arc<Rt>> {
        ids.iter()
            .filter_map(|id| self.rt(id).ok())
            .filter(|rt| {
                let mut st = rt.st.lock();
                !std::mem::replace(&mut st.running, true)
            })
            .collect()
    }

    /// Continue interrupted unattended sessions: report what happened to
    /// the tool calls in flight (see `recovery_trace`), then run a turn.
    fn resume_interrupted(self: &Arc<Self>, sessions: Vec<Arc<Rt>>) {
        for rt in sessions {
            let (id, kind) = {
                let st = rt.st.lock();
                (st.info.id.clone(), st.info.kind)
            };
            // Still the turn it was: the Stops since it began count.
            {
                let mut st = rt.st.lock();
                st.next_turn_stops = Some(st.internal.turn_marks.clone());
            }
            let trace = self.recovery_trace(&rt);
            self.recover_interrupted(&rt);
            if let Err(e) = self.append(
                &rt,
                EntryBody::User {
                    content: vec![ContentPart::text(trace)],
                    author: Some("harness".into()),
                },
            ) {
                tracing::error!("resuming {id}: {e}");
                rt.st.lock().running = false;
                continue;
            }
            tracing::info!("resuming interrupted session {id}");
            if kind == SessionKind::Subagent {
                self.continue_background(rt);
                continue;
            }
            // A job's run records how it ended as its turn ends (`inbox.rs`).
            let me = self.clone();
            tokio::spawn(async move {
                me.run(rt, CancellationToken::new()).await;
            });
        }
    }

    /// What became of the tool calls of the agent's last step when the
    /// harness stopped, so a resumed agent verifies instead of repeating
    /// something that may already have happened.
    fn recovery_trace(&self, rt: &Rt) -> String {
        let id = rt.st.lock().info.id.clone();
        let entries = self.store.entries(&id, 0).unwrap_or_default();
        let (mut done, mut lost, mut uncertain, mut not_run) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        if let Some(i) = entries
            .iter()
            .rposition(|e| matches!(e.body, EntryBody::Assistant { .. }))
            && let EntryBody::Assistant { content, .. } = &entries[i].body
        {
            let answered: std::collections::HashSet<&str> = entries[i..]
                .iter()
                .filter_map(|e| match &e.body {
                    EntryBody::ToolResult { call_id, .. } => Some(call_id.as_str()),
                    _ => None,
                })
                .collect();
            for p in content {
                let AssistantPart::ToolCall {
                    id: call_id,
                    name,
                    arguments,
                } = p
                else {
                    continue;
                };
                let summary = self
                    .registry
                    .read()
                    .get(name)
                    .map(|t| t.summary(arguments))
                    .unwrap_or_default();
                let what = if summary.trim().is_empty() {
                    format!("`{name}`")
                } else {
                    format!("`{name}` {}", one_line(&summary, 120))
                };
                if answered.contains(call_id.as_str()) {
                    done.push(what);
                } else {
                    match self.store.tool_run_state(&id, call_id) {
                        Ok(Some(true)) => lost.push(what),
                        Ok(Some(false)) => uncertain.push(what),
                        _ => not_run.push(what),
                    }
                }
            }
        }
        let mut s = String::from(
            "[Harness restarted] The Tessera server restarted while you were working. This session runs unattended, so it continues on its own.",
        );
        if done.is_empty() && lost.is_empty() && uncertain.is_empty() && not_run.is_empty() {
            s.push_str(" No tool call was in flight.");
        } else {
            s.push_str(" Your last tool calls:");
            for (label, list) in [
                ("completed", &done),
                (
                    "finished, but its result was lost (it probably took effect)",
                    &lost,
                ),
                (
                    "UNCERTAIN, it was running when the server stopped and may or may not have taken effect",
                    &uncertain,
                ),
                ("not run", &not_run),
            ] {
                for what in list {
                    s.push_str(&format!("\n- {label}: {what}"));
                }
            }
        }
        s.push_str(
            "\nCheck the real state (files, git, services) before redoing anything uncertain, and never repeat an external action (push, deploy, publish, send, pay, delete) unless you have confirmed it did not happen. Then continue the task; if it cannot continue safely, stop and report why.",
        );
        // Claude Code's subagents end with its process: they continue only if
        // it resumes them (their work so far is kept).
        if self.claude_subagents_stopped(&id) {
            s.push_str(
                "\nSubagents you started were stopped by the restart: unless their work was finished, resume each one now (SendMessage to its id) so it carries on from where it was, instead of starting over.",
            );
        }
        s
    }

    /// Claude Code subagents of `session` that this restart cut off (not
    /// ones stopped long before).
    fn claude_subagents_stopped(&self, session: &str) -> bool {
        let cut_off = self.cut_off.lock();
        self.store
            .children_of(session)
            .unwrap_or_default()
            .iter()
            .any(|info| info.parent_call.is_some() && cut_off.contains(&info.id))
    }

    // ---- sending & the agent loop -----------------------------------------------

    /// `notes` (about the message, e.g. a key taken out of it) follow it.
    fn send(
        &self,
        id: &str,
        content: Vec<ContentPart>,
        delivery: Delivery,
        notes: Vec<(NoticeLevel, String)>,
    ) -> Result<(), ApiError> {
        let rt = self.rt(id)?;
        // What this message's turn belongs to: the Stop count now.
        let stops = self.stops_here();
        let start = {
            let mut st = rt.st.lock();
            let start = self.update_queue(&mut st, |st| {
                let w = Waiting::user(content, stops);
                let start = if delivery == Delivery::Interrupt {
                    interrupt(st, w)
                } else if st.running {
                    // While it works, messages wait in the queue (where the user
                    // can edit them or turn them into steering); only an
                    // explicit steer is read at the next step.
                    match delivery {
                        Delivery::Steer => st.steer.push(w),
                        _ => st.followups.push(w),
                    }
                    None
                } else {
                    st.running = true;
                    // The user is here: what follows is new work.
                    st.work_span = None;
                    Some(w)
                };
                if let Some(w) = &start {
                    st.delivering.push((Queue::Now, w.clone()));
                }
                if let Some(w) = &start {
                    st.next_turn_stops = Some(w.stops.clone());
                }
                Ok(start)
            })?;
            if start.is_none() {
                self.queue_changed(&st);
            }
            start
        };
        match start {
            Some(w) => self.start_turn(rt, vec![w], notes),
            None => {
                self.post_notes(id, notes);
                Ok(())
            }
        }
    }

    /// Start a turn with the user's `first` messages (the session is marked
    /// running already); `notes` follow them.
    fn start_turn(
        &self,
        rt: Arc<Rt>,
        first: Vec<Waiting>,
        notes: Vec<(NoticeLevel, String)>,
    ) -> Result<(), ApiError> {
        let me = self
            .me
            .upgrade()
            .ok_or_else(|| internal("engine shutting down"))?;
        tokio::spawn(async move {
            me.open_turn(&rt, first);
            for (level, text) in notes {
                me.notice(&rt, level, text);
            }
            let cancel = CancellationToken::new();
            me.clone().run(rt, cancel).await;
        });
        Ok(())
    }

    /// What a turn starts with: calls an earlier turn left unanswered, then
    /// the user's messages.
    fn open_turn(&self, rt: &Rt, first: Vec<Waiting>) {
        self.recover_interrupted(rt);
        for w in first {
            if let Err(e) = self.append_waiting(rt, w) {
                tracing::error!("append user message: {e}");
            }
        }
    }

    /// Tell clients what waits in the queue now. Called with the state
    /// locked, so changes racing each other reach them in order.
    fn queue_changed(&self, st: &RtState) {
        if let Err(e) = self.persist_queue(st) {
            tracing::error!("persisting queued messages: {e}");
            return;
        }
        let _ = self.events.send(Event::QueueChanged {
            session_id: st.info.id.clone(),
            queued: queued_messages(st),
        });
    }

    /// Report tool calls that never got a result (crash or restart) so the
    /// model does not blindly repeat side effects.
    fn recover_interrupted(&self, rt: &Rt) {
        let id = rt.st.lock().info.id.clone();
        // Only the tail from the last reply on matters.
        let from = self.store.last_assistant_seq(&id).unwrap_or(0);
        let Ok(entries) = self.store.entries(&id, from.saturating_sub(1)) else {
            return;
        };
        let Some(last_asst) = entries
            .iter()
            .rposition(|e| matches!(e.body, EntryBody::Assistant { .. }))
        else {
            return;
        };
        let answered: Vec<&str> = entries[last_asst..]
            .iter()
            .filter_map(|e| match &e.body {
                EntryBody::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        let EntryBody::Assistant { content, .. } = &entries[last_asst].body else {
            return;
        };
        for p in content {
            if let AssistantPart::ToolCall {
                id: call_id, name, ..
            } = p
            {
                if answered.contains(&call_id.as_str()) {
                    continue;
                }
                let started = self.store.tool_was_started(&id, call_id).unwrap_or(false);
                let text = if started {
                    "Interrupted: the harness stopped while this call was running. It may or may not have completed — check the actual state before retrying anything with side effects."
                } else {
                    "Not executed: the harness stopped before this call ran."
                };
                let _ = self.append(
                    rt,
                    EntryBody::ToolResult {
                        call_id: call_id.clone(),
                        tool: name.clone(),
                        content: vec![ContentPart::text(text)],
                        is_error: true,
                        artifact: None,
                        details: Some(json!({ "interrupted": true, "started": started })),
                    },
                );
            }
        }
    }

    fn run(self: Arc<Self>, rt: Arc<Rt>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        let turn = async move {
            let live = self.stops_here();
            let attempt_id = self.begin_attempt(&rt);
            {
                let mut st = rt.st.lock();
                st.running = true;
                st.cancel = Some(cancel.clone());
                st.turn_stops = st.next_turn_stops.take().unwrap_or(live);
                st.internal.turn_marks = st.turn_stops.clone();
                st.claude_limit_notified = false;
                st.claude_limit_window = None;
            }
            // Kept before it acts: a turn continued after a restart is still
            // from before the Stops that came after it.
            self.save(&rt);
            self.set_status(&rt, SessionStatus::Running);
            self.job_turn_started(&rt);
            let began = (
                std::time::Instant::now(),
                now_ms(),
                rt.st.lock().info.last_seq,
            );
            // A new chat is named as it starts (from its first message at
            // once, refined by a model alongside the turn), not after a
            // first turn that may take hours.
            let (untitled, private, chat) = {
                let st = rt.st.lock();
                (
                    !st.internal.titled,
                    st.info.private,
                    st.info.kind == SessionKind::Chat,
                )
            };
            if untitled && private {
                self.local_title(&rt);
            } else if untitled && chat {
                tokio::spawn(self.clone().generate_title(rt.clone()));
            }
            let room = is_room(&rt.st.lock().info);
            let claude = !room && self.runs_claude_code(&rt.st.lock());
            let outcome = if room {
                self.room_rounds(&rt, &cancel).await
            } else if claude {
                crate::activity::monitor(
                    &crate::activity::CLAUDE,
                    self.claude_code_turn(&rt, &cancel),
                )
                .await
            } else {
                self.turn_loop(&rt, &cancel).await
            };
            self.finish_redactions(&rt);
            // Cut off by a planned stop: no error, no "Stopped", and the
            // session stays marked running, so the next start continues it.
            if !matches!(outcome, Outcome::Done)
                && self.stopping.load(std::sync::atomic::Ordering::SeqCst)
            {
                tracing::info!(
                    "session {}: turn cut off by the server stopping; it continues after the restart",
                    rt.st.lock().info.id
                );
                return;
            }
            self.release_provisional_steps(&rt);
            #[cfg(debug_assertions)]
            {
                let id = rt.st.lock().info.id.clone();
                let gate = self.final_admission_gates.lock().remove(&id);
                if let Some(gate) = gate {
                    gate.reached.notify_one();
                    gate.release.notified().await;
                }
            }
            // Classify outside the lock, but do not commit a terminal yet.
            let result = self.attempt_result(&rt, attempt_id.as_deref(), &outcome);
            // Read the local Stop snapshot before taking the admission lock.
            // Done is not authority after a Stop, even before its watcher ticks.
            let here = self.here_now();
            let (id, first, now, worked_for, limit_notified, limit_window, outcome) = {
                let mut st = rt.st.lock();
                // Effective cause and write-once boundary share the admission
                // lock with Stop. A stale Done can never persist Completed.
                let cancelled = st
                    .cancel
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                    || here.stopped
                    || !here.since(&st.turn_stops);
                let outcome = if cancelled { Outcome::Aborted } else { outcome };
                let terminal = if cancelled {
                    Some(crate::store::AttemptOutcome::Stopped)
                } else {
                    result
                };
                let end = st.info.last_seq;
                if let (Some(id), Some(terminal)) = (attempt_id.as_ref(), terminal)
                    && let Some(attempt) = st.internal.attempts.iter_mut().find(|a| &a.id == id)
                {
                    attempt.finish(terminal, end, None);
                }
                let status = match &outcome {
                    Outcome::Done => SessionStatus::Idle,
                    Outcome::Aborted
                        if st.info.parent_call.is_some() && st.internal.subagent.is_some() =>
                    {
                        SessionStatus::Interrupted
                    }
                    Outcome::Aborted => SessionStatus::Idle,
                    Outcome::Failed(_) => SessionStatus::Error,
                };
                // Admission and the idle transition share this lock. No
                // successful run can leave accepted work behind it.
                let now = st.finish_admission(&outcome, status, &here);
                let worked_for = if st.running {
                    None
                } else {
                    // Capture this exact terminal under the admission lock
                    // that consumes working_for, before another send can run.
                    let terminal = attempt_id
                        .as_ref()
                        .and_then(|id| st.internal.attempts.iter().find(|a| &a.id == id))
                        .cloned();
                    if let Some(receipt) = st.bot_receipt.take() {
                        *receipt.lock() = terminal.clone();
                    }
                    st.info.working_for.take().map(|wf| (wf, terminal))
                };
                (
                    st.info.id.clone(),
                    !st.internal.titled,
                    now,
                    worked_for,
                    st.claude_limit_notified,
                    st.claude_limit_window.clone(),
                    outcome,
                )
            };
            // One content-free line per turn (crate::trace).
            {
                let (kind, private) = {
                    let st = rt.st.lock();
                    (st.info.kind, st.info.private)
                };
                let entries = self.store.entries(&id, began.2).unwrap_or_default();
                let end = match &outcome {
                    Outcome::Done => crate::trace::TurnEnd::Done,
                    Outcome::Aborted => crate::trace::TurnEnd::Stopped,
                    Outcome::Failed(e) => crate::trace::TurnEnd::Failed(e),
                };
                crate::trace::turn(&id, kind, private, &entries, began.0.elapsed(), end);
            }
            match &outcome {
                Outcome::Aborted => self.notice(&rt, NoticeLevel::Info, "Stopped."),
                Outcome::Failed(e) => self.notice(&rt, NoticeLevel::Error, e.clone()),
                Outcome::Done => {}
            }
            // Long work (overnight, say) ended while the user is likely away.
            // It goes on while its own helpers work (their reports start the
            // next turn) or a queued message is next: one notification at
            // the end of all of it, never "Finished" while work continues.
            let goes_on =
                matches!(outcome, Outcome::Done) && (!now.is_empty() || self.helpers_at_work(&id));
            let began = {
                let mut st = rt.st.lock();
                let span = st.work_span.take().unwrap_or(began);
                if goes_on {
                    st.work_span = Some(span);
                }
                span
            };
            let long = self.config.read().agent.notify_after_minutes;
            if long > 0.0
                && worked_for.is_none()
                && !goes_on
                && !limit_notified
                && began.0.elapsed().as_secs_f64() >= long * 60.0
            {
                let took = human_wait(began.0.elapsed());
                match &outcome {
                    Outcome::Done => {
                        let said = self.last_words(&id, began.2);
                        let body = if said.is_empty() {
                            format!("Worked {took}.")
                        } else {
                            format!("Worked {took}. {said}")
                        };
                        if !said.is_empty()
                            || !self
                                .store
                                .entries(&id, began.2)
                                .unwrap_or_default()
                                .iter()
                                .any(crate::tools::meta::is_protocol_entry)
                        {
                            self.notify_long_work_window(
                                &rt,
                                began.1,
                                "Finished",
                                body,
                                limit_window.as_ref().map(|(id, at)| (id.as_str(), *at)),
                            );
                        }
                    }
                    Outcome::Failed(e) => {
                        self.notify_long_work_window(
                            &rt,
                            began.1,
                            "Stopped by an error",
                            format!("After {took}: {}", one_line(e, 200)),
                            limit_window.as_ref().map(|(id, at)| (id.as_str(), *at)),
                        );
                    }
                    Outcome::Aborted => {}
                }
            }
            let handoff = !now.is_empty();
            self.turn_settled(&rt, &outcome, handoff || worked_for.is_some(), began);
            if handoff {
                // The next turn starts as a send to the idle chat would.
                self.open_turn(&rt, now);
            } else {
                // Status was decided under admission, never overwrite a new
                // send that started after that boundary.
                self.save(&rt);
            }
            if let Some((wf, terminal)) = worked_for {
                self.save(&rt);
                self.deliver_bot_reply(&rt, wf, terminal);
            }
            self.turn_ended.notify_waiters();
            self.queue_changed(&rt.st.lock());
            let private = rt.st.lock().info.private;
            if first && private {
                self.local_title(&rt);
            } else if first && rt.st.lock().info.kind == SessionKind::Chat {
                tokio::spawn(self.clone().generate_title(rt.clone()));
            }
            if matches!(outcome, Outcome::Done) {
                let (interval, idle) = {
                    let cfg = self.config.read();
                    (cfg.agent.review_interval, cfg.agent.review_idle_minutes)
                };
                let unreviewed = {
                    let mut st = rt.st.lock();
                    if st.info.kind == SessionKind::Chat
                        && !st.info.private
                        && st.depth == 0
                        && interval > 0
                        && self.config.read().memory.enabled
                    {
                        st.internal.user_turns += 1;
                        Some(
                            st.internal
                                .user_turns
                                .saturating_sub(st.internal.reviewed_turns),
                        )
                    } else {
                        None
                    }
                };
                match unreviewed {
                    Some(n) if n >= interval => {
                        self.save(&rt);
                        tokio::spawn(self.clone().review(rt.clone()));
                    }
                    // Short chats teach it too: review once it has gone quiet.
                    Some(n) if n >= 2 && idle > 0 => {
                        self.save(&rt);
                        let me = self.clone();
                        let rt = rt.clone();
                        let turns = rt.st.lock().internal.user_turns;
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(idle as u64 * 60))
                                .await;
                            let (quiet, id) = {
                                let st = rt.st.lock();
                                (
                                    !st.running
                                        && st.internal.user_turns == turns
                                        && st.internal.reviewed_turns < turns,
                                    st.info.id.clone(),
                                )
                            };
                            if quiet && me.sessions.lock().contains_key(&id) {
                                me.review(rt).await;
                            }
                        });
                    }
                    _ => {}
                }
            }
            if handoff {
                self.run(rt, CancellationToken::new()).await;
            }
        };
        Box::pin(crate::activity::monitor(&crate::activity::TURN, turn))
    }

    async fn turn_loop(self: &Arc<Self>, rt: &Arc<Rt>, cancel: &CancellationToken) -> Outcome {
        let max_steps = self.config.read().agent.max_steps.max(1);
        // The first time a model on the user's own server is used: one
        // tiny check that it calls tools (see `engine/small.rs`).
        self.check_model_once(rt, cancel).await;
        let mut guard = self.guardrails(rt);
        // Small model mode is decided with the prompt (first request below):
        // its tighter loop limits are set once that is known.
        let mut guard_limit_set = false;
        // Where this user turn's messages start (what the user said about
        // running things; see `verify_note`).
        let mut turn_from = {
            let id = rt.st.lock().info.id.clone();
            self.store.last_assistant_seq(&id).unwrap_or(0)
        };
        let mut overflow_retry = false;
        // Where the observation budget looks again after a look that found
        // too little to trim (see `trim_observations`).
        let mut observe_floor = 0;
        // A fallback model that took over after the session's model failed
        // stays for the rest of this turn (prompt caches); the next turn
        // tries the session's own model again.
        let mut fallback: Option<ModelRef> = None;
        // The completion check is asked at most once per user turn, and only
        // after a turn that used tools: a plain reply (a question about
        // status, small talk) never gets one.
        let mut checked = false;
        let mut check = TurnWork::default();
        let mut nudged = Nudged::default();
        let provider = rt.st.lock().info.model.provider.clone();
        self.providers.ensure_discovered(&provider).await;
        for step in 0.. {
            if cancel.is_cancelled() {
                return Outcome::Aborted;
            }
            if step > 0 && step % max_steps == 0 {
                // Long work (hours, overnight) checks in at every
                // `max_steps` instead of stopping, unless it is going in
                // circles or has run four rounds; subagents have budgets.
                // A helper whose own budget is longer goes on like a chat.
                let (top, helper_budget) = {
                    let st = rt.st.lock();
                    (st.depth == 0, st.budget.is_some())
                };
                // Helpers have their own explicit budget, not the chat's
                // four-round ceiling. Loop detection still protects both.
                if (!helper_budget && (!top || step >= max_steps * LONG_TURN_ROUNDS))
                    || guard.spinning()
                {
                    let why = if guard.spinning() {
                        " It seemed to be going in circles (repeated or failing calls)."
                    } else {
                        ""
                    };
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!("Paused after {step} steps in one turn (agent.max_steps is {max_steps}).{why} Send a message to continue."),
                    );
                    return Outcome::Done;
                }
                guard.new_round();
                let ledger = self
                    .tool_specs(&rt.st.lock())
                    .iter()
                    .any(|t| t.name == "task");
                let _ = self.append(
                    rt,
                    EntryBody::User {
                        content: vec![ContentPart::text(format!(
                            "[Progress check] {step} steps into this turn.{} If the work is moving forward, carry on; \
                             if you are blocked or repeating yourself, stop and say plainly what is in the way.",
                            if ledger {
                                " Bring the task ledger up to date (`task`) with what is done and what is next."
                            } else {
                                ""
                            }
                        ))],
                        author: Some("harness".into()),
                    },
                );
            }
            self.check_budget(rt);
            let steer = rt.st.lock().take_queue(Queue::Steer);
            if !steer.is_empty() {
                for p in steer {
                    if p.author
                        .as_deref()
                        .is_some_and(|a| a.starts_with("subagent:"))
                    {
                        checked = false; // New dependency result, not a replay of the old reasons.
                    }
                    let _ = self.append_waiting(rt, p);
                }
                self.queue_changed(&rt.st.lock());
            }

            let mut prepared = match self.prepare_request(rt) {
                Ok(r) => r,
                Err(e) => return Outcome::Failed(e.to_string()),
            };
            // Small model mode (decided with the prompt just built): loops
            // are stopped sooner.
            if !guard_limit_set {
                guard_limit_set = true;
                if self.repeat_limit(rt).is_some() {
                    guard.set_limits(LoopLimits::small());
                }
            }
            // Compaction only when this request would cross the trigger (the
            // transcript is read and estimated once per step, not twice).
            if self.over_trigger(rt, &prepared) {
                // Output the model saw may be replaced by references: a file
                // read again after this is not a loop.
                guard.context_trimmed();
                if let Err(e) = self.compact(rt, false, None, cancel).await
                    && !cancel.is_cancelled()
                {
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!("Context compaction failed: {e}"),
                    );
                }
                if cancel.is_cancelled() {
                    return Outcome::Aborted;
                }
                prepared = match self.prepare_request(rt) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Failed(e.to_string()),
                };
            } else {
                if self.trim_observations(rt, &prepared, &mut observe_floor) {
                    guard.context_trimmed();
                    prepared = match self.prepare_request(rt) {
                        Ok(r) => r,
                        Err(e) => return Outcome::Failed(e.to_string()),
                    };
                }
                self.precompact_early(rt, &prepared);
            }
            // The memory review's steps give way to the turns of the chat it
            // reviews where both use one server that takes one request at a time.
            if let Some(chat) = self.helped_chat(rt) {
                tokio::select! {
                    _ = self.when_idle(&chat, &prepared.model) => {}
                    _ = cancel.cancelled() => return Outcome::Aborted,
                }
            }
            let (resp, answered_by) = match self
                .call_model(rt, &prepared, cancel, &mut fallback)
                .await
            {
                Ok(r) => r,
                Err(e) if e.kind == ErrorKind::ContextOverflow && !overflow_retry => {
                    overflow_retry = true;
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        "The provider reported the context is too long; compacting and retrying.",
                    );
                    if let Err(e) = self.compact(rt, true, None, cancel).await {
                        return Outcome::Failed(format!(
                            "Context overflow and compaction failed: {e}"
                        ));
                    }
                    guard.context_trimmed();
                    continue;
                }
                Err(e) => {
                    let hint = match e.kind {
                        ErrorKind::Auth => {
                            " — check the provider's sign-in or API key in settings."
                        }
                        ErrorKind::RateLimited => {
                            " — the provider's rate or usage limit was reached."
                        }
                        _ if e.unreachable => " — start it, or pick another model for this chat.",
                        _ => "",
                    };
                    return Outcome::Failed(format!("Model request failed: {e}{hint}"));
                }
            };
            overflow_retry = false;
            let LlmResponse {
                mut content,
                usage,
                mut stop,
                allow_tool_repair,
            } = resp;
            let failed_reply = (stop == StopReason::Error).then_some(FAILED_REPLY_NOTICE);
            self.record_usage(rt, &usage, prepared.estimate);
            // Cut off because it kept repeating itself (`stream_retrying`).
            let repeated = std::mem::take(&mut rt.st.lock().repeated);
            // Near misses (mostly small models): calls written as text, other
            // harnesses' tool names and argument shapes. Fixed before the
            // reply is stored, so the transcript and the model's own history
            // show the call as it ran.
            // (None while no tool may be called, though they stay listed.)
            let offered: &[crate::llm::ToolSpec] = if rt.st.lock().no_tool_calls {
                &[]
            } else {
                &prepared.request.tools
            };
            // A reply that didn't end normally (the output limit, a content
            // filter or refusal, an error): its calls may be cut short, so
            // their brackets are never closed by the JSON repair.
            let cut_short =
                !allow_tool_repair || !matches!(stop, StopReason::Stop | StopReason::ToolUse);
            if !cut_short && calls::recover_text_calls(&mut content, offered) > 0 {
                stop = StopReason::ToolUse;
            }
            calls::normalize_reply(&mut content, offered, cut_short);
            // GPT/Codex models' own edit form ("*** Begin Patch").
            if !cut_short && self.takes_patches(&answered_by) {
                calls::patch_calls(&mut content, offered);
            }
            if stop == StopReason::Length {
                // Arguments cut off by the output limit: say so, not just "invalid JSON".
                for p in content.iter_mut() {
                    if let AssistantPart::ToolCall { arguments, .. } = p
                        && let Some(o) = arguments.as_object_mut()
                        && o.contains_key("__invalid_arguments")
                    {
                        o.insert("__cut_off".into(), Value::Bool(true));
                    }
                }
            }
            let calls: Vec<(String, String, Value)> = content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some((id.clone(), name.clone(), arguments.clone())),
                    _ => None,
                })
                .collect();
            let aborted = stop == StopReason::Aborted;
            // An answer that ends by asking the user something they must
            // answer waits for them, not for a completion check. A courtesy
            // question after finished work ("Want me to also add tests?")
            // doesn't: what changed still gets its check.
            let asks = content
                .iter()
                .any(|p| matches!(p, AssistantPart::Text { text } if loops::waits_for_user(text)));
            if !content.is_empty() || !aborted {
                let author = self.speaker_id(&rt.st.lock());
                let entry = match self.append(
                    rt,
                    EntryBody::Assistant {
                        content,
                        model: answered_by,
                        usage,
                        stop,
                        error: failed_reply.map(str::to_owned),
                        author,
                    },
                ) {
                    Ok(e) => e,
                    Err(e) => return Outcome::Failed(e.to_string()),
                };
                if aborted {
                    return Outcome::Aborted;
                }
                if let Some(error) = failed_reply {
                    // Preserve partial text, but an unconfirmed/error ending
                    // cannot act or become a successful completion next step.
                    for (call_id, tool, _) in calls {
                        let _ = self.append(rt, EntryBody::ToolResult {
                            call_id, tool,
                            content: vec![ContentPart::text("Not executed: the model did not finish its reply successfully.")],
                            is_error: true, artifact: None, details: None,
                        });
                    }
                    return Outcome::Failed(error.into());
                }
                if calls.is_empty() {
                    // Nothing said and nothing called (reasoning only, or an
                    // empty reply), or a reply cut off at the output limit: one
                    // nudge each per turn, then the turn ends as before.
                    let said = entry_said(&entry);
                    let nudge = if repeated {
                        (!nudged.repeat).then(|| {
                            nudged.repeat = true;
                            "[Repetition] Your reply started repeating. Continue from where it was still useful, briefly."
                        })
                    } else if stop == StopReason::Length {
                        (!nudged.cut).then(|| {
                            nudged.cut = true;
                            if said {
                                "[Continue] Your reply was cut off at the output limit. Continue exactly where it stopped, without repeating what you already wrote."
                            } else {
                                "[Continue] Your reply was cut off at the output limit while you were still thinking. Be brief: answer, or call a tool, now."
                            }
                        })
                    } else if !said
                        && stop == StopReason::Stop
                        && rt.st.lock().info.kind != SessionKind::Subagent
                    {
                        // Helpers get their single budget-aware report nudge
                        // in run_child, shared with Claude Code.
                        (!nudged.empty).then(|| {
                            nudged.empty = true;
                            "[Empty reply] Your last reply had no text and no tool call. Carry on: call a tool, or answer the user."
                        })
                    } else if stop == StopReason::Stop
                        && !nudged.action
                        && self.wants_action_nudge(rt, &entry, offered)
                    {
                        nudged.action = true;
                        Some(
                            "[No tool was called. If you meant to act, call the tool now; if you are finished, give the final answer.]",
                        )
                    } else {
                        None
                    };
                    if let Some(note) = nudge {
                        let _ = self.append(
                            rt,
                            EntryBody::User {
                                content: vec![ContentPart::text(note)],
                                author: Some("harness".into()),
                            },
                        );
                        continue;
                    }
                    if repeated {
                        self.notice(
                            rt,
                            NoticeLevel::Warning,
                            "The model's reply kept repeating itself, so it was cut off.",
                        );
                    } else if stop == StopReason::Length {
                        self.notice(
                            rt,
                            NoticeLevel::Warning,
                            "The response reached the model's output limit.",
                        );
                    } else if !said && stop == StopReason::Stop {
                        self.notice(
                            rt,
                            NoticeLevel::Warning,
                            "The model ended its turn without saying anything.",
                        );
                    }
                    // Steering that arrived during the final answer must not be
                    // stranded: take another step so it is read now.
                    if rt.st.lock().has_admitted(Queue::Steer) {
                        continue;
                    }
                    let next = {
                        let mut st = rt.st.lock();
                        // In channels, follow-ups start a new round of replies instead.
                        if st.followups.is_empty() || is_room(&st.info) {
                            None
                        } else {
                            st.take_one(Queue::FollowUp)
                        }
                    };
                    match next {
                        Some(p) => {
                            let _ = self.append_waiting(rt, p);
                            // Delivered: its card above the composer goes.
                            self.queue_changed(&rt.st.lock());
                            checked = false;
                            check = TurnWork::default();
                            nudged = Nudged::default();
                            guard.new_turn();
                            turn_from = entry.seq;
                            continue;
                        }
                        None => {
                            // About to finish. One end-of-turn note at most:
                            // first, whether code changed since the last check
                            // (the harness knows; `engine/verify.rs`), …
                            if !checked
                                && !asks
                                && !repeated
                                && !check.delegated
                                && let Some(note) = self.verify_note(rt, &mut guard, turn_from)
                            {
                                checked = true;
                                let _ = self.append(
                                    rt,
                                    EntryBody::User {
                                        content: vec![ContentPart::text(note)],
                                        author: Some("harness".into()),
                                    },
                                );
                                continue;
                            }
                            // … else, if the ledger names done-when criteria or
                            // open plan items, ask once for evidence.
                            if !checked
                                && !asks
                                && !repeated
                                // A ledger is independent of tool/delegation activity.
                                && (check.wants_check() || rt.st.lock().info.task.is_some())
                                && let Some(note) = self.completion_check(rt)
                            {
                                checked = true;
                                let _ = self.append(
                                    rt,
                                    EntryBody::User {
                                        content: vec![ContentPart::text(note)],
                                        author: Some("harness".into()),
                                    },
                                );
                                continue;
                            }
                            return Outcome::Done;
                        }
                    }
                }
                check.batch(&calls);
                // Small model mode: the plan's next step now and then.
                let note = self.plan_note(rt, step);
                self.run_tools(rt, calls, entry.seq, cancel, &mut guard, note)
                    .await;
                if guard.stuck() && !cancel.is_cancelled() {
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!("Paused after {} steps: it seemed to be going in circles (the same calls failing or repeating). Send a message to continue.", step + 1),
                    );
                    return Outcome::Done;
                }
            } else {
                return Outcome::Aborted;
            }
        }
        Outcome::Done
    }

    /// The context a chat on `model` works in (shown as its window): the
    /// model's, capped by `context.max_tokens` where Tessera keeps the
    /// context; Claude Code keeps its own.
    fn working_window(&self, model: &ModelRef, window: u64) -> u64 {
        if self.providers.kind_of(&model.provider) == Some(ProviderKind::ClaudeCode) {
            return window;
        }
        context::working_window(window, &self.config.read().context)
    }

    /// Bytes of one tool result shown to the model in `session_id`
    /// (`context::tool_output_limit` of its working window).
    fn tool_output_limit(&self, session_id: &str) -> usize {
        let configured = self.config.read().context.max_tool_output;
        match self
            .rt(session_id)
            .map(|rt| rt.st.lock().info.context.window)
        {
            Ok(window) if window > 0 => context::tool_output_limit(configured, window),
            _ => configured,
        }
    }

    /// Whether `p` (calibrated) is past the compaction trigger, as
    /// [`compact`](Self::compact) would judge it.
    fn over_trigger(&self, rt: &Rt, p: &Prepared) -> bool {
        let Some(info) = self.providers.model_info(&p.model) else {
            return false;
        };
        let cfg = self.context_cfg(&p.model, info.context_window);
        let budget = context::budget(info.context_window, info.max_output, &cfg);
        let cal = rt.st.lock().internal.calibration.unwrap_or(1.0);
        (p.estimate as f64 * cal) as u64 > budget.trigger
    }

    fn record_usage(&self, rt: &Rt, usage: &Usage, estimate: u64) {
        let mut st = rt.st.lock();
        if let Some(c) = usage.cost_usd {
            *st.info.usage.cost_usd.get_or_insert(0.0) += c;
        }
        st.info.usage.input += usage.input;
        st.info.usage.output += usage.output;
        st.info.usage.cache_read += usage.cache_read;
        st.info.usage.cache_write += usage.cache_write;
        let actual = usage.input + usage.cache_read + usage.cache_write;
        if actual > 0 && estimate > 1000 {
            let ratio = (actual as f64 / estimate as f64).clamp(0.4, 2.5);
            let prev = st.internal.calibration.unwrap_or(ratio);
            st.internal.calibration = Some(prev * 0.5 + ratio * 0.5);
            st.info.context.tokens = actual + usage.output;
            st.info.context.source = Some("reported".into());
        }
    }

    /// When the loop guard speaks up for this session. Small-model profiles
    /// lower `block_after` (see `LoopLimits::small`).
    fn loop_limits(&self, _st: &RtState) -> LoopLimits {
        LoopLimits::from_config(&self.config.read().agent)
    }

    /// A turn's loop guard and verification bookkeeping.
    fn guardrails(&self, rt: &Rt) -> Guardrails {
        let st = rt.st.lock();
        let mode = self.config.read().agent.verify_check;
        Guardrails::new(
            self.loop_limits(&st),
            verify::Verification::new(mode, PathBuf::from(&st.info.cwd))
                .with_protected(self.protected_paths()),
        )
    }

    /// The note before a turn ends when it changed code and nothing checked
    /// it since (`engine/verify.rs`): not where no command can run (private
    /// chats, channels, read-only, a subagent writing its report), and not
    /// when the user (or the agent that started this one) said not to run
    /// anything this turn.
    fn verify_note(&self, rt: &Rt, guard: &mut Guardrails, turn_from: u64) -> Option<String> {
        if guard.verify.unchecked().is_empty() {
            return None;
        }
        let id = {
            let st = rt.st.lock();
            let runs = self
                .callable_tools(&st)
                .iter()
                .any(|t| t.name == "bash" || t.name == "machines");
            if st.info.private || is_room(&st.info) || !runs {
                return None;
            }
            st.info.id.clone()
        };
        let told_not_to = self
            .store
            .entries(&id, turn_from)
            .unwrap_or_default()
            .iter()
            .any(|e| match &e.body {
                EntryBody::User { content, author } => {
                    author.as_deref() != Some("harness")
                        && verify::forbids_running(&plain_text(content))
                }
                _ => false,
            });
        if told_not_to {
            return None;
        }
        guard.verify.end_note()
    }

    /// A final reply that says it is about to act but called nothing
    /// (`loops::announces_action`), where tools can be called.
    fn wants_action_nudge(&self, rt: &Rt, entry: &Entry, offered: &[crate::llm::ToolSpec]) -> bool {
        if offered.is_empty() || !self.config.read().agent.action_nudge {
            return false;
        }
        let st = rt.st.lock();
        if st.no_tool_calls || is_room(&st.info) {
            return false;
        }
        drop(st);
        let EntryBody::Assistant { content, .. } = &entry.body else {
            return false;
        };
        let text: String = content
            .iter()
            .filter_map(|p| match p {
                AssistantPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        loops::announces_action(&text)
    }

    /// `model` writes edits as `apply_patch` patches (GPT/Codex), and those
    /// are taken (`agent.apply_patch`).
    fn takes_patches(&self, model: &ModelRef) -> bool {
        self.config.read().agent.apply_patch
            && (self.providers.kind_of(&model.provider) == Some(ProviderKind::OpenaiCodex)
                || crate::tools::patch::patch_family(&model.model))
    }

    /// A result/recovery changes the reasons for leaving work open without
    /// changing its ledger text. Drop pending too: a stale in-flight mark
    /// must not acknowledge the newly actionable generation.
    fn invalidate_completion(&self, rt: &Rt) {
        let mut st = rt.st.lock();
        st.internal.completion_acknowledged = None;
        st.internal.completion_pending = None;
        drop(st);
        self.save(rt);
    }
    /// The harness's check before a turn ends (see `meta::completion_note`):
    /// only with a task ledger, never in private chats or channels, and not
    /// for a subagent whose tools were taken away to write its report.
    fn completion_check(&self, rt: &Rt) -> Option<String> {
        if !self.config.read().agent.completion_check {
            return None;
        }
        let mut st = rt.st.lock();
        if st.info.private
            || is_room(&st.info)
            || st.no_tool_calls
            || st.tools_only.as_ref().is_some_and(|t| !t.contains(&"task"))
        {
            return None;
        }
        let ledger = st.info.task.as_ref()?;
        let note = crate::tools::meta::completion_note(ledger)?;
        let state = crate::tools::meta::completion_state(ledger);
        if st.internal.completion_acknowledged.as_ref() == Some(&state) {
            return None;
        }
        st.internal.completion_pending = Some(state);
        drop(st);
        self.save(rt);
        Some(note)
    }

    /// Tell the user (once per session) what the injection scan found in the
    /// context this session loads. Called with the session locked, so the
    /// notice goes straight to the store.
    fn report_findings(&self, st: &mut RtState, findings: Vec<String>) {
        let new: Vec<String> = findings
            .into_iter()
            .filter(|f| !st.internal.flagged.contains(f))
            .collect();
        if new.is_empty() || st.info.private {
            return;
        }
        st.internal.flagged.extend(new.iter().cloned());
        let text = format!(
            "Possible prompt injection: {}. The agent was told it is data, not instructions (hidden characters are removed); check where it came from.",
            new.join("; ")
        );
        tracing::warn!("session {}: {text}", st.info.id);
        let body = EntryBody::Notice {
            level: NoticeLevel::Warning,
            text,
            learned: Vec::new(),
        };
        if let Ok(entry) = self.store.append(&st.info.id, body) {
            st.info.last_seq = entry.seq;
            st.info.updated_at = entry.ts;
            let _ = self.events.send(Event::EntryAdded {
                session_id: st.info.id.clone(),
                entry,
            });
        }
    }

    /// The tools offered to `st`'s model: everything it may use, or in
    /// small model mode the core tools in their simple forms, or at a lean
    /// start all but the tools it keeps off until one is called.
    fn tool_specs(&self, st: &RtState) -> Vec<crate::llm::ToolSpec> {
        let specs = self.lean_start_tools(st, self.shaped_tools(st, self.base_tool_specs(st)));
        self.mcp_later_tools(st, specs)
    }

    /// Every tool `st` may use (whatever small model mode or a lean start
    /// keeps off): a terminal chat's lean coding set, or everything its
    /// permissions, bot and settings allow.
    fn base_tool_specs(&self, st: &RtState) -> Vec<crate::llm::ToolSpec> {
        let mut specs = self.permitted_tool_specs(st);
        if st.info.lean {
            specs.retain(|t| crate::profiles::LEAN_TOOLS.contains(&t.name.as_str()));
        }
        specs
    }

    /// What `st` may use before a terminal chat's lean set is applied.
    fn permitted_tool_specs(&self, st: &RtState) -> Vec<crate::llm::ToolSpec> {
        let memory = self.config.read().memory.enabled;
        let mut specs = self
            .registry
            .read()
            .specs(self.effective_permission(st), st.depth, memory);
        if let Some(only) = &st.tools_only {
            specs.retain(|t| only.contains(&t.name.as_str()));
        }
        // The persisted role also applies before a queued helper runs, and
        // when a finished helper is loaded again after eviction or restart.
        if let Some(allow) = st.internal.subagent.as_ref().and_then(|s| s.tools.as_ref()) {
            specs.retain(|t| crate::agents::tool_allowed(allow, &t.name));
        }
        if st.info.private {
            // Private chats: the web, nothing that reads or writes local state.
            specs.retain(|t| matches!(t.name.as_str(), "web_search" | "web_fetch"));
            return specs;
        }
        let speaker = self.speaker_bot(st);
        // The memory review curates the bot's memory whatever its preset
        // (`tools_only` already limits it to memory and skills).
        if let Some(allowed) = speaker
            .as_ref()
            .filter(|_| !st.info.internal)
            .and_then(crate::bots::allowed_tools)
        {
            specs.retain(|t| allowed.iter().any(|a| a == &t.name));
        }
        // message_bot only when someone is reachable.
        let bots = self.all_bots();
        let chats = self.config.read().security.bot_handoff;
        let reachable = bots
            .iter()
            .any(|b| crate::bots::may_hand_to(speaker.as_ref(), b, chats));
        if !reachable || st.bot_depth >= 3 {
            specs.retain(|t| t.name != "message_bot");
        }
        if !self.helpers_enabled(st) {
            specs.retain(|t| !matches!(t.name.as_str(), "agent" | "agents"));
        }
        // Do not advertise computers this actor cannot use at all.
        let (machines, desktop) = self.usable_machines(st, speaker.as_ref());
        if !machines {
            specs.retain(|t| t.name != "machines");
        }
        if !desktop {
            specs.retain(|t| t.name != "desktop");
        }
        // lsp only where a language server matched the directory at creation
        // (and not read-only, where starting one would run project code).
        let read_only = self.effective_permission(st) == PermissionMode::ReadOnly;
        if !self
            .lsp
            .read()
            .as_ref()
            .is_some_and(|l| !l.detect(Path::new(&st.info.cwd)).is_empty())
            || (read_only && !self.config.read().lsp.in_read_only)
        {
            specs.retain(|t| t.name != "lsp");
        }
        // The agent types this session can start (its project's and the user's too).
        if let Some(agent) = specs.iter_mut().find(|t| t.name == "agent") {
            agent.description = self.agent_description(st);
        }
        specs
    }

    /// The tools the model may call now: none while it writes a report.
    fn callable_tools(&self, st: &RtState) -> Vec<crate::llm::ToolSpec> {
        if st.no_tool_calls {
            return Vec::new();
        }
        self.tool_specs(st)
    }

    /// A registered helper tool denied by its live ceiling is not unknown.
    fn unavailable_tool(&self, st: &RtState, name: &str, available: &[String]) -> String {
        if st.budget.is_some() && st.no_tool_calls {
            return helper_limits::STEP_EXHAUSTED.into();
        }
        if st.info.kind == SessionKind::Subagent && self.registry.read().get(name).is_some() {
            let why = "the role, read-only setting or current parent/bot policy excludes it";
            return format!(
                "Not executed: tool `{name}` is registered but unavailable to this helper: {why}. Effective tools now: {}. Use an equipped role or return the blocker to the parent; do not work around a policy refusal.",
                available.join(", ")
            );
        }
        format!(
            "Unknown tool `{name}`. Available tools: {}.",
            available.join(", ")
        )
    }

    /// The chat switch and no-nesting rule apply to every helper entry point.
    fn helpers_enabled(&self, st: &RtState) -> bool {
        st.info.subagents
            && st.depth == 0
            && st.info.kind != SessionKind::Subagent
            && self.config.read().agent.max_subagents > 0
    }

    fn helpers_offered(&self, st: &RtState) -> bool {
        if !self.helpers_enabled(st) || st.info.private || st.info.lean {
            return false;
        }
        // Match the helper filters without building every tool's JSON schema
        // for each streamed Claude line. Both entry points can start tasks.
        let allowed = self
            .speaker_bot(st)
            .filter(|_| !st.info.internal)
            .as_ref()
            .and_then(crate::bots::allowed_tools);
        self.registry
            .read()
            .available(self.effective_permission(st), st.depth, true)
            .any(|t| {
                matches!(t.name(), "agent" | "agents")
                    && st
                        .tools_only
                        .as_ref()
                        .is_none_or(|only| only.contains(&t.name()))
                    && allowed
                        .as_ref()
                        .is_none_or(|a| a.iter().any(|name| name == t.name()))
            })
    }

    fn helper_note(&self, st: &RtState) -> &'static str {
        if self.helpers_offered(st) {
            "Helpers are on in this chat."
        } else {
            "Helpers are off in this chat; work without them."
        }
    }
    fn system_prompt(&self, st: &mut RtState) -> String {
        let helpers = self.helpers_offered(st);
        if !st.info.internal && st.prompt_helpers != Some(helpers) {
            st.internal.system_prompt = None;
            st.internal.bot_prompts.clear();
            st.prompt_helpers = Some(helpers);
        }
        let room = is_room(&st.info);
        let speaker = self.speaker_bot(st);
        if room {
            if let Some(p) = speaker
                .as_ref()
                .and_then(|b| st.internal.bot_prompts.get(&b.id))
            {
                return p.clone();
            }
        } else if let Some(p) = &st.internal.system_prompt {
            return p.clone();
        }
        if st.info.private {
            let prompt = format!(
                "{}\n\n{}",
                crate::prompt::private(&self.effective_model(st)),
                self.helper_note(st)
            );
            st.internal.system_prompt = Some(prompt.clone());
            return prompt;
        }
        let cfg = self.config.read().clone();
        let cwd = PathBuf::from(&st.info.cwd);
        // A terminal chat loads only what coding needs: no skills index or
        // contacts (it has neither tool), and of memory only the profile.
        let lean = st.info.lean;
        let skills = if lean {
            Vec::new()
        } else {
            self.skills_for(&cwd)
        };
        let bots = self.all_bots();
        // Small model mode and a lean start are decided here, with the
        // prompt, and frozen with it.
        if !room {
            st.internal.small = self.decide_small(st, &self.base_tool_specs(st));
            st.internal.lean_start = self.decide_lean_start(st);
            st.internal.mcp_later = self.decide_mcp_later(st);
        }
        let lean_start = !room && self.lean_start_state(st).is_some();
        let mut blocks = Vec::new();
        blocks.push(self.helper_note(st).to_string());
        if !lean {
            blocks.push(crate::bots::contacts_block(
                speaker.as_ref(),
                &bots,
                cfg.security.bot_handoff,
            ));
        }
        if let Some(b) = speaker.as_ref().filter(|b| !b.knowledge.is_empty()) {
            let notes: Vec<NoteInfo> = b
                .knowledge
                .iter()
                .filter_map(|id| self.store.note(id).ok().flatten())
                .map(|n| notes::note_info(&n))
                .collect();
            blocks.push(crate::bots::knowledge_block(&notes));
        }
        // A lean start lists the most recently used passwords, where
        // `save_secret` can list the rest.
        let secrets_cap = (lean_start
            && self
                .base_tool_specs(st)
                .iter()
                .any(|t| t.name == "save_secret"))
        .then_some(crate::profiles::LEAN_START_SECRETS);
        blocks.extend(self.secrets_block(st, secrets_cap));
        blocks.extend(self.sandbox_prompt());
        if let (true, Some(b)) = (room, speaker.as_ref()) {
            let members: Vec<&Bot> = bots
                .iter()
                .filter(|m| st.info.members.contains(&m.id))
                .collect();
            blocks.push(crate::bots::channel_block(&st.info.title, b, &members));
        }
        // A subagent started as a defined agent type: its instructions.
        if let Some(role) = st
            .internal
            .subagent
            .as_ref()
            .and_then(|s| s.role.as_deref())
        {
            blocks.push(format!("# Agent role\n{}", role.trim()));
        }
        // Rule files, memory and skills can carry hidden text or injected
        // instructions: the scans strip and label them, and the user is told.
        let (memory_block, mut findings) = if cfg.memory.enabled && lean {
            self.memory_for(speaker.as_ref()).profile_block_scanned()
        } else if cfg.memory.enabled {
            let (mut block, findings) = self.memory_for(speaker.as_ref()).prompt_block_scanned();
            // A bot with its own memory is told which memory is whose.
            if let Some(g) = speaker.as_ref().and_then(crate::bots::memory_guidance) {
                block.push_str(&g);
            }
            (block, findings)
        } else {
            (String::new(), Vec::new())
        };
        let (skills_index, skill_findings) = if lean_start {
            crate::skills::prompt_index_ranked(
                &skills,
                &self.skill_uses(),
                crate::profiles::LEAN_START_SKILLS_CHARS,
            )
        } else {
            crate::skills::prompt_index_scanned(&skills)
        };
        findings.extend(skill_findings);
        if lean_start {
            blocks.extend(self.lean_start_prompt_line(st));
        }
        blocks.extend(self.mcp_later_prompt_line(st));
        let (mut prompt, project_findings) =
            crate::prompt::build_scanned(&crate::prompt::PromptInputs {
                cwd: &cwd,
                model: &self.effective_model(st),
                kind: st.info.kind,
                memory_block,
                skills_index,
                extra: cfg.agent.instructions.as_deref(),
                tool_names: vec![],
                persona: speaker.as_ref().map(crate::bots::persona),
                blocks,
                protected: &self.protected_paths(),
                subagents: helpers,
                small: !room && self.small_state(st).is_some(),
                lean,
            });
        // `[tuning] read_paths = false`: `read` is offered without `paths`;
        // `plan_alongside = false`: no word on when to update the plan.
        let tuning = cfg.tuning_for(&self.effective_model(st));
        if tuning.read_paths == Some(false) {
            prompt = prompt.replacen(crate::prompt::READ_SEVERAL, "", 1);
        }
        if tuning.plan_alongside == Some(false) {
            prompt = prompt.replacen(crate::prompt::PLAN_ALONGSIDE, "", 1);
        }
        findings.splice(0..0, project_findings);
        self.report_findings(st, findings);
        match (room, speaker) {
            (true, Some(b)) => {
                st.internal.bot_prompts.insert(b.id, prompt.clone());
            }
            _ => {
                // Changed mid-chat (a setting, a new credential): reasoning
                // written under the old prompt isn't replayed. Claude refuses
                // it, and the prompt cache starts over here anyway.
                let digest = prompt_digest(&prompt);
                if st.internal.prompt_digest.is_some_and(|d| d != digest) {
                    st.internal.thinking_before = st.info.last_seq + 1;
                }
                st.internal.prompt_digest = Some(digest);
                st.internal.system_prompt = Some(prompt.clone());
            }
        }
        prompt
    }

    /// Every value that must never reach a model: credentials, vault
    /// secrets and the server's private key (the E2EE identity).
    fn redaction_secrets(&self) -> Vec<String> {
        let mut v = self.auth.secrets();
        if let Ok(text) = std::fs::read_to_string(self.paths.data_dir.join("identity.key"))
            && let Some(secret) = text.lines().nth(1).map(str::trim).filter(|s| s.len() >= 16)
        {
            v.push(secret.to_string());
        }
        v
    }

    /// Tessera's own credentials and settings, off limits to every tool.
    fn protected_paths(&self) -> crate::tools::Protected {
        let mut roots = vec![self.paths.data_dir.clone(), self.paths.config_dir.clone()];
        roots.extend(crate::config::Paths::app_data_dir());
        // Windows: the desktop app's web storage (WebView2), in LocalAppData.
        #[cfg(windows)]
        roots.extend(dirs::data_local_dir().map(|d| d.join("dev.tessera.app")));
        crate::tools::Protected {
            roots,
            // Readable exceptions: skills, uploads and what the browser
            // downloaded (read-only to commands).
            except: vec![
                self.paths.skills_dir(),
                self.paths.uploads_dir(),
                self.browser.server().downloads_dir().to_path_buf(),
            ],
        }
    }

    fn skills_for(&self, cwd: &Path) -> Vec<crate::skills::Skill> {
        let cfg = self.config.read();
        let roots = crate::skills::roots(cwd, &self.paths.skills_dir(), &cfg.skills.extra_dirs);
        crate::skills::discover_protected(&roots, &cfg.skills.disabled, &self.protected_paths())
    }

    /// What the recent conversation shows about a skill the agent in
    /// `session_id` wants to save: how much work it did, and whether the user
    /// asked for a skill. The memory review looks at the chat it reviews.
    fn skill_evidence(&self, session_id: &str) -> crate::skills::Evidence {
        let basis = self
            .rt(session_id)
            .ok()
            .map(|rt| {
                let st = rt.st.lock();
                match (st.info.internal, &st.info.parent_id) {
                    (true, Some(parent)) => parent.clone(),
                    _ => st.info.id.clone(),
                }
            })
            .unwrap_or_else(|| session_id.to_string());
        evidence_from(&self.store.history(&basis).unwrap_or_default())
    }

    /// Skills this session's agent has created since the user's last message
    /// (a memory review: since its own instructions).
    fn skills_made_this_turn(&self, session_id: &str) -> usize {
        let entries = self.store.history(session_id).unwrap_or_default();
        let mut made = std::collections::HashSet::new();
        let mut created = 0;
        for e in entries.iter().rev() {
            match &e.body {
                EntryBody::User { author, .. } if instructs(author.as_deref()) => break,
                EntryBody::ToolResult {
                    call_id,
                    is_error: false,
                    ..
                } => {
                    made.insert(call_id.clone());
                }
                EntryBody::Assistant { content, .. } => {
                    created += content
                        .iter()
                        .filter(|p| {
                            matches!(p, AssistantPart::ToolCall { id, name, arguments }
                                if name == "skill_manage"
                                    && arguments["action"] == "create"
                                    && made.contains(id))
                        })
                        .count();
                }
                _ => {}
            }
        }
        created
    }

    /// Keep volatile facts current without rewriting an already cached prefix.
    /// Shared with Claude Code: the note is stored, so subsequent steps and
    /// resumes see it once, not reinserted into every request.
    fn refresh_environment(&self, rt: &Rt) -> Result<()> {
        let (sid, ledger) = {
            let st = rt.st.lock();
            (st.info.id.clone(), st.info.task.clone())
        };
        if let Some(mut ledger) = ledger
            && crate::tools::meta::maintain_ledger(&mut ledger, &self.store, &sid)?
        {
            {
                let mut st = rt.st.lock();
                let RtState { info, internal, .. } = &mut *st;
                crate::claude_code::reconcile_tasks(
                    info.task.as_ref().map_or(&[], |task| task.plan.as_slice()),
                    &mut internal.claude_tasks,
                    &ledger.plan,
                );
                info.task = Some(ledger);
            }
            self.save(rt);
        }
        let (current, append) = {
            let mut st = rt.st.lock();
            let current = crate::prompt::current_environment(&self.effective_model(&st));
            if st.internal.environment.as_ref() == Some(&current) {
                return Ok(());
            }
            let append = st.internal.environment.is_some()
                || st.internal.system_prompt.is_some()
                || !st.internal.bot_prompts.is_empty()
                || st.internal.claude_code.is_some()
                || !st.internal.claude_code_bots.is_empty();
            if !append {
                // A fresh native/Claude Code context includes these already.
                st.internal.environment = Some(current.clone());
            }
            (current, append)
        };
        if append {
            self.append(rt, EntryBody::User {
                content: vec![ContentPart::text(format!(
                    "[Environment update] Current facts (replace earlier date/model):\n{current}"
                ))],
                author: Some("harness".into()),
            })?;
            rt.st.lock().internal.environment = Some(current);
        }
        Ok(())
    }
    fn prepare_request(&self, rt: &Rt) -> Result<Prepared> {
        self.refresh_environment(rt)?;
        let id = rt.st.lock().info.id.clone();
        let entries = self.store.context_history(&id)?;
        let mut st = rt.st.lock();
        if st.internal.ledger_shown.is_none() {
            compaction::history_edited(&mut st);
        }
        let system = self.system_prompt(&mut st);
        let mut tools = self.tool_specs(&st);
        let view = self.view_for(&entries, &st);
        let model = self.effective_model(&st);
        // No calls allowed: the tools stay listed with `tool_choice: none`
        // where the provider takes it (the prompt cache holds), else they go.
        let no_tool_calls = st.no_tool_calls && self.providers.takes_tool_choice_none(&model);
        if st.no_tool_calls && !no_tool_calls {
            tools.clear();
        }
        let info = self.providers.model_info(&model);
        let estimate = context::estimate_request(&system, &tools, &view.messages);
        let cal = st.internal.calibration.unwrap_or(1.0);
        st.info.context.tokens = (estimate as f64 * cal) as u64;
        st.info.context.source = Some("estimated".into());
        if let Some(i) = &info {
            st.info.context.window = self.working_window(&model, i.context_window);
        }
        let thinking = if is_room(&st.info) {
            self.speaker_bot(&st)
                .and_then(|b| b.thinking)
                .unwrap_or(st.info.thinking)
        } else {
            st.info.thinking
        };
        Ok(Prepared {
            author: self.speaker_id(&st),
            model: model.clone(),
            request: LlmRequest {
                model: model.model.clone(),
                system,
                messages: view.messages,
                tools,
                thinking,
                max_output: info
                    .as_ref()
                    .filter(|i| i.max_output > 0 && self.providers.bounds_output(&model.provider))
                    .map(|i| i.max_output),
                cache_key: id,
                fast: st.info.fast,
                daybreak: st.info.daybreak,
                no_tool_calls,
            },
            estimate,
        })
    }

    /// Stream one model response for `p` and return it with the model that
    /// wrote it. When the session's model keeps failing after retries (rate
    /// limits, server errors, outages, rejected credentials) and a fallback
    /// chain is configured (`[models.fallback]`), the next model takes over
    /// and the user is told; it stays in `sticky` for the rest of the turn.
    async fn call_model(
        &self,
        rt: &Arc<Rt>,
        p: &Prepared,
        cancel: &CancellationToken,
        sticky: &mut Option<ModelRef>,
    ) -> Result<(LlmResponse, ModelRef), crate::llm::LlmError> {
        // A program choice is tied to this account and model. Never turn a
        // Daybreak denial into an automatic request on another service.
        let chain = if p.request.daybreak {
            Vec::new()
        } else {
            self.fallback_candidates(&p.model)
        };
        let (mut target, mut rest) = match sticky
            .as_ref()
            .and_then(|m| chain.iter().position(|c| c == m))
        {
            Some(i) => (chain[i].clone(), chain[i + 1..].to_vec()),
            None => (p.model.clone(), chain),
        };
        let mut req = if target == p.model {
            std::borrow::Cow::Borrowed(&p.request)
        } else {
            std::borrow::Cow::Owned(self.retarget(&p.request, &target))
        };
        // Accounts this turn moved away from (their plan's limit reached).
        let mut tried: Vec<String> = Vec::new();
        loop {
            let more = !rest.is_empty()
                || (!p.request.daybreak && self.next_account(&target, &tried).is_some());
            match self
                .stream_retrying(rt, &target, p.author.clone(), &req, cancel, more)
                .await
            {
                Ok(mut r) => {
                    r.usage.cost_usd = self.price_of(&target).map(|price| price.cost(&r.usage));
                    let (sid, kind, chat) = {
                        let st = rt.st.lock();
                        (
                            st.info.id.clone(),
                            Self::usage_kind(&st.info),
                            st.info.kind == SessionKind::Chat && !st.info.internal,
                        )
                    };
                    self.log_usage(Some(&sid), &target, kind, &r.usage);
                    // A model on the user's own server that runs with a window
                    // too small for Tessera: the chat says so once (the model
                    // list shows it too), with how to raise it. What the server
                    // runs it with is read again beside the turn, and the chat
                    // told then if that is what shows it.
                    if chat {
                        self.tell_window(rt, &sid, &target);
                    }
                    if self.providers.own_server(&target.provider)
                        && let Some(me) = self.me.upgrade()
                    {
                        let (rt, target) = (rt.clone(), target.clone());
                        tokio::spawn(async move {
                            me.providers.recheck_window(&target).await;
                            if chat {
                                let sid = rt.st.lock().info.id.clone();
                                me.tell_window(&rt, &sid, &target);
                            }
                        });
                    }
                    if target != p.model {
                        *sticky = Some(target.clone());
                    }
                    return Ok((r, target));
                }
                // The account's plan is used up: the same model on the user's
                // next account of that kind, for good (`[accounts] continue_on_next`).
                Err(e)
                    if e.kind == ErrorKind::RateLimited
                        && !p.request.daybreak
                        && !cancel.is_cancelled()
                        && self.next_account(&target, &tried).is_some() =>
                {
                    let next = self.next_account(&target, &tried).expect("just found");
                    tried.push(target.provider.clone());
                    tracing::info!("{target}: {e}; moving to {next}");
                    self.move_to_account(rt, &target, &next);
                    req = std::borrow::Cow::Owned(self.retarget(&p.request, &next));
                    target = next;
                }
                Err(e)
                    if !rest.is_empty()
                        && matches!(
                            e.kind,
                            ErrorKind::Transient | ErrorKind::RateLimited | ErrorKind::Auth
                        )
                        && !cancel.is_cancelled() =>
                {
                    let next = rest.remove(0);
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!(
                            "{target} failed ({}); continuing with {next} for the rest of this turn.",
                            one_line(&e.message, 160)
                        ),
                    );
                    req = std::borrow::Cow::Owned(self.retarget(&p.request, &next));
                    target = next;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Models to switch to, in order, when `primary` keeps failing: its
    /// fallback chain, resolved, without duplicates or signed-out providers.
    fn fallback_candidates(&self, primary: &ModelRef) -> Vec<ModelRef> {
        let chain = self.config.read().fallback_chain(&primary.to_string());
        if chain.is_empty() {
            return Vec::new();
        }
        let ready: Vec<String> = self
            .providers
            .infos()
            .into_iter()
            .filter(|p| p.authenticated)
            .map(|p| p.id)
            .collect();
        let mut out: Vec<ModelRef> = Vec::new();
        for entry in chain {
            let m = match entry.trim().parse::<ModelRef>() {
                Ok(m) if m.as_role().is_none() && self.providers.exists(&m.provider) => m,
                _ => match self.resolve_model(&entry) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("fallback model `{entry}` skipped: {e}");
                        continue;
                    }
                },
            };
            if m != *primary && !out.contains(&m) && ready.contains(&m.provider) {
                out.push(m);
            }
        }
        out
    }

    /// `req` for another model: its id and a thinking level it supports,
    /// without replayed reasoning (bound to the model that wrote it).
    fn retarget(&self, req: &LlmRequest, to: &ModelRef) -> LlmRequest {
        let mut r = req.clone();
        r.model = to.model.clone();
        if r.no_tool_calls && !self.providers.takes_tool_choice_none(to) {
            r.tools.clear();
            r.no_tool_calls = false;
        }
        if let Some(info) = self.providers.model_info(to) {
            r.thinking = crate::thinking::clamp(r.thinking, &info.thinking, info.default_thinking);
        }
        for m in &mut r.messages {
            if let Msg::Assistant(parts) = m {
                parts.retain(|p| !matches!(p, AssistantPart::Thinking { .. }));
            }
        }
        r.messages
            .retain(|m| !matches!(m, Msg::Assistant(parts) if parts.is_empty()));
        r
    }

    /// What a response is billed per token (none for subscriptions; see
    /// `engine/usage.rs`).
    fn price_of(&self, m: &ModelRef) -> Option<crate::config::Price> {
        self.billed_price(m)
    }

    /// Stream one response from `model`, mirroring deltas to clients, with
    /// retries for transient failures (fewer when a fallback is waiting, and
    /// none when a rate limit asks for a long pause).
    async fn stream_retrying(
        &self,
        rt: &Arc<Rt>,
        model: &ModelRef,
        author: Option<String>,
        request: &LlmRequest,
        cancel: &CancellationToken,
        fallback_ready: bool,
    ) -> Result<LlmResponse, crate::llm::LlmError> {
        let id = rt.st.lock().info.id.clone();
        // Subscribe before the request: a message can arrive while the model
        // is failing, before its pause starts. IDs reconcile missed events and
        // ensure an already waiting message wakes only one retry.
        let mut retry_events = self.events.subscribe();
        let mut retry_seen: HashSet<String> = user_queue(&rt.st.lock())
            .map(|(_, _, w)| w.id.clone())
            .collect();
        // A server on this network (llama.cpp loading a model, a PC waking
        // up) gets several minutes; hosted APIs about one. Work nobody
        // watches keeps trying much longer (`llm/retry.rs`).
        let own = self.providers.bounds_output(&model.provider);
        let max_attempts = if fallback_ready {
            2
        } else if own {
            10
        } else {
            5
        };
        let mut backoff: Option<retry::Backoff> = None;
        let mut same = retry::SameError::default();
        // When an unattended wait was last reported, and whether the user's
        // devices were told.
        let mut told_at = std::time::Duration::ZERO;
        let mut told_devices = false;
        let mut limit_waits = 0u32;
        // What the model's profile adds (sampling, one call per reply).
        let tuning = self.request_tuning(rt, model);
        loop {
            {
                rt.st.lock().streaming = Some(StreamingMessage {
                    content: vec![],
                    model: model.clone(),
                    author: author.clone(),
                    running_tools: vec![],
                });
            }
            let _ = self.events.send(Event::StreamStarted {
                session_id: id.clone(),
                model: model.clone(),
                author: author.clone(),
            });
            let (tx, rx) = mpsc::unbounded_channel::<LlmEvent>();
            let events = self.events.clone();
            let rt2 = rt.clone();
            let sid = id.clone();
            // A reply that keeps repeating itself is cut here (`loops.rs`);
            // pieces go out batched (`deltas.rs`), each batch watched first.
            let watch = self.config.read().agent.repetition_cut;
            let cut = cancel.child_token();
            let cut2 = cut.clone();
            let repeated_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = repeated_flag.clone();
            let mut watches: HashMap<u32, loops::RepeatWatch> = HashMap::new();
            let forward = tokio::spawn(deltas::forward(rx, move |ev| {
                use std::sync::atomic::Ordering;
                if flag.load(Ordering::Relaxed) {
                    // More of the same, already on its way out.
                    return;
                }
                if watch
                    && let LlmEvent::Text { index, text } | LlmEvent::Thinking { index, text } = &ev
                    && watches.entry(*index).or_default().push(text).is_some()
                {
                    flag.store(true, Ordering::Relaxed);
                    cut2.cancel();
                }
                deltas::send(&rt2, &events, &sid, ev)
            }));
            let mut result = self
                .providers
                .stream_tuned(model, request, &tuning, &tx, &cut)
                .await;
            drop(tx);
            let _ = forward.await;
            let repeated = repeated_flag.load(std::sync::atomic::Ordering::Relaxed);
            if repeated
                && !cancel.is_cancelled()
                && let Ok(r) = &mut result
                && r.stop != StopReason::Error
            {
                // Keep what was still useful; a call cut off mid-way isn't whole.
                loops::trim_repeated(&mut r.content);
                r.stop = StopReason::Stop;
                rt.st.lock().repeated = true;
            }
            deltas::finish(rt, &self.events, &id);
            let _ = self.events.send(Event::StreamEnded {
                session_id: id.clone(),
            });
            let shown = rt.st.lock().streaming.take();
            if result.is_err() {
                // What the user already read stays, marked as cut off (the
                // model is asked again and doesn't see it).
                let said: Vec<AssistantPart> = shown
                    .map(|s| s.content)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(
                        |p| matches!(p, AssistantPart::Text { text } if !text.trim().is_empty()),
                    )
                    .collect();
                if !said.is_empty() {
                    let _ = self.append(
                        rt,
                        EntryBody::Assistant {
                            content: said,
                            model: model.clone(),
                            usage: Usage::default(),
                            stop: StopReason::Aborted,
                            error: Some("Cut off: the connection dropped, so the reply was asked for again.".into()),
                            author: author.clone(),
                        },
                    );
                }
            }
            match result {
                Ok(r) => {
                    // A long wait ended: say so (the notices above stay).
                    if let Some(b) = &backoff
                        && b.mode() == retry::Mode::Unattended
                        && !cancel.is_cancelled()
                    {
                        let waited = b.elapsed(tokio::time::Instant::now());
                        if waited >= std::time::Duration::from_secs(60) {
                            self.notice(
                                rt,
                                NoticeLevel::Info,
                                format!(
                                    "{model} answered again after {}. Carrying on.",
                                    human_wait(waited)
                                ),
                            );
                        }
                    }
                    return Ok(r);
                }
                // A usage limit that resets in a while (a subscription's
                // window): long work waits for it instead of failing, with
                // no other model to switch to. Stop cancels the wait.
                Err(e)
                    if e.kind == ErrorKind::RateLimited
                        && e.plan_reset
                        && !fallback_ready
                        && !cancel.is_cancelled()
                        && limit_waits < 3
                        && e.retry_after.is_some_and(|w| {
                            w > std::time::Duration::from_secs(120) && w <= LIMIT_WAIT_MAX
                        }) =>
                {
                    limit_waits += 1;
                    let wait = e.retry_after.unwrap_or_default();
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!(
                            "The usage limit is reached ({}). Continuing when it resets, in {}; Stop to cancel.",
                            one_line(&e.message, 120),
                            human_wait(wait)
                        ),
                    );
                    if self.config.read().agent.notify_after_minutes > 0.0 {
                        self.notify_long_work(
                            rt,
                            now_ms(),
                            "Waiting for the usage limit",
                            format!("Continuing when it resets, in {}.", human_wait(wait)),
                        );
                    }
                    if self
                        .pause_for_retry(
                            rt,
                            cancel,
                            wait + std::time::Duration::from_secs(5),
                            &mut retry_events,
                            &mut retry_seen,
                        )
                        .await
                    {
                        return Ok(LlmResponse {
                            content: vec![],
                            usage: Usage::default(),
                            stop: StopReason::Aborted,
                            allow_tool_repair: false,
                        });
                    }
                }
                Err(mut e)
                    if matches!(e.kind, ErrorKind::Transient | ErrorKind::RateLimited)
                        && !cancel.is_cancelled()
                        && !(fallback_ready && e.retry_after.is_some_and(|w| w.as_secs() > 20)) =>
                {
                    let b = backoff.get_or_insert_with(|| {
                        retry::Backoff::new(if !fallback_ready && self.unattended(rt) {
                            retry::Mode::Unattended
                        } else {
                            retry::Mode::Watched { max_attempts }
                        })
                    });
                    let now = tokio::time::Instant::now();
                    // The user's own server answering the same error again and
                    // again (a model that fails to load) isn't mended by
                    // waiting; asking it for half an hour would only keep a
                    // single-slot server busy.
                    if b.mode() == retry::Mode::Unattended && same.futile(&e, own) {
                        e.message = format!(
                            "{} (the server gave the same answer {} times in a row)",
                            e.message,
                            same.count()
                        );
                        return Err(e);
                    }
                    let Some(pause) = b.next(now, e.retry_after, rand::random::<f64>()) else {
                        if b.mode() == retry::Mode::Unattended
                            && b.elapsed(now) >= std::time::Duration::from_secs(60)
                        {
                            e.message = format!(
                                "{} (kept trying for {})",
                                e.message,
                                human_wait(b.elapsed(now))
                            );
                        }
                        return Err(e);
                    };
                    let wait = pause.wait;
                    match b.mode() {
                        retry::Mode::Watched { max_attempts } => self.notice(
                            rt,
                            NoticeLevel::Warning,
                            format!(
                                "{e} — retrying in {}s (attempt {}/{max_attempts})",
                                wait.as_secs(),
                                pause.attempt
                            ),
                        ),
                        // One notice when it starts, then one every ten
                        // minutes (not one per try), and once the devices
                        // when it has lasted five.
                        retry::Mode::Unattended => {
                            let left = retry::PATIENCE.saturating_sub(pause.elapsed);
                            if pause.attempt == 1 {
                                self.notice(
                                    rt,
                                    NoticeLevel::Warning,
                                    format!(
                                        "{e} — trying again in {}, and for up to {} if it keeps failing (this runs unattended). Stop to cancel.",
                                        pause_text(wait),
                                        human_wait(retry::PATIENCE)
                                    ),
                                );
                                told_at = pause.elapsed;
                            } else if pause.elapsed >= told_at + std::time::Duration::from_secs(600)
                            {
                                self.notice(
                                    rt,
                                    NoticeLevel::Warning,
                                    format!(
                                        "Still no answer from {model} after {} ({}). Trying again in {}, for up to {} more. Stop to cancel.",
                                        human_wait(pause.elapsed),
                                        one_line(&e.message, 100),
                                        pause_text(wait),
                                        human_wait(left)
                                    ),
                                );
                                told_at = pause.elapsed;
                            }
                            if !told_devices
                                && pause.elapsed >= std::time::Duration::from_secs(300)
                                && self.config.read().agent.notify_after_minutes > 0.0
                            {
                                told_devices = true;
                                self.notify_long_work(
                                    rt,
                                    now_ms(),
                                    "Waiting for the model",
                                    format!(
                                        "{model} hasn't answered for {}. Still trying for up to {} more.",
                                        human_wait(pause.elapsed),
                                        human_wait(left)
                                    ),
                                );
                            }
                        }
                    }
                    if self
                        .pause_for_retry(rt, cancel, wait, &mut retry_events, &mut retry_seen)
                        .await
                    {
                        return Ok(LlmResponse {
                            content: vec![],
                            usage: Usage::default(),
                            stop: StopReason::Aborted,
                            allow_tool_repair: false,
                        });
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Wait `wait` before the next try; true when Stop cancelled the wait. A
    /// message the user queues meanwhile ends it: they are back, and the
    /// model is asked again at once.
    async fn pause_for_retry(
        &self,
        rt: &Rt,
        cancel: &CancellationToken,
        wait: std::time::Duration,
        events: &mut broadcast::Receiver<Event>,
        seen: &mut HashSet<String>,
    ) -> bool {
        let id = rt.st.lock().info.id.clone();
        let sleep = tokio::time::sleep(wait);
        tokio::pin!(sleep);
        loop {
            let new_message = user_queue(&rt.st.lock())
                .fold(false, |new, (_, _, w)| seen.insert(w.id.clone()) || new);
            if new_message {
                return false;
            }
            tokio::select! {
                _ = &mut sleep => return false,
                _ = cancel.cancelled() => return true,
                ev = events.recv() => match ev {
                    Ok(Event::QueueChanged { session_id, .. }) if session_id == id => {}
                    // Other events (and ones missed when the server is busy)
                    // change nothing: the wait goes on.
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        sleep.await;
                        return false;
                    }
                },
            }
        }
    }

    /// Tell the chat that its model runs with a window too small for
    /// Tessera, once (not again after a restart: the notice is in its
    /// history).
    fn tell_window(&self, rt: &Rt, session_id: &str, model: &ModelRef) {
        let Some(text) = self.providers.window_notice(session_id, model) else {
            return;
        };
        let told = self.store.history(session_id).is_ok_and(|h| {
            h.iter()
                .rev()
                .any(|e| matches!(&e.body, EntryBody::Notice { text: t, .. } if *t == text))
        });
        if !told {
            self.notice(rt, NoticeLevel::Warning, text);
        }
    }

    /// Nobody is likely watching this turn (`retry::unattended`): a scheduled
    /// job, a helper, a bot's work, a chat in YOLO mode, or a chat whose user
    /// wrote last long ago. Such work keeps asking a failing model server for
    /// much longer (`llm/retry.rs`). User-driven rooms always stay watched.
    fn unattended(&self, rt: &Rt) -> bool {
        let info = rt.st.lock().info.clone();
        // The user's last own message (the harness's and agents' notes don't
        // count); only a chat that has no other reason needs it.
        let last_user = self.store.history(&info.id).ok().and_then(|h| {
            h.iter().rev().find_map(|e| match &e.body {
                EntryBody::User { author: None, .. } => Some(e.ts),
                _ => None,
            })
        });
        retry::unattended(&info, last_user, now_ms())
    }

    /// Run one reply's calls; `note` (a plan reminder) goes at the end of
    /// the last result.
    async fn run_tools(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        calls: Vec<(String, String, Value)>,
        seq: u64,
        cancel: &CancellationToken,
        guard: &mut Guardrails,
        mut note: Option<String>,
    ) {
        // Only what this session was offered runs (a private chat's model
        // naming `bash`, or `agent` with subagents off, gets "unknown tool").
        let offered: std::collections::HashSet<String> = self
            .callable_tools(&rt.st.lock())
            .into_iter()
            .map(|t| t.name)
            .collect();
        let registry_tools: Vec<_> = {
            let reg = self.registry.read();
            calls
                .iter()
                .map(|(_, name, _)| reg.get(name).filter(|_| offered.contains(name)))
                .collect()
        };
        // Small model mode lets a reply carry several calls; what a small
        // model chains after a change that failed (the test run after an
        // edit that didn't apply, the delete after a failed copy) is held
        // back, and a call repeated in the same reply runs once.
        let guarded = calls.len() > 1 && self.reply_guard(rt);
        let mut failed: Option<String> = None;
        let mut i = 0;
        while i < calls.len() {
            if cancel.is_cancelled() {
                for (id, name, _) in &calls[i..] {
                    let _ = self.append(
                        rt,
                        EntryBody::ToolResult {
                            call_id: id.clone(),
                            tool: name.clone(),
                            content: vec![ContentPart::text(
                                "Not executed: the user stopped the turn.",
                            )],
                            is_error: true,
                            artifact: None,
                            details: None,
                        },
                    );
                }
                return;
            }
            // Group consecutive calls that are safe to run together.
            let mut j = i + 1;
            let concurrent = |k: usize| {
                registry_tools[k]
                    .as_ref()
                    .is_some_and(|t| t.concurrent(&calls[k].2))
            };
            if concurrent(i) {
                while j < calls.len() && concurrent(j) {
                    j += 1;
                }
            }
            let held: Vec<Option<String>> = (i..j)
                .map(|k| {
                    guarded
                        .then(|| {
                            held_back(&calls, k, failed.as_deref(), registry_tools[k].as_deref())
                        })
                        .flatten()
                })
                .collect();
            let futs = (i..j).map(|k| {
                let (id, name, args) = calls[k].clone();
                let tool = registry_tools[k].clone();
                // The user stopped this computer: no tool runs here.
                let blocked = held[k - i]
                    .clone()
                    .or_else(|| guard.blocked(&name, &args))
                    .or_else(|| self.stopped_here());
                async move {
                    let out = match (tool, blocked) {
                        (_, Some(msg)) => ToolOutput::error(msg),
                        (None, _) => match self.turned_on_by_call(rt, &name) {
                            Some(msg) => ToolOutput::error(msg),
                            None => {
                                let names: Vec<String> = self
                                    .callable_tools(&rt.st.lock())
                                    .into_iter()
                                    .map(|t| t.name)
                                    .collect();
                                ToolOutput::error(self.unavailable_tool(
                                    &rt.st.lock(),
                                    &name,
                                    &names,
                                ))
                            }
                        },
                        (Some(tool), None) => {
                            self.execute_tool(rt, tool, &id, args.clone(), seq, cancel)
                                .await
                        }
                    };
                    (id, name, args, out)
                }
            });
            let results = futures::future::join_all(futs).await;
            let last = results.len().saturating_sub(1);
            for (k, (id, name, args, mut out)) in results.into_iter().enumerate() {
                if held[k].is_none() {
                    if guarded
                        && out.is_error
                        && failed.is_none()
                        && registry_tools[i + k].as_ref().is_some_and(|t| {
                            matches!(
                                t.kind(),
                                crate::tools::ToolKind::Write | crate::tools::ToolKind::Exec
                            )
                        })
                    {
                        failed = Some(name.clone());
                    }
                    if let Some(warning) = guard.observe(&name, &args, &out) {
                        out.content.push(ContentPart::text(warning));
                    }
                }
                if j == calls.len()
                    && k == last
                    && let Some(n) = note.take()
                {
                    out.content.push(ContentPart::text(n));
                }
                let artifact = out
                    .details
                    .as_ref()
                    .and_then(|d| d.get("artifact"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let _ = self.append(
                    rt,
                    EntryBody::ToolResult {
                        call_id: id,
                        tool: name,
                        content: out.content,
                        is_error: out.is_error,
                        artifact,
                        details: out.details,
                    },
                );
            }
            i = j;
        }
        let mut st = rt.st.lock();
        if let Some(s) = st.streaming.as_mut() {
            s.running_tools.clear();
        }
    }

    async fn execute_tool(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        tool: Arc<dyn crate::tools::Tool>,
        call_id: &str,
        args: Value,
        seq: u64,
        cancel: &CancellationToken,
    ) -> ToolOutput {
        self.execute_tool_ext(rt, tool, call_id, args, seq, cancel, true, false)
            .await
    }

    /// [`execute_tool`](Self::execute_tool); `announce: false` leaves the
    /// call's card (started, running, finished) to the caller: a Claude Code
    /// turn whose own stream shows it (`engine/claude_code.rs`).
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_ext(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        tool: Arc<dyn crate::tools::Tool>,
        call_id: &str,
        args: Value,
        seq: u64,
        cancel: &CancellationToken,
        announce: bool,
        actor_read_only: bool,
    ) -> ToolOutput {
        let mut provisional = helper_limits::Provisional::new(self, rt, call_id);
        if let Some(raw) = args.get("__invalid_arguments").and_then(Value::as_str)
            && args.get("__cut_off").is_some()
        {
            return ToolOutput::error(format!(
                "Your reply reached the output limit inside this call, so its arguments were cut off and it did not run \
                 ({} characters arrived). Send less in one call: for a long file, `write` the first part, then add the \
                 rest with `edit` (`at` \">$\").",
                raw.chars().count()
            ));
        }
        if let Some(raw) = args.get("__invalid_arguments").and_then(Value::as_str) {
            return ToolOutput::error(format!(
                "The arguments were not valid JSON, so the tool did not run. Received: {}\nSend the call again with a valid JSON object.",
                crate::util::truncate(raw, 500)
            ));
        }
        // Small model mode's flat forms (`edit {path, old, new}`, `task
        // {done}`) in the shapes the tools take; the call stays as sent.
        let budget_args = args.clone();
        let args = self.full_shape(&tool, args);
        if let Some(msg) = self.empty_small_edit(rt, &tool, &args) {
            return ToolOutput::error(msg);
        }
        let (session_id, cwd, mode, depth, approvals_on, admission) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                PathBuf::from(&st.info.cwd),
                self.effective_permission(&st),
                st.depth,
                st.approvals_on.clone(),
                self.action_admission(&st).with_cancel(cancel.clone()),
            )
        };
        if let Err(reason) = self.helper_authorized_cwd(rt) {
            return ToolOutput::error(reason);
        }
        let mode = actor_permission(mode, actor_read_only);
        let host: Arc<dyn ToolHost> = Arc::new(Host(self.clone(), actor_read_only));
        let events = self.events.clone();
        let rt_p = rt.clone();
        let sid = session_id.clone();
        let cid = call_id.to_string();
        let progress: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |chunk: &str| {
            {
                let mut st = rt_p.st.lock();
                if st.streaming.is_none() {
                    st.streaming = Some(StreamingMessage {
                        content: vec![],
                        model: st.info.model.clone(),
                        author: None,
                        running_tools: vec![],
                    });
                }
                if let Some(s) = st.streaming.as_mut()
                    && let Some(t) = s.running_tools.iter_mut().find(|t| t.call_id == cid)
                {
                    t.output.push_str(chunk);
                    if t.output.len() > 64 * 1024 {
                        let cut = crate::util::ceil_char(&t.output, t.output.len() - 48 * 1024);
                        t.output = t.output[cut..].to_string();
                    }
                }
            }
            let _ = events.send(Event::ToolOutput {
                session_id: sid.clone(),
                call_id: cid.clone(),
                chunk: chunk.to_string(),
            });
        });
        let mut ctx = ToolCtx {
            session_id: session_id.clone(),
            call_id: call_id.to_string(),
            seq,
            cwd,
            cancel: cancel.clone(),
            host,
            progress,
            depth,
            cleared: None,
            admission: if actor_read_only {
                crate::tools::admission::ActionAdmission::denied()
            } else {
                admission
            },
        };

        // Policy.
        let assessed = crate::tools::permission_risk(tool.as_ref(), &args, &ctx, mode);
        let ask = match assessed.clone() {
            Risk::Block { reason } => {
                let outcome = ApprovalOutcome::Rule(reason);
                approval::audit(&session_id, tool.name(), &Answer::rule(outcome.clone()));
                return ToolOutput::error(outcome.message(tool.name()));
            }
            Risk::Ask { reason, key } => Some((reason, key)),
            Risk::None => None,
        };
        if let Some((reason, key)) = ask {
            let keys = self.session_keys(&rt.st.lock(), &key);
            let once = crate::tools::files::startup_write(tool.name(), &args, &ctx)
                || (tool.name() == "bash" && ctx.host.unconfined_commands_ask());
            let allowed = !once && Self::session_allowed(&rt.st.lock(), &keys);
            if !allowed {
                let request = ApprovalRequest {
                    id: new_id(),
                    call_id: call_id.to_string(),
                    tool: tool.name().to_string(),
                    // Shown exactly and in full: a `\r`, an escape sequence
                    // or a later line must not hide part of a command from
                    // the person approving.
                    summary: crate::util::visible_controls_lines(&if once {
                        format!("{}\n\n{}", tool.approval_summary(&args), args)
                    } else {
                        tool.approval_summary(&args)
                    }),
                    reason: crate::util::visible_controls(&reason),
                    arguments: args.clone(),
                    once,
                };
                // A helper's parent may answer for it where it could do this
                // itself without asking (checked again when it answers).
                let helper = helper_approvals::is_helper(&rt.st.lock());
                let authority = helper.then(|| helper_approvals::ParentAuthority {
                    unasked: helper_approvals::ParentAuthority::MODES
                        .into_iter()
                        .filter(|m| {
                            matches!(
                                crate::tools::permission_risk(tool.as_ref(), &args, &ctx, *m),
                                Risk::None
                            )
                        })
                        .collect(),
                    keys: keys.clone(),
                });
                match self
                    .ask_approval_as(rt, &approvals_on, request, cancel, authority)
                    .await
                {
                    ApprovalOutcome::Allowed(ApprovalDecision::AllowSession) if !once => {
                        Self::allow_for_session(&mut rt.st.lock(), keys);
                        self.save(rt);
                    }
                    ApprovalOutcome::Allowed(_) => {}
                    outcome => return ToolOutput::error(outcome.message_in(tool.name(), helper)),
                }
            }
        }
        // Permission changes while a card is open cannot authorize the stale
        // action. Its caller can submit a new action under the new policy.
        if !self.turn_running_here(rt) {
            approval::audit(
                &session_id,
                tool.name(),
                &Answer::rule(ApprovalOutcome::Stopped),
            );
            return ToolOutput::error(ApprovalOutcome::Stopped.message(tool.name()));
        }
        if cancel.is_cancelled() {
            return ToolOutput::error(ApprovalOutcome::Dismissed.message(tool.name()));
        }
        if actor_permission(self.effective_permission(&rt.st.lock()), actor_read_only) != mode {
            let outcome = ApprovalOutcome::Rule(
                "The chat's permissions changed; submit new work under the current mode.".into(),
            );
            approval::audit(&session_id, tool.name(), &Answer::rule(outcome.clone()));
            return ToolOutput::error(outcome.message(tool.name()));
        }
        // Nothing across a Stop on this computer: not while stopped, not an
        // approval answered after one, not a turn that began before one.
        if !self.turn_running_here(rt) {
            approval::audit(
                &session_id,
                tool.name(),
                &Answer::rule(ApprovalOutcome::Stopped),
            );
            return ToolOutput::error(STOPPED_SINCE);
        }
        if let Risk::Block { reason } = tool.assess(&args, &ctx) {
            let outcome = ApprovalOutcome::Rule(reason);
            approval::audit(&session_id, tool.name(), &Answer::rule(outcome.clone()));
            return ToolOutput::error(outcome.message(tool.name()));
        }
        if let Risk::Ask { key, .. } = assessed {
            ctx.cleared = Some(key);
        }

        // The last capability check precedes consumption; a stale bridge
        // call denied here must return its hook's provisional allowance.
        {
            let st = rt.st.lock();
            if self.runs_claude_code(&st)
                && !self
                    .claude_capabilities(&st)
                    .iter()
                    .any(|n| n == tool.name())
            {
                return ToolOutput::error("This tool is no longer available to this actor.");
            }
        }
        if cancel.is_cancelled() {
            return ToolOutput::error("Cancelled; not executed.");
        }
        // Permissions are settled. Reserve immediately before dispatch;
        // concurrent calls take their units under the same state lock.
        if let Err(why) = self.reserve_helper_step(
            rt,
            tool.name(),
            &budget_args,
            (!announce).then_some(call_id),
            true,
        ) {
            return ToolOutput::error(why);
        }
        provisional.transfer();
        if announce {
            let _ = self.store.tool_started(&session_id, call_id, tool.name());
            rt.st.lock().info.steps += 1;
            self.save(rt);
            {
                let mut st = rt.st.lock();
                if st.streaming.is_none() {
                    st.streaming = Some(StreamingMessage {
                        content: vec![],
                        model: st.info.model.clone(),
                        author: None,
                        running_tools: vec![],
                    });
                }
                if let Some(s) = st.streaming.as_mut() {
                    s.running_tools.push(RunningTool {
                        call_id: call_id.to_string(),
                        name: tool.name().to_string(),
                        output: String::new(),
                    });
                }
            }
            let _ = self.events.send(Event::ToolStarted {
                session_id: session_id.clone(),
                call_id: call_id.to_string(),
                name: tool.name().to_string(),
            });
        }

        let mut out =
            crate::activity::monitor(crate::activity::tool(tool.name()), tool.run(args, &ctx))
                .await;
        if let Some(details) = out.details.as_mut() {
            self.redact_metadata(details);
        }
        if announce {
            let _ = self.store.tool_finished(&session_id, call_id);
            out.details = self.tool_timing(&session_id, call_id, out.details);
        }
        // What the files it changed hold now (rewind checks against it).
        let _ = self.seal_snapshots(&session_id, seq);
        {
            let mut st = rt.st.lock();
            if let Some(s) = st.streaming.as_mut().filter(|_| announce) {
                s.running_tools.retain(|t| t.call_id != call_id);
            }
            // A loaded skill the injection scan flagged: tell the user.
            if let Some(f) = out
                .details
                .as_ref()
                .and_then(|d| d.get("injection"))
                .and_then(Value::as_str)
            {
                self.report_findings(&mut st, vec![f.to_string()]);
            }
        }
        // Redact secrets and cap anything a tool returned unbounded.
        let secrets = self.redaction_secrets();
        let patterns = self.config.read().security.redact_patterns;
        let max = self.tool_output_limit(&session_id);
        for part in out.content.iter_mut() {
            // A picture no provider takes would break every request after it.
            if let ContentPart::Image { data, .. } = part
                && let Some(why) = crate::tools::images::unfit_base64(data)
            {
                *part = ContentPart::text(format!(
                    "[A picture was left out: {why}. {}]",
                    crate::tools::images::SHRINK_HINT
                ));
            }
            if let ContentPart::Text { text } = part {
                // What the model reads is what a person looking at it sees:
                // invisible characters (tag characters, direction overrides,
                // zero-width runs) can carry instructions nobody notices.
                let (visible, hidden) = crate::scan::strip_hidden(text);
                if hidden > 0 {
                    *text = format!(
                        "{visible}\n[Tessera removed {hidden} hidden character{} from this output.]",
                        if hidden == 1 { "" } else { "s" }
                    );
                }
                let redacted = if patterns {
                    crate::tools::redact(text, &secrets)
                } else {
                    crate::tools::redact_known(text, &secrets)
                };
                if redacted.len() > max * 2 {
                    let (fit, artifact) = crate::tools::fit_output(&redacted, &ctx, tool.name());
                    *text = fit;
                    if let Some(a) = artifact {
                        let mut d = out.details.take().unwrap_or_else(|| json!({}));
                        d["artifact"] = Value::String(a);
                        out.details = Some(d);
                    }
                } else {
                    *text = redacted;
                }
            }
        }
        out
    }

    /// Keep real execution timestamps with the result for every client/reload.
    fn tool_timing(&self, session: &str, call: &str, details: Option<Value>) -> Option<Value> {
        let Some(ms) = self.store.tool_duration(session, call).ok().flatten() else {
            return details;
        };
        let mut d = details.unwrap_or_else(|| json!({}));
        if let Some(o) = d.as_object_mut() {
            o.entry("duration_ms").or_insert(json!(ms));
        }
        Some(d)
    }

    async fn ask_approval(
        &self,
        rt: &Rt,
        shown_on: &str,
        request: ApprovalRequest,
        cancel: &CancellationToken,
    ) -> ApprovalOutcome {
        self.ask_approval_as(rt, shown_on, request, cancel, None)
            .await
    }

    /// [`ask_approval`](Self::ask_approval); a helper's tells the chat that
    /// started it at once, which may answer it with `authority`.
    async fn ask_approval_as(
        &self,
        rt: &Rt,
        shown_on: &str,
        request: ApprovalRequest,
        cancel: &CancellationToken,
        authority: Option<helper_approvals::ParentAuthority>,
    ) -> ApprovalOutcome {
        let (tx, rx) = oneshot::channel();
        let req_id = request.id.clone();
        let tool = request.tool.clone();
        let (sid, mode) = {
            let st = rt.st.lock();
            (st.info.id.clone(), self.effective_permission(&st))
        };
        let deadline = tokio::time::Instant::now() + approval::WAIT;
        self.approvals.lock().insert(
            req_id.clone(),
            Pending {
                shown_on: shown_on.to_string(),
                acting_on: sid.clone(),
                request: request.clone(),
                deadline,
                tx,
                authority: authority.clone(),
            },
        );
        self.set_status(rt, SessionStatus::AwaitingApproval);
        let mut shown = request;
        self.redact_metadata(&mut shown.arguments);
        let secrets = self.redaction_secrets();
        shown.summary = crate::tools::redact_known(&shown.summary, &secrets);
        shown.reason = crate::tools::redact_known(&shown.reason, &secrets);
        self.tell_parent_of_approval(rt, &shown, authority.as_ref());
        let summary = shown.summary.clone();
        let _ = self.events.send(Event::ApprovalRequested {
            session_id: shown_on.to_string(),
            request: shown,
        });
        let mut answer = self
            .wait_for_user(
                shown_on,
                "Needs your approval",
                &summary,
                approval::wait_answer(rx, cancel, deadline),
            )
            .await;
        if !self.turn_running_here(rt) && answer.outcome != ApprovalOutcome::Stopped {
            answer = Answer::rule(ApprovalOutcome::Stopped);
        } else if answer.outcome != ApprovalOutcome::Stopped
            && self.effective_permission(&rt.st.lock()) != mode
        {
            answer = Answer::rule(ApprovalOutcome::Rule("The chat's permissions changed while it waited; submit new work under the current mode.".into()));
            self.notice(
                rt,
                NoticeLevel::Info,
                "Not done: the chat's permissions changed while it waited for your answer.",
            );
        }
        self.approvals.lock().remove(&req_id);
        approval::audit(&sid, &tool, &answer);
        self.set_status(rt, SessionStatus::Running);
        let _ = self.events.send(Event::ApprovalResolved {
            session_id: shown_on.to_string(),
            request_id: req_id,
            decision: answer.outcome.decision(),
        });
        answer.outcome
    }

    // ---- compaction: see engine/compaction.rs -----------------------------------

    /// A tool-less completion for a session on its utility model (a private
    /// chat's only on its own model), counted in the session's usage; the
    /// utility model's fallback chain applies (not in private chats).
    async fn utility_complete(
        &self,
        session_id: &str,
        system: &str,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let rt = self.rt(session_id).map_err(|e| e.message)?;
        let (model, private) = self.utility_model_of(&rt);
        let mut chain = if private {
            Vec::new()
        } else {
            self.fallback_candidates(&model)
        };
        chain.insert(0, model);
        let mut last = String::new();
        for m in chain {
            // This is a tool's synchronous helper: waiting for its own chat
            // to become idle would deadlock. Let web_fetch return the page
            // instead, or try a configured fallback on another server.
            if self.shares_server(&rt, &m) && rt.st.lock().running {
                last = "Extraction skipped to keep this chat's local model cache; use a separate utility provider for extracts".into();
                continue;
            }
            match self
                .complete_text_usage(&m, system, prompt, cancel, (session_id, "other"))
                .await
            {
                Ok((text, mut usage)) => {
                    usage.cost_usd = self.price_of(&m).map(|p| p.cost(&usage));
                    let mut st = rt.st.lock();
                    let u = &mut st.info.usage;
                    u.input += usage.input;
                    u.output += usage.output;
                    u.cache_read += usage.cache_read;
                    u.cache_write += usage.cache_write;
                    if let Some(c) = usage.cost_usd {
                        *u.cost_usd.get_or_insert(0.0) += c;
                    }
                    return Ok(text);
                }
                Err(e) if cancel.is_cancelled() => return Err(e.to_string()),
                Err(e) => last = format!("{m}: {e}"),
            }
        }
        Err(last)
    }

    /// The model of a session's utility requests, and whether the chat is
    /// private (then only its own model is used).
    fn utility_model_of(&self, rt: &Rt) -> (ModelRef, bool) {
        let (own, private) = {
            let st = rt.st.lock();
            (self.effective_model(&st), st.info.private)
        };
        let model = if private {
            self.utility_for(own)
        } else {
            self.config
                .read()
                .utility_model()
                .filter(|m| self.providers.model_info(m).is_some())
                .unwrap_or_else(|| self.utility_for(own))
        };
        (model, private)
    }

    /// Whether a session's utility requests may run side by side: not on a
    /// server at home or on the local network (often one request at a
    /// time), nor through Claude Code (a process each).
    fn utility_parallel(&self, session_id: &str) -> bool {
        let Ok(rt) = self.rt(session_id) else {
            return false;
        };
        let (model, _) = self.utility_model_of(&rt);
        match self.providers.kind_of(&model.provider) {
            None | Some(ProviderKind::ClaudeCode) => false,
            Some(ProviderKind::OpenaiChat) => !self.providers.on_local_network(&model.provider),
            Some(_) => true,
        }
    }

    /// Whether work sent to `model` competes with `chat`'s own turns: both on
    /// one server that answers one request at a time (see
    /// [`Providers::one_at_a_time`]).
    fn shares_server(&self, chat: &Rt, model: &ModelRef) -> bool {
        let own = self.effective_model(&chat.st.lock());
        own.provider == model.provider && self.providers.one_at_a_time(model)
    }

    /// Tessera's own work for `chat` on `model` (its title, the memory
    /// review's steps) that [shares its server](Self::shares_server) waits
    /// until the chat's turn is over: sent alongside the turn it would hold
    /// the turn up and push the chat's cached prompt out, so the next step
    /// would read everything again. Elsewhere it goes at once.
    async fn when_idle(&self, chat: &Rt, model: &ModelRef) {
        if !self.shares_server(chat, model) {
            return;
        }
        loop {
            let ended = self.turn_ended.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            if !chat.st.lock().running {
                return;
            }
            // Looked at again now and then as well: not every way a session
            // stops running ends a run.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(30), ended).await;
        }
    }

    /// The chat a helper session works for (the memory review's), whose
    /// turns its steps give way to (see [`when_idle`](Self::when_idle)).
    fn helped_chat(&self, rt: &Rt) -> Option<Arc<Rt>> {
        let parent = {
            let st = rt.st.lock();
            st.info
                .internal
                .then(|| st.info.parent_id.clone())
                .flatten()
        }?;
        self.sessions.lock().get(&parent).cloned()
    }

    /// [`complete_text`](Self::complete_text) that also returns the usage.
    async fn complete_text_usage(
        &self,
        model: &ModelRef,
        system: &str,
        prompt: &str,
        cancel: &CancellationToken,
        tag: (&str, &str),
    ) -> Result<(String, Usage)> {
        if self.providers.kind_of(&model.provider) == Some(ProviderKind::ClaudeCode) {
            return self
                .claude_complete(model, system, prompt, cancel, tag)
                .await;
        }
        let req = LlmRequest {
            model: model.model.clone(),
            system: system.to_string(),
            messages: vec![Msg::User(vec![ContentPart::text(prompt)])],
            tools: vec![],
            // Reading a page for an answer needs no deliberation.
            thinking: self.quickest_thinking(model),
            max_output: None,
            // One per kind of request: they share their instructions.
            cache_key: format!("util-{}", tag.1),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut attempt = 0;
        loop {
            match self.providers.stream(model, &req, &tx, cancel).await {
                Ok(r) if r.stop == StopReason::Aborted => return Err(anyhow!("cancelled")),
                Ok(r) => {
                    let mut logged = r.usage.clone();
                    logged.cost_usd = self.price_of(model).map(|p| p.cost(&logged));
                    self.log_usage(Some(tag.0), model, tag.1, &logged);
                    let text = r
                        .content
                        .iter()
                        .filter_map(|p| match p {
                            AssistantPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    return Ok((text, r.usage));
                }
                Err(e)
                    if matches!(e.kind, ErrorKind::Transient | ErrorKind::RateLimited)
                        && attempt < 2
                        && !cancel.is_cancelled() =>
                {
                    attempt += 1;
                    let wait = e
                        .retry_after
                        .unwrap_or(std::time::Duration::from_secs(2 * attempt))
                        .min(std::time::Duration::from_secs(20));
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                    }
                }
                Err(e) => return Err(anyhow!(e)),
            }
        }
    }

    /// The least reasoning `model` offers (off where it can), for a quick
    /// one-liner such as a title.
    fn quickest_thinking(&self, model: &ModelRef) -> ThinkingLevel {
        self.providers
            .model_info(model)
            .and_then(|i| {
                i.thinking
                    .iter()
                    .map(|o| o.level)
                    .filter(|l| *l != ThinkingLevel::Auto)
                    .min()
            })
            .unwrap_or(ThinkingLevel::Low)
    }

    /// One-shot text completion (no tools) used for summaries and titles.
    /// `tag`: the session it is for and its usage kind (`title`…).
    /// `fast`: priority processing where the model offers it (someone waits
    /// on the answer). A stopped request is an error, never its partial text.
    #[allow(clippy::too_many_arguments)]
    async fn complete_text(
        &self,
        model: &ModelRef,
        system: &str,
        prompt: &str,
        cancel: &CancellationToken,
        tag: (&str, &str),
        fast: bool,
        thinking: ThinkingLevel,
    ) -> Result<String> {
        if self.providers.kind_of(&model.provider) == Some(ProviderKind::ClaudeCode) {
            return self
                .claude_complete(model, system, prompt, cancel, tag)
                .await
                .map(|(text, _)| text);
        }
        let req = LlmRequest {
            model: model.model.clone(),
            system: system.to_string(),
            messages: vec![Msg::User(vec![ContentPart::text(prompt)])],
            tools: vec![],
            thinking,
            max_output: None,
            cache_key: format!("util-{}", tag.1),
            fast,
            daybreak: false,
            no_tool_calls: false,
        };
        self.complete_request(model, &req, cancel, tag).await
    }

    /// [`complete_text`](Self::complete_text) for a request built by the
    /// caller: its text answer, with retries for passing failures.
    async fn complete_request(
        &self,
        model: &ModelRef,
        req: &LlmRequest,
        cancel: &CancellationToken,
        tag: (&str, &str),
    ) -> Result<String> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut attempt = 0;
        loop {
            match self.providers.stream(model, req, &tx, cancel).await {
                Ok(r) if r.stop == StopReason::Aborted || cancel.is_cancelled() => {
                    return Err(anyhow!("cancelled"));
                }
                Ok(r) => {
                    let mut logged = r.usage.clone();
                    logged.cost_usd = self.price_of(model).map(|p| p.cost(&logged));
                    self.log_usage(Some(tag.0), model, tag.1, &logged);
                    let text = if tag.1 == "title" {
                        title::final_answer(&r.content)
                    } else {
                        r.content
                            .iter()
                            .filter_map(|p| match p {
                                AssistantPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    };
                    return Ok(text);
                }
                Err(e)
                    if matches!(e.kind, ErrorKind::Transient | ErrorKind::RateLimited)
                        && attempt < 3
                        && !cancel.is_cancelled() =>
                {
                    attempt += 1;
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)) => {}
                        _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                    }
                }
                Err(e) => return Err(anyhow!(e)),
            }
        }
    }

    /// Turn attached files into something the agent can use: images stay
    /// images, other files are saved where tools can read them (text files
    /// are inlined too). Private chats keep everything in memory: text and
    /// images only.
    fn attach(
        &self,
        session_id: &str,
        content: &mut Vec<ContentPart>,
        files: Vec<tessera_proto::Attachment>,
    ) -> Result<(), ApiError> {
        use base64::Engine as _;
        const MAX_FILE: usize = tessera_proto::MAX_FILE_ATTACHMENT_BYTES;
        const MAX_INLINE: usize = 64 << 10;
        if files.len() > 10 {
            return Err(bad("attach at most 10 files at a time"));
        }
        let private = self.rt(session_id)?.st.lock().info.private;
        let dir = self.paths.uploads_dir().join(session_id);
        let mut total = 0usize;
        for f in files {
            let name: String = f
                .name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect();
            let name = if name.trim().is_empty() || name == "." || name == ".." {
                "file".to_string()
            } else {
                name.trim().to_string()
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(f.data.trim())
                .map_err(|_| bad(format!("{name}: not valid base64")))?;
            total += bytes.len();
            if bytes.len() > MAX_FILE || total > tessera_proto::MAX_MESSAGE_ATTACHMENT_BYTES {
                return Err(bad(format!(
                    "{name} is too large (25 MB per file, 50 MB per message)"
                )));
            }
            let mime = f
                .mime
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let image = matches!(
                mime.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            );
            // A picture no provider takes stays a file (it would break
            // every request of the chat after it).
            let unfit = image.then(|| crate::tools::images::unfit(&bytes)).flatten();
            if let (Some(why), true) = (&unfit, private) {
                return Err(bad(format!("{name} can't be shown to the model: {why}")));
            }
            if image && unfit.is_none() {
                content.push(ContentPart::Image { mime, data: f.data });
                if private {
                    continue;
                }
            }
            let text = std::str::from_utf8(&bytes)
                .ok()
                .filter(|t| !t.contains('\0'));
            let size = crate::util::human_bytes(bytes.len() as u64);
            if private {
                let Some(t) = text.filter(|t| t.len() <= 4 * MAX_INLINE) else {
                    return Err(bad(format!(
                        "{name}: private chats can take text files and images only, because nothing from them is written to disk"
                    )));
                };
                content.push(ContentPart::text(format!(
                    "[Attached file: {name} · {size}]\n```\n{t}\n```"
                )));
                continue;
            }
            std::fs::create_dir_all(&dir).map_err(internal)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            let mut path = dir.join(&name);
            let mut n = 1;
            while path.exists() {
                let (stem, ext) = name
                    .rsplit_once('.')
                    .map(|(s, e)| (s.to_string(), format!(".{e}")))
                    .unwrap_or((name.clone(), String::new()));
                path = dir.join(format!("{stem} ({n}){ext}"));
                n += 1;
            }
            crate::config::write_atomic(&path, &bytes, true).map_err(internal)?;
            // Downloadable again from the chat, on any device.
            self.offer_file(&path, Some(session_id), None, "upload");
            if image {
                // Also on disk, for models without vision and for tools.
                let mut note = format!(
                    "[Attached image: {name} · {size} · saved at {}]",
                    path.display()
                );
                if let Some(why) = unfit {
                    note.push_str(&format!(
                        "\n(Not shown to the model: {why}. {})",
                        crate::tools::images::SHRINK_HINT
                    ));
                }
                content.push(ContentPart::text(note));
                continue;
            }
            let mut note = format!(
                "[Attached file: {name} · {size} · saved at {}]",
                path.display()
            );
            match text {
                Some(t) if t.len() <= MAX_INLINE => note.push_str(&format!("\n```\n{t}\n```")),
                Some(_) => note.push_str("\n(Large text file: read it with the `read` tool.)"),
                None => note.push_str(
                    "\n(Binary file: use tools that understand it, e.g. `pdftotext` for PDFs.)",
                ),
            }
            content.push(ContentPart::text(note));
        }
        Ok(())
    }

    /// Set (or clear) the session goal: kept verbatim in the task ledger
    /// through every compaction, and told to the agent right away.
    fn set_goal(&self, id: &str, goal: &str) -> Result<SessionInfo, ApiError> {
        let rt = self.rt(id)?;
        let goal = goal.trim();
        if goal.len() > 4000 {
            return Err(bad("keep the goal under 4000 characters"));
        }
        let (had, running) = {
            let mut st = rt.st.lock();
            let task = st.info.task.get_or_insert_with(TaskLedger::default);
            let had = !task.goal.is_empty();
            task.goal = goal.to_string();
            (had, st.running)
        };
        let note = if !goal.is_empty() {
            Some(format!("{GOAL_SET}\n{goal}"))
        } else if had {
            Some(GOAL_CLEARED.to_string())
        } else {
            None
        };
        if let Some(note) = note {
            let content = vec![ContentPart::text(note)];
            if running {
                let mut st = rt.st.lock();
                let stops = st.turn_stops.clone();
                st.steer.push(Waiting {
                    id: new_id(),
                    content,
                    author: Some("goal".into()),
                    stops,
                });
            } else {
                self.append(
                    &rt,
                    EntryBody::User {
                        content,
                        author: Some("goal".into()),
                    },
                )
                .map_err(internal)?;
            }
        }
        self.save(&rt);
        Ok(rt.st.lock().info.clone())
    }

    /// Title a private chat from its first message, without asking any model.
    fn local_title(&self, rt: &Arc<Rt>) {
        let id = rt.st.lock().info.id.clone();
        let first = self.store.history(&id).ok().and_then(|es| {
            es.iter().find_map(|e| match &e.body {
                EntryBody::User { content, .. } => user_preview(content),
                _ => None,
            })
        });
        if let Some(first) = first {
            let mut st = rt.st.lock();
            if !st.internal.titled {
                st.info.title = one_line(&first, 48);
                st.internal.titled = true;
            }
        }
        self.save(rt);
    }

    async fn generate_title(self: Arc<Self>, rt: Arc<Rt>) {
        let (id, model) = {
            let st = rt.st.lock();
            (st.info.id.clone(), st.info.model.clone())
        };
        let Ok(entries) = self.store.history(&id) else {
            return;
        };
        let first_user = entries.iter().find_map(|e| match &e.body {
            EntryBody::User { content, .. } => user_preview(content),
            _ => None,
        });
        let Some(first) = first_user else { return };
        // Immediate fallback title, refined by the model when possible.
        {
            let mut st = rt.st.lock();
            if st.internal.titled {
                return;
            }
            st.info.title = title::fallback(&first);
            st.internal.titled = true;
        }
        let fallback = rt.st.lock().info.title.clone();
        self.save(&rt);
        drop(entries);
        let model = self
            .config
            .read()
            .utility_model()
            .filter(|m| self.providers.model_info(m).is_some())
            .unwrap_or_else(|| self.utility_for(model));
        // On the chat's own one-request-at-a-time server, after the turn.
        self.when_idle(&rt, &model).await;
        if !self.sessions.lock().contains_key(&id) {
            return;
        }
        let prompt = format!(
            "Name this conversation in 3-7 words. Return only the title, no explanation:\n\n{}",
            crate::util::truncate(&first, 1000)
        );
        let cancel = CancellationToken::new();
        let sid = rt.st.lock().info.id.clone();
        let fut = self.complete_text(
            &model,
            "You name conversations. Reply with the title only.",
            &prompt,
            &cancel,
            (&sid, "title"),
            false,
            self.quickest_thinking(&model),
        );
        if let Ok(Ok(answer)) = tokio::time::timeout(std::time::Duration::from_secs(60), fut).await
            && let Some(t) = title::clean(&answer)
        {
            // Not over a name the user gave it meanwhile.
            let mut st = rt.st.lock();
            if st.info.title == fallback {
                st.info.title = t;
                drop(st);
                self.save(&rt);
            }
        }
    }

    // ---- providers & auth ---------------------------------------------------------

    fn persist_providers(&self) -> Result<()> {
        let mut c = self.config.write();
        c.providers = self.providers.persisted_configs();
        c.save(&self.paths)
    }

    async fn save_provider(&self, spec: ProviderSpec) -> Result<ProviderInfo, ApiError> {
        let id = spec.id.trim().to_ascii_lowercase().replace(' ', "-");
        if id.is_empty() || id.contains('/') {
            return Err(bad("provider id must be non-empty and contain no '/'"));
        }
        // Claude Code's is the `claude` command (empty: find it).
        if spec.base_url.trim().is_empty()
            && !matches!(
                spec.kind,
                ProviderKind::OpenaiCodex | ProviderKind::ClaudeCode
            )
        {
            return Err(bad("base URL is required"));
        }
        let claude = spec.kind == ProviderKind::ClaudeCode;
        if claude
            && spec
                .api_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty())
        {
            return Err(bad(
                "Tessera never takes Claude credentials: Claude Code signs in by itself (run `claude` once on the server)",
            ));
        }
        // Claude Code is listed only where it is installed: saving it anywhere
        // else would keep a provider nobody can see.
        if claude && crate::claude_code::locate(&spec.base_url).is_none() {
            return Err(bad(
                "Claude Code isn't installed on this server. Install it, run `claude` there once to sign in, then add it here.",
            ));
        }
        let cfg = ProviderConfig {
            id: id.clone(),
            name: if spec.name.trim().is_empty() {
                id.clone()
            } else {
                spec.name.trim().to_string()
            },
            kind: spec.kind,
            base_url: spec.base_url.trim().trim_end_matches('/').to_string(),
            discover: spec.discover,
            models: if claude && spec.models.is_empty() {
                // Its own list arrives with its first turn.
                crate::claude_code::preset().models
            } else {
                spec.models
                    .iter()
                    .map(|m| ModelConfig::from_info(m, spec.kind))
                    .collect()
            },
            headers: Default::default(),
            reasoning_replay: Default::default(),
            // Set in config.toml only: kept when the app saves the provider.
            parallel: self
                .providers
                .configs()
                .into_iter()
                .find(|c| c.id == id)
                .and_then(|c| c.parallel),
        };
        let account = self.providers.claude_account(&cfg);
        if let Some(key) = spec.api_key.filter(|k| !k.trim().is_empty()) {
            self.auth
                .set(
                    &id,
                    Credential::ApiKey {
                        key: key.trim().to_string(),
                    },
                )
                .map_err(internal)?;
        }
        self.providers.upsert(cfg);
        self.persist_providers().map_err(internal)?;
        if claude {
            // Listed once found; Tessera's tools and context come with every
            // turn of it, nothing more to set up.
            self.claude_code_detect(&account).await;
        }
        let errors = self.providers.discover(Some(&id)).await;
        if let Some((_, e)) = errors.first() {
            let _ = self.events.send(Event::Notice {
                session_id: None,
                level: NoticeLevel::Warning,
                message: format!("Saved `{id}`, but listing its models failed: {e}"),
            });
        }
        if self.config.read().default_model.is_none()
            && let Some(m) = self
                .providers
                .infos()
                .into_iter()
                .find(|p| p.id == id)
                .and_then(|p| p.models.first().cloned())
        {
            let mut c = self.config.write();
            c.default_model = Some(format!("{}/{}", id, m.id));
            let _ = c.save(&self.paths);
        }
        let _ = self.events.send(Event::ProvidersChanged);
        self.providers
            .infos()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| internal("provider vanished"))
    }

    async fn start_oauth(self: &Arc<Self>, provider: &str) -> Result<OAuthStart, ApiError> {
        let kind = self.providers.kind_of(provider);
        if provider == crate::claude_code::PROVIDER || kind == Some(ProviderKind::ClaudeCode) {
            // Signing in is Claude Code's own business, never Tessera's.
            return Err(bad(self
                .claude_code_status(true, Some(provider))
                .await?
                .hint));
        }
        // ChatGPT sign-in, for the usual account or another one of the user's.
        if !(provider == CODEX_PROVIDER || kind == Some(ProviderKind::OpenaiCodex)) {
            return Err(bad(format!("`{provider}` does not use browser sign-in")));
        }
        let provider = provider.to_string();
        let http = self.providers.http().clone();
        let dc = crate::auth::codex_device_start(&http)
            .await
            .map_err(|e| ApiError::new(ErrorCode::Provider, e.to_string()))?;
        let me = self.clone();
        let dc2 = dc.clone();
        tokio::spawn(async move {
            match crate::auth::codex_device_finish(&http, &dc2).await {
                Ok(t) => {
                    let cred = Credential::Oauth {
                        access: t.access,
                        refresh: t.refresh.unwrap_or_default(),
                        expires: t.expires,
                        account_id: t.account_id,
                        email: t.email.clone(),
                    };
                    // The same ChatGPT account signed in under another name shares its limits.
                    let twin = me.same_chatgpt_account(&provider, &cred);
                    if let Err(e) = me.auth.set(&provider, cred) {
                        tracing::error!("saving ChatGPT credentials: {e}");
                        return;
                    }
                    me.providers.discover(Some(&provider)).await;
                    if me.config.read().default_model.is_none()
                        && let Some(m) = me
                            .providers
                            .infos()
                            .into_iter()
                            .find(|p| p.id == provider)
                            .and_then(|p| p.models.first().cloned())
                    {
                        let mut c = me.config.write();
                        c.default_model = Some(format!("{provider}/{}", m.id));
                        let _ = c.save(&me.paths);
                    }
                    let _ = me.events.send(Event::ProvidersChanged);
                    let who = t.email.map(|e| format!(" as {e}")).unwrap_or_default();
                    let _ = me.events.send(Event::Notice {
                        session_id: None,
                        level: NoticeLevel::Info,
                        message: format!("Signed in to ChatGPT{who}."),
                    });
                    if let Some(name) = twin {
                        let _ = me.events.send(Event::Notice {
                            session_id: None,
                            level: NoticeLevel::Warning,
                            message: format!(
                                "That is the same ChatGPT account as “{name}”: they share one plan and its limits. Sign in with another ChatGPT account for more."
                            ),
                        });
                    }
                }
                Err(e) => {
                    let _ = me.events.send(Event::Notice {
                        session_id: None,
                        level: NoticeLevel::Error,
                        message: format!("ChatGPT sign-in failed: {e}"),
                    });
                }
            }
        });
        Ok(OAuthStart {
            url: crate::auth::CODEX_DEVICE_URL.into(),
            instructions: format!(
                "Open {} on any device, sign in to ChatGPT, and enter the code {}. This finishes automatically.",
                crate::auth::CODEX_DEVICE_URL,
                dc.user_code
            ),
            auto_complete: true,
            user_code: Some(dc.user_code),
        })
    }

    /// Find model servers so the harness works immediately: this machine's
    /// usual ports (on the first run too), and with `lan` the home network
    /// (see `llm::local`). Only model lists are asked for; nothing loads a
    /// model. Returns the providers added or updated.
    async fn autodetect_local(&self, lan: bool) -> Vec<String> {
        let candidates = [
            ("llama-cpp", "llama.cpp", "http://127.0.0.1:1234/v1"),
            ("llama-cpp", "llama.cpp", "http://127.0.0.1:8080/v1"),
            ("ollama", "Ollama", "http://127.0.0.1:11434/v1"),
            ("lmstudio", "LM Studio", "http://127.0.0.1:1235/v1"),
            ("vllm", "vLLM", "http://127.0.0.1:8000/v1"),
        ];
        let http = self.providers.http().clone();
        let mut added = Vec::new();
        for (id, name, url) in candidates {
            if added.iter().any(|(i, _): &(String, String)| i == id) {
                continue;
            }
            let Ok(models) = crate::llm::openai_chat::list_models(&http, url, &[]).await else {
                continue;
            };
            if models.is_empty() {
                continue;
            }
            let (label, name) = if id == "llama-cpp" && url.contains(":1234") {
                ("local", "Local (llama.cpp)")
            } else {
                (id, name)
            };
            self.providers.upsert(ProviderConfig {
                id: label.into(),
                name: name.into(),
                kind: ProviderKind::OpenaiChat,
                base_url: url.into(),
                discover: true,
                models: vec![],
                headers: Default::default(),
                reasoning_replay: Default::default(),
                parallel: None,
            });
            added.push((
                label.to_string(),
                models.iter().find(|m| m.2).unwrap_or(&models[0]).0.clone(),
            ));
        }
        // Only a server on this machine becomes the default by itself: one
        // elsewhere on the network may be someone else's, so it waits until
        // the user picks it.
        let here = added.len();
        if lan {
            let known: Vec<String> = self
                .providers
                .infos()
                .into_iter()
                .map(|p| p.base_url.trim_end_matches('/').to_string())
                .collect();
            for f in crate::llm::local::find_on_lan(&http).await {
                if known.contains(&f.base_url) {
                    continue;
                }
                let id = crate::llm::local::lan_provider_id(&f, |id| {
                    self.providers.exists(id) || added.iter().any(|(a, _)| a == id)
                });
                let host = f
                    .base_url
                    .trim_start_matches("http://")
                    .trim_end_matches("/v1")
                    .to_string();
                self.providers.upsert(ProviderConfig {
                    id: id.clone(),
                    name: format!("{} on {host}", f.server),
                    kind: ProviderKind::OpenaiChat,
                    base_url: f.base_url.clone(),
                    discover: true,
                    models: vec![],
                    headers: Default::default(),
                    reasoning_replay: Default::default(),
                    parallel: None,
                });
                added.push((id, f.models[0].clone()));
            }
        }
        let _ = self.store.kv_set("autodetected", "1");
        if added.is_empty() {
            return Vec::new();
        }
        let _ = self.persist_providers();
        if self.config.read().default_model.is_none()
            && let Some((p, m)) = added[..here].first()
        {
            let mut c = self.config.write();
            c.default_model = Some(format!("{p}/{m}"));
            let _ = c.save(&self.paths);
        }
        tracing::info!("detected local model servers: {:?}", added);
        added.into_iter().map(|(id, _)| id).collect()
    }

    // ---- jobs -------------------------------------------------------------------

    fn job_gate(&self, id: &str) -> Arc<Mutex<()>> {
        let mut gates = self.job_admissions.lock();
        gates.retain(|_, g| g.strong_count() > 0);
        if let Some(g) = gates.get(id).and_then(std::sync::Weak::upgrade) {
            return g;
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(id.to_string(), Arc::downgrade(&gate));
        gate
    }

    fn save_job(&self, mut job: Job) -> Result<Job> {
        if job.id.is_empty() {
            job.id = format!("job_{}", &new_id()[12..]);
        }
        let gate = self.job_gate(&job.id);
        let _admission = gate.lock();
        job.schedule = job.schedule.trim().to_string();
        crate::jobs::validate(&job.schedule)?;
        // The runs, and the limits an older app doesn't know about (it
        // leaves them out), are the server's: an omitted limit stays, 0
        // clears it.
        let before = (!job.id.is_empty())
            .then(|| self.store.jobs().ok()?.into_iter().find(|j| j.id == job.id))
            .flatten();
        if let Some(old) = &before {
            job.runs = old.runs;
            job.last_run = old.last_run;
            job.last_session = old.last_session.clone();
            job.last_status = old.last_status.clone();
            job.last_end = old.last_end;
            job.until = job.until.or(old.until);
            job.max_runs = job.max_runs.or(old.max_runs);
            job.ended = if job.enabled { None } else { old.ended.clone() };
        } else {
            job.runs = 0;
            job.ended = None;
        }
        job.until = job.until.filter(|u| *u > 0);
        job.max_runs = job.max_runs.filter(|m| *m > 0);
        if job.enabled
            && let Some(why) = crate::jobs::schedule_over(&job, now_ms())
        {
            return Err(anyhow!(
                "its schedule is over ({why}): raise or clear `max_runs`, or set a later `until`, to turn it on"
            ));
        }
        let next = crate::jobs::next_run_within(&job, now_ms());
        if job.enabled && next.is_none() {
            return Err(anyhow!(
                "no run would come before its end date (`until`): set a later one"
            ));
        }
        if job.prompt.trim().is_empty() {
            return Err(anyhow!("prompt is empty"));
        }
        if let Some(role) = job.model.as_ref().and_then(|m| m.as_role()) {
            self.config.read().role(role).map_err(|e| anyhow!(e))?;
        }
        if job.id.is_empty() {
            job.id = format!("job_{}", &new_id()[12..]);
        }
        if job.cwd.trim().is_empty() {
            job.cwd = self.default_cwd().display().to_string();
        }
        if !crate::util::expand_home(&job.cwd).is_dir() {
            return Err(anyhow!("{} is not a directory", job.cwd));
        }
        job.next_run = next.filter(|_| job.enabled);
        self.store.put_job(&job)?;
        self.sched.notify_one();
        let _ = self.events.send(Event::JobUpdated { job: job.clone() });
        Ok(job)
    }

    fn defer_job(&self, id: &str, now: u64) {
        let gate = self.job_gate(id);
        let _admission = gate.lock();
        if let Ok(jobs) = self.store.jobs()
            && let Some(mut j) = jobs.into_iter().find(|j| j.id == id)
            && j.enabled
            && !self.job_is_working(&j)
        {
            j.next_run = crate::jobs::next_run_within(&j, now);
            let _ = self.store.put_job(&j);
        }
    }

    fn run_job(self: Arc<Self>, job: Job) -> Result<SessionInfo> {
        self.admit_job(&job.id, false)
    }

    fn admit_job(self: Arc<Self>, id: &str, scheduled: bool) -> Result<SessionInfo> {
        let gate = self.job_gate(id);
        let admission = gate.lock();
        let job = self
            .store
            .jobs()?
            .into_iter()
            .find(|j| j.id == id)
            .ok_or_else(|| anyhow!("no such job"))?;
        let now = now_ms();
        if scheduled && (!job.enabled || job.next_run.is_none_or(|t| t > now)) {
            anyhow::bail!("the schedule is no longer due");
        }
        if crate::jobs::schedule_over(&job, now).is_some() || (!job.enabled && job.ended.is_some())
        {
            anyhow::bail!(
                "this task's schedule has ended; raise or clear its limits before another run"
            );
        }
        if self.job_is_working(&job) {
            return Err(anyhow!(
                "this task's last run is still working, including its helpers; wait for it to finish"
            ));
        }
        let title = format!("{} · {}", job.name, crate::jobs::fmt_time(now_ms()));
        let bot = job.bot_id.as_deref().and_then(|b| self.find_bot(b));
        // A removed bot's duty doesn't run as a plain chat instead.
        if job.bot_id.is_some() && bot.is_none() {
            anyhow::bail!("the bot this duty belongs to was removed");
        }
        let rt = self.create_session(
            Some(job.cwd.clone()),
            job.model.clone(),
            Some(title),
            SessionKind::Job,
            None,
            Some(job.permission),
            bot.as_ref(),
        )?;
        // Accepted now: a Stop before its task gets going still counts.
        let marks = self.stops_here();
        let cancel = CancellationToken::new();
        {
            let mut st = rt.st.lock();
            st.next_turn_stops = Some(marks);
            // Stoppable, and running if the server stops before the turn
            // starts (it then continues after the restart like any run).
            st.running = true;
            st.cancel = Some(cancel.clone());
            st.info.status = SessionStatus::Running;
        }
        self.save(&rt);
        // The job's standing instructions, not something the user just said,
        // kept before the turn starts.
        if let Err(e) = self.append(
            &rt,
            EntryBody::User {
                content: vec![ContentPart::text(job.prompt.clone())],
                author: Some(format!("job:{}", job.id)),
            },
        ) {
            {
                let mut st = rt.st.lock();
                st.running = false;
                st.cancel = None;
                st.info.status = SessionStatus::Error;
            }
            self.save(&rt);
            return Err(e);
        }
        let info = rt.st.lock().info.clone();
        let now = now_ms();
        let before = job.clone();
        let mut job = job;
        job.runs = job.runs.saturating_add(1);
        job.last_run = Some(now);
        job.last_session = Some(info.id.clone());
        job.last_status = Some("running".into());
        job.last_end = None;
        job.next_run = if job.enabled {
            crate::jobs::next_run_within(&job, now)
        } else {
            None
        };
        if let Err(e) = self.store.reserve_job_run(&before, &job, &info.id, now) {
            let mut st = rt.st.lock();
            st.running = false;
            st.cancel = None;
            st.info.status = SessionStatus::Error;
            drop(st);
            self.save(&rt);
            return Err(e);
        }
        drop(admission);
        tracing::info!(job_id = %job.id, run = job.runs, session_id = %info.id, "job started");
        let _ = self.events.send(Event::JobUpdated { job: job.clone() });
        // Its last run, or none left before its end date: no more.
        if job.enabled {
            let why = crate::jobs::schedule_over(&job, now).or_else(|| {
                (job.next_run.is_none() && job.until.is_some())
                    .then(|| "no run comes before its end date".to_string())
            });
            if let Some(why) = why {
                self.end_schedule(job, &why);
            }
        }
        let me = self.clone();
        tokio::spawn(async move {
            // How it ends is recorded as its turn ends (`inbox.rs`).
            me.run(rt, cancel).await;
        });
        Ok(info)
    }

    /// Why no tool runs: the user stopped Tessera on this computer (`tessera
    /// node stop`, or the Stop bar of the machine on it); `None` otherwise.
    pub(crate) fn stopped_here(&self) -> Option<String> {
        self.here_now().stopped.then(|| {
            "The user stopped Tessera on this computer (its Stop bar, the Stop key or `tessera node stop`): no tool runs here until they resume it on this computer. Don't retry; tell the user.".to_string()
        })
    }

    /// The user's Stops on this computer now: its Stop file, and this very
    /// computer's own machines (their Stop bar; also one with its own
    /// Tessera folder, or a Stop it couldn't save, said live).
    fn here_now(&self) -> HereNow {
        let latch = crate::node::StopLatch::load(&self.paths);
        HereNow {
            stopped: latch.stopped || self.machines.this_computer_stopped(),
            file: latch.stops,
            machines: self.machines.this_computer_counts(),
        }
    }

    /// What a turn accepted now is from: every Stop count it knows (here and
    /// on each machine). Counts only go up and are kept where they are
    /// counted, so a later Stop is always a higher count than the turn's,
    /// across restarts and whatever arrived in which order.
    pub(crate) fn stops_here(&self) -> StopMarks {
        StopMarks {
            here: crate::node::StopLatch::load(&self.paths).stops,
            machines: self.machines.stop_counts(),
        }
    }

    /// Not stopped now, and no Stop here since a turn from `marks`.
    pub(crate) fn running_here_since(&self, marks: &StopMarks) -> bool {
        let now = self.here_now();
        !now.stopped && now.since(marks)
    }

    /// The same for `rt`'s turn (its marks copied out first: nothing is read
    /// from disk while its state is locked).
    fn turn_running_here(&self, rt: &Rt) -> bool {
        let marks = rt.st.lock().turn_stops.clone();
        self.running_here_since(&marks)
    }

    /// Every chat's turn here ends when the user stops this computer, also
    /// a Stop already resumed by the next look (a second later): its count
    /// is past theirs. `stopped_here` refuses their tools meanwhile.
    async fn watch_stop_here(engine: std::sync::Weak<Self>) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            // Ends with the engine.
            let Some(me) = engine.upgrade() else {
                return;
            };
            let now = me.here_now();
            // Never stopped here: no turn can be from before a Stop.
            if now.file == 0 && now.machines.iter().all(|(_, n)| *n == 0) {
                continue;
            }
            let mut ended = false;
            // Helpers among them first, by their own run (not a later run of
            // the same helper): marked stopped before they are ended, so
            // their reports never start a new turn of their parent.
            let runs: Vec<(String, u64, CancellationToken)> = me
                .background
                .lock()
                .values()
                .flatten()
                .filter(|a| !a.done && !a.cancel.is_cancelled())
                .map(|a| (a.session_id.clone(), a.batch, a.cancel.clone()))
                .collect();
            for (id, batch, token) in runs {
                let stale = me
                    .rt(&id)
                    .is_ok_and(|rt| !now.since(&rt.st.lock().turn_stops));
                if stale {
                    if let Some(a) = me
                        .background
                        .lock()
                        .values_mut()
                        .flatten()
                        .find(|a| a.session_id == id && a.batch == batch)
                    {
                        a.stopped = true;
                    }
                    token.cancel();
                    ended = true;
                }
            }
            // Then every other turn from before (exact tokens). The sessions
            // first, each one's state after (never both at once).
            let rts: Vec<Arc<Rt>> = me.sessions.lock().values().cloned().collect();
            for rt in rts {
                let token = {
                    let st = rt.st.lock();
                    (st.running && !now.since(&st.turn_stops))
                        .then(|| st.cancel.clone())
                        .flatten()
                };
                if let Some(t) = token.filter(|t| !t.is_cancelled()) {
                    t.cancel();
                    ended = true;
                }
            }
            if ended {
                let _ = me.events.send(Event::Notice {
                    session_id: None,
                    level: NoticeLevel::Warning,
                    message: "Tessera was stopped on the computer it runs on: every chat's turn there ended, and no tool runs until it is resumed on that computer.".into(),
                });
            }
        }
    }

    async fn scheduler(self: Arc<Self>) {
        use std::time::Duration;
        // Jobs missed while the server was down run once on startup. Then it
        // sleeps until something is due (a job, a notification, a look at a
        // model server that was off, giving memory back), five minutes at
        // most, and wakes early when a job is saved, a device connects or a
        // chat's turn ends. (It used to wake every 20 s.)
        let mut quiet = memory::Quiet::default();
        let mut wake = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => {}
                _ = self.sched.notified() => {}
                _ = self.turn_ended.notified() => {}
            }
            let mut next = Duration::from_secs(300);
            let after = |at: u64, now: u64| Duration::from_millis(at.saturating_sub(now));
            self.maintain_sessions();
            self.deliver_due_notifications();
            self.remind_remote_key();
            quiet.look(self.live_sessions().iter().any(|rt| rt.st.lock().running));
            if let Some(at) = quiet.next_look() {
                next = next.min(at.saturating_duration_since(std::time::Instant::now()));
            }
            // A model server that was off: ask now and then while someone is
            // connected, so its models come back by themselves.
            if self.providers.any_offline()
                && self.clients.load(std::sync::atomic::Ordering::Relaxed) > 0
            {
                next = next.min(Duration::from_secs(20));
                let me = self.clone();
                tokio::spawn(async move {
                    let gap = std::time::Duration::from_secs(60);
                    if me.providers.recheck_offline(gap).await {
                        let _ = me.events.send(Event::ProvidersChanged);
                    }
                });
            }
            let now = now_ms();
            if let Ok(Some(due)) = self.store.next_notification_due() {
                next = next.min(after(due, now));
            }
            if let Ok(Some(at)) = self.store.kv_get("notification_digest_next")
                && let Ok(due) = at.parse::<u64>()
            {
                next = next.min(after(due, now));
            }
            if let Ok(jobs) = self.store.jobs() {
                for job in jobs {
                    if !job.enabled {
                        continue;
                    }
                    // Its end date came, or its runs are used up: no more.
                    if let Some(why) = crate::jobs::schedule_over(&job, now) {
                        self.end_schedule(job, &why);
                        continue;
                    }
                    match job.next_run {
                        Some(t) if t <= now => {
                            if self.job_is_working(&job) {
                                // Don't overlap runs of the same job: look again
                                // when a turn ends, or soon.
                                next = next.min(Duration::from_secs(20));
                                continue;
                            }
                            if let Err(e) = self.clone().admit_job(&job.id, true) {
                                tracing::error!("job {} failed to start: {e}", job.id);
                                // Not again at once: at its next time.
                                self.defer_job(&job.id, now);
                            }
                            if let Some(t) = crate::jobs::next_run_within(&job, now) {
                                next = next.min(after(t, now));
                            }
                        }
                        Some(t) => next = next.min(after(t, now)),
                        None => {
                            let gate = self.job_gate(&job.id);
                            let admission = gate.lock();
                            let Some(mut j) = self
                                .store
                                .jobs()
                                .ok()
                                .and_then(|v| v.into_iter().find(|j| j.id == job.id))
                            else {
                                continue;
                            };
                            if !j.enabled || j.next_run.is_some() {
                                continue;
                            }
                            j.next_run = crate::jobs::next_run_within(&j, now);
                            if j.next_run.is_none() && j.until.is_some() {
                                drop(admission);
                                self.end_schedule(j, "no run comes before its end date");
                                continue;
                            }
                            let _ = self.store.put_job(&j);
                            if let Some(t) = j.next_run {
                                next = next.min(after(t, now));
                            }
                        }
                    }
                }
            }
            // Never in a tight loop (a notification that couldn't be sent).
            wake = tokio::time::Instant::now() + next.max(Duration::from_secs(1));
        }
    }

    // ---- bots ------------------------------------------------------------------

    fn all_bots(&self) -> Vec<Bot> {
        let mut v = self.bots.read().clone();
        v.sort_by_key(|b| b.name.to_lowercase());
        v
    }

    /// By id, handle or @handle.
    fn find_bot(&self, key: &str) -> Option<Bot> {
        let k = key.trim().trim_start_matches('@');
        let h = crate::bots::normalize_handle(k);
        self.bots
            .read()
            .iter()
            .find(|b| b.id == k || (!h.is_empty() && b.handle == h))
            .cloned()
    }

    fn speaker_id(&self, st: &RtState) -> Option<String> {
        st.speaker.clone().or_else(|| st.info.bot_id.clone())
    }

    fn speaker_bot(&self, st: &RtState) -> Option<Bot> {
        self.speaker_id(st).and_then(|id| self.find_bot(&id))
    }

    fn effective_model(&self, st: &RtState) -> ModelRef {
        match (
            is_room(&st.info),
            self.speaker_bot(st)
                .and_then(|b| b.model)
                .and_then(|m| self.resolve_ref(&m).ok()),
        ) {
            (true, Some(m)) if self.providers.exists(&m.provider) => m,
            _ => st.info.model.clone(),
        }
    }

    fn memory_for(&self, bot: Option<&Bot>) -> Arc<Memory> {
        let Some(bot) = bot.filter(|b| b.own_memory || !b.share_user_profile) else {
            return self.memory.clone();
        };
        if let Some(m) = self.bot_memories.lock().get(&bot.id) {
            return m.clone();
        }
        let cfg = self.config.read().memory.clone();
        let dir = self
            .paths
            .data_dir
            .join("bots")
            .join(&bot.id)
            .join("memory");
        let notes = if bot.own_memory {
            dir.clone()
        } else {
            self.paths.memory_dir()
        };
        let user = if bot.share_user_profile {
            self.paths.memory_dir()
        } else {
            dir
        };
        let m = Arc::new(Memory::with_user_dir(
            notes,
            user,
            cfg.memory_chars,
            cfg.user_chars,
        ));
        self.bot_memories.lock().insert(bot.id.clone(), m.clone());
        m
    }

    /// How the model is told who wrote each message in `entries`, by author
    /// (see `author_label`).
    fn author_labels(&self, entries: &[Entry], st: &RtState) -> HashMap<String, String> {
        let bots = self.all_bots();
        let speaker = self.speaker_id(st);
        // Another chat's agent by its title, read from the store: this
        // session is locked, and live ones are locked after the session list.
        let chat = |id: &str| {
            (Some(id) != st.info.parent_id.as_deref()).then(|| {
                self.store
                    .get_session(id)
                    .ok()
                    .flatten()
                    .map(|(i, _)| one_line(&i.title, 60))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "another chat".into())
            })
        };
        let mut out = HashMap::new();
        for e in entries {
            let (EntryBody::User {
                author: Some(a), ..
            }
            | EntryBody::Assistant {
                author: Some(a), ..
            }) = &e.body
            else {
                continue;
            };
            if !out.contains_key(a) {
                out.insert(a.clone(), author_label(a, speaker.as_deref(), &bots, chat));
            }
        }
        out
    }

    fn view_for(&self, entries: &[Entry], st: &RtState) -> context::View {
        let labels = self.author_labels(entries, st);
        let speaker = self.speaker_id(st);
        let handle = |id: &str| labels.get(id).cloned().unwrap_or_default();
        let who = context::Perspective {
            speaker: speaker.as_deref(),
            room: is_room(&st.info),
            handle: &handle,
        };
        // The ledger as of the last history edit (see `ledger_shown`).
        let ledger = match &st.internal.ledger_shown {
            Some(l) => Some(l),
            None => st.info.task.as_ref(),
        };
        let mut view = context::build_view_as(entries, &st.internal, ledger, &who);
        // Small model mode: worked examples open the conversation (always
        // the same, so the prompt cache holds).
        let examples = self.small_examples(st);
        if !examples.is_empty() {
            view.messages.splice(0..0, examples);
        }
        view
    }

    fn resolve_members(&self, members: &[String]) -> Result<Vec<String>, ApiError> {
        let mut out = Vec::new();
        for m in members {
            let b = self
                .find_bot(m)
                .ok_or_else(|| bad(format!("no bot `{m}`")))?;
            if !out.contains(&b.id) {
                out.push(b.id);
            }
        }
        Ok(out)
    }

    fn save_bot(&self, mut bot: Bot) -> Result<Bot, ApiError> {
        bot.name = one_line(&bot.name, 40);
        if bot.name.is_empty() {
            return Err(bad("give the bot a name"));
        }
        let handle = crate::bots::normalize_handle(if bot.handle.trim().is_empty() {
            &bot.name
        } else {
            &bot.handle
        });
        if handle.is_empty() {
            return Err(bad("the handle needs letters or digits"));
        }
        if ["user", "you", "all", "everyone", "here", "tessera", "main"].contains(&handle.as_str())
        {
            return Err(bad(format!("@{handle} is reserved; pick another handle")));
        }
        let existing = self.bots.read().iter().find(|b| b.id == bot.id).cloned();
        if self
            .bots
            .read()
            .iter()
            .any(|b| b.handle == handle && b.id != bot.id)
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!("another bot already uses @{handle}"),
            ));
        }
        bot.handle = handle;
        let now = now_ms();
        match &existing {
            Some(e) => bot.created_at = e.created_at,
            None => {
                bot.id = format!("bot_{}", &new_id()[10..]);
                bot.created_at = now;
            }
        }
        // An app that doesn't know the switch keeps what is set.
        if bot.askable.is_none() {
            bot.askable = existing.as_ref().and_then(|e| e.askable);
        }
        bot.updated_at = now;
        bot.avatar = bot.avatar.trim().to_string();
        if bot.avatar.is_empty() {
            bot.avatar = bot
                .name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".into());
        }
        // A mosaic portrait (`m:<id>`, drawn by the apps) or a short emoji/text.
        if !is_mosaic_avatar(&bot.avatar) {
            bot.avatar = bot.avatar.chars().take(4).collect();
        }
        let valid_color = bot.color.len() == 7
            && bot.color.starts_with('#')
            && bot.color[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !valid_color {
            let n = self.bots.read().len();
            bot.color =
                crate::bots::DEFAULT_COLORS[n % crate::bots::DEFAULT_COLORS.len()].to_string();
        }
        bot.role = one_line(&bot.role, 120);
        let mut contacts: Vec<String> = Vec::new();
        for h in &bot.can_message {
            let h = if h.trim() == "*" {
                "*".to_string()
            } else {
                crate::bots::normalize_handle(h)
            };
            if !h.is_empty() && h != bot.handle && !contacts.contains(&h) {
                contacts.push(h);
            }
        }
        bot.can_message = contacts;
        // Knowledge: notes that exist, once each.
        let mut knowledge: Vec<String> = Vec::new();
        for id in &bot.knowledge {
            let id = id.trim().to_string();
            if !knowledge.contains(&id) && self.store.note(&id).ok().flatten().is_some() {
                knowledge.push(id);
            }
        }
        knowledge.truncate(50);
        bot.knowledge = knowledge;
        bot.starters = bot
            .starters
            .iter()
            .map(|s| one_line(s, 200))
            .filter(|s| !s.is_empty())
            .take(4)
            .collect();
        // Machines and passwords open to any chat: kept when an edit leaves
        // them out; none for a new bot.
        crate::bots::settle_machines(&mut bot, existing.as_ref());
        crate::bots::settle_secrets(&mut bot, existing.as_ref());
        if let Some(c) = bot.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
            if !crate::util::expand_home(c).is_dir() {
                return Err(bad(format!("{c} is not a directory")));
            }
        } else {
            bot.cwd = None;
        }
        if let Some(m) = &bot.model {
            // `@role` bots follow the role as the config changes.
            match m.as_role() {
                Some(role) => {
                    self.config.read().role(role).map_err(bad)?;
                }
                None if !self.providers.exists(&m.provider) => {
                    return Err(bad(format!("unknown provider `{}`", m.provider)));
                }
                None => {}
            }
        }
        self.store.put_bot(&bot).map_err(internal)?;
        {
            let mut bots = self.bots.write();
            bots.retain(|b| b.id != bot.id);
            bots.push(bot.clone());
        }
        self.bot_memories.lock().remove(&bot.id);
        // Sessions with this bot pick up its new persona on their next turn.
        for rt in self.live_sessions() {
            let mut st = rt.st.lock();
            self.publish_permission(&st);
            if st.info.bot_id.as_deref() == Some(bot.id.as_str()) {
                st.internal.system_prompt = None;
                st.internal
                    .claude_code
                    .iter_mut()
                    .for_each(|l| l.context = None);
            }
            st.internal.bot_prompts.remove(&bot.id);
            if let Some(l) = st.internal.claude_code_bots.get_mut(&bot.id) {
                l.context = None;
            }
        }
        let _ = self.events.send(Event::BotUpdated { bot: bot.clone() });
        Ok(bot)
    }

    async fn room_rounds(self: &Arc<Self>, rt: &Arc<Rt>, cancel: &CancellationToken) -> Outcome {
        loop {
            let outcome = self.room_round(rt, cancel).await;
            if !matches!(outcome, Outcome::Done) {
                return outcome;
            }
            let next = {
                let mut st = rt.st.lock();
                if st.followups.is_empty() {
                    None
                } else {
                    st.take_one(Queue::FollowUp)
                }
            };
            match next {
                Some(p) => {
                    let _ = self.append_waiting(rt, p);
                    self.queue_changed(&rt.st.lock());
                }
                None => return Outcome::Done,
            }
        }
    }

    /// One round in a channel: the @mentioned members (or everyone) reply in
    /// turn; a reply that @mentions another member hands the floor to it.
    async fn room_round(self: &Arc<Self>, rt: &Arc<Rt>, cancel: &CancellationToken) -> Outcome {
        const MAX_TURNS: u32 = 8;
        let (members, id) = {
            let st = rt.st.lock();
            (st.info.members.clone(), st.info.id.clone())
        };
        let bots: Vec<Bot> = members.iter().filter_map(|m| self.find_bot(m)).collect();
        if bots.is_empty() {
            return Outcome::Failed("This channel has no bots. Add members first.".into());
        }
        let entries = self.store.entries(&id, 0).unwrap_or_default();
        let last_user = entries
            .iter()
            .rev()
            .find_map(|e| match &e.body {
                EntryBody::User {
                    content,
                    author: None,
                } => Some(plain_text(content)),
                _ => None,
            })
            .unwrap_or_default();
        let mentioned: Vec<String> = crate::bots::mentions(&last_user)
            .iter()
            .filter_map(|h| bots.iter().find(|b| &b.handle == h).map(|b| b.id.clone()))
            .collect();
        let mut queue: std::collections::VecDeque<String> = if mentioned.is_empty() {
            bots.iter().map(|b| b.id.clone()).collect()
        } else {
            mentioned.into()
        };
        let mut spoke: HashMap<String, u32> = HashMap::new();
        let mut turns = 0;
        while let Some(bid) = queue.pop_front() {
            if cancel.is_cancelled() {
                return Outcome::Aborted;
            }
            if turns >= MAX_TURNS {
                self.notice(
                    rt,
                    NoticeLevel::Info,
                    "The bots handed the conversation back and forth several times; over to you.",
                );
                break;
            }
            let from_seq = rt.st.lock().info.last_seq;
            {
                let mut st = rt.st.lock();
                st.speaker = Some(bid.clone());
                self.publish_permission(&st);
            }
            // A bot on Claude Code replies through it (its own conversation).
            let out = if self.runs_claude_code(&rt.st.lock()) {
                crate::activity::monitor(
                    &crate::activity::CLAUDE,
                    self.claude_code_turn(rt, cancel),
                )
                .await
            } else {
                self.turn_loop(rt, cancel).await
            };
            {
                let mut st = rt.st.lock();
                st.speaker = None;
                self.publish_permission(&st);
            }
            turns += 1;
            *spoke.entry(bid.clone()).or_default() += 1;
            let handle = bots
                .iter()
                .find(|b| b.id == bid)
                .map(|b| b.handle.clone())
                .unwrap_or_default();
            match out {
                Outcome::Aborted => return Outcome::Aborted,
                Outcome::Failed(e) => {
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!("@{handle} couldn't reply: {e}"),
                    );
                    continue;
                }
                Outcome::Done => {}
            }
            let reply = self.last_reply_by(&id, from_seq, &bid);
            for h in crate::bots::mentions(&reply) {
                if let Some(b) = bots.iter().find(|b| b.handle == h)
                    && b.id != bid
                    && spoke.get(&b.id).copied().unwrap_or(0) < 2
                    && !queue.contains(&b.id)
                {
                    queue.push_back(b.id.clone());
                }
            }
        }
        Outcome::Done
    }

    fn last_reply_by(&self, session: &str, after_seq: u64, bot: &str) -> String {
        self.store
            .entries(session, after_seq)
            .unwrap_or_default()
            .iter()
            .rev()
            .filter(|e| !crate::tools::meta::is_protocol_entry(e))
            .find_map(|e| match &e.body {
                EntryBody::Assistant {
                    content, author, ..
                } if author.as_deref() == Some(bot) => {
                    let t: String = content
                        .iter()
                        .filter_map(|p| {
                            if let AssistantPart::Text { text } = p {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!t.trim().is_empty() && !is_check_ack(&t)).then_some(t)
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Private bot-to-bot message: runs the recipient with its own tools,
    /// permissions and memory in a DM channel the user can inspect, and
    /// waits for its reply (`message_bot` with `wait`; see
    /// `engine/handoffs.rs` for the background way).
    async fn message_bot(
        self: Arc<Self>,
        caller: String,
        target: String,
        message: String,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        let started = self.start_bot_message(&caller, &target, message)?;
        self.clone()
            .run(started.dm.clone(), cancel.child_token())
            .await;
        let terminal = started.receipt.lock().clone();
        let answer = self.bot_answer(&started.dm_id, terminal.as_ref())?;
        Ok(format!("@{} replied:\n\n{}", started.to.handle, answer))
    }

    /// Everything a message to a bot needs before the bot works on it: who
    /// may ask whom, its conversation with the asker (made the first time),
    /// marked running with the message in it.
    fn start_bot_message(
        &self,
        caller: &str,
        target: &str,
        message: String,
    ) -> Result<BotMessage, String> {
        let caller_rt = self.rt(caller).map_err(|e| e.message)?;
        let (from, author, depth, approvals_on, cwd, caller_stops) = {
            let st = caller_rt.st.lock();
            (
                self.speaker_bot(&st),
                // Its bot, or (a plain chat) the chat: never the user.
                agent_author(&st),
                st.bot_depth,
                st.approvals_on.clone(),
                st.info.cwd.clone(),
                // The bot's work belongs to the caller's turn (its Stops).
                st.turn_stops.clone(),
            )
        };
        let to = self.find_bot(target).ok_or_else(|| {
            let known: Vec<String> = self
                .all_bots()
                .iter()
                .map(|b| format!("@{}", b.handle))
                .collect();
            format!(
                "There is no bot `{target}`. Known bots: {}.",
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            )
        })?;
        if let Some(f) = &from {
            if f.id == to.id {
                return Err("You can't message yourself.".into());
            }
            if !crate::bots::may_message(f, &to) {
                return Err(format!(
                    "You are not allowed to message @{}. The user can allow it in your bot settings.",
                    to.handle
                ));
            }
        } else if self.config.read().security.bot_handoff == BotHandoff::Off {
            return Err(
                "Chats may not hand work to bots: the user turned that off (Settings → Privacy & safety). Do it here, or ask the user to talk to the bot."
                    .into(),
            );
        }
        if !crate::bots::askable(&to) {
            return Err(format!(
                "@{} takes work only from the user, not from other chats or bots (its settings). Do it here, or ask the user to talk to it.",
                to.handle
            ));
        }
        if depth >= 3 {
            return Err("Too many nested bot conversations; finish this one yourself.".into());
        }
        if message.trim().is_empty() {
            return Err("The message is empty.".into());
        }
        let key = format!(
            "dm:{}:{}",
            from.as_ref().map(|b| b.id.as_str()).unwrap_or("main"),
            to.id
        );
        let existing = self
            .store
            .kv_get(&key)
            .ok()
            .flatten()
            .and_then(|sid| self.rt(&sid).ok());
        let dm = match existing {
            Some(rt) => rt,
            None => {
                let title = match &from {
                    Some(f) => format!("@{} ↔ @{}", f.handle, to.handle),
                    None => format!("Your chats → @{}", to.handle),
                };
                let cwd = to
                    .cwd
                    .clone()
                    .filter(|c| crate::util::expand_home(c).is_dir())
                    .unwrap_or(cwd);
                let rt = self
                    .create_session(
                        Some(cwd),
                        None,
                        Some(title),
                        SessionKind::Channel,
                        None,
                        None,
                        Some(&to),
                    )
                    .map_err(|e| e.to_string())?;
                let id = {
                    let mut st = rt.st.lock();
                    st.info.members = from
                        .iter()
                        .map(|b| b.id.clone())
                        .chain([to.id.clone()])
                        .collect();
                    st.info.id.clone()
                };
                self.save(&rt);
                let _ = self.store.kv_set(&key, &id);
                rt
            }
        };
        let receipt: HandoffReceipt = Arc::new(Mutex::new(None));
        let (dm_id, from_seq) = {
            let mut st = dm.st.lock();
            if st.running {
                // Its reply to this chat's earlier hand-off comes here first.
                let ours = st
                    .info
                    .working_for
                    .as_ref()
                    .is_some_and(|w| w.session_id == caller);
                return Err(if ours {
                    format!(
                        "@{} is still working on what this chat handed it earlier; its reply will arrive here. Send this after that, or put both in one message.",
                        to.handle
                    )
                } else {
                    format!(
                        "@{} is busy with another conversation; try again shortly.",
                        to.handle
                    )
                });
            }
            st.running = true;
            st.next_turn_stops = Some(caller_stops);
            st.bot_depth = depth + 1;
            st.bot_receipt = Some(receipt.clone());
            st.approvals_on = approvals_on;
            (st.info.id.clone(), st.info.last_seq)
        };
        if let Err(e) = self.append(
            &dm,
            EntryBody::User {
                content: vec![ContentPart::text(message)],
                author: Some(author),
            },
        ) {
            dm.st.lock().running = false;
            return Err(e.to_string());
        }
        Ok(BotMessage {
            dm,
            dm_id,
            from_seq,
            to,
            receipt,
        })
    }

    /// What `to` answered in its conversation `dm_id` after `from_seq`, or
    /// why it didn't.
    fn bot_answer(
        &self,
        dm_id: &str,
        attempt: Option<&crate::store::Attempt>,
    ) -> Result<String, String> {
        let entries = self.store.entries(dm_id, 0).map_err(|e| e.to_string())?;
        let text = attempts::history(&entries, attempt);
        let outcome = attempt.and_then(|a| a.outcome);
        if outcome == Some(crate::store::AttemptOutcome::Completed) {
            Ok(text)
        } else {
            Err(text)
        }
    }

    // ---- learning loop -----------------------------------------------------------

    /// Background review: look back over recent turns and curate memory and
    /// skills. Runs with only memory/skill tools and never blocks the user.
    async fn review(self: Arc<Self>, parent: Arc<Rt>) {
        // It works from the chat's latest turn: not after a Stop here since
        // (the chat's next turn brings it back), and from that turn's Stops.
        let marks = parent.st.lock().turn_stops.clone();
        if !self.running_here_since(&marks) {
            return;
        }
        let (pid, cwd, since) = {
            let mut st = parent.st.lock();
            if st.info.private {
                return;
            }
            st.internal.reviewed_turns = st.internal.user_turns;
            (
                st.info.id.clone(),
                st.info.cwd.clone(),
                st.internal.reviewed_seq,
            )
        };
        let Ok(entries) = self.store.entries(&pid, 0) else {
            self.save(&parent);
            return;
        };
        let last = entries.last().map(|e| e.seq + 1).unwrap_or(0);
        parent.st.lock().internal.reviewed_seq = last;
        self.save(&parent);
        // Only what the last review didn't read (a rewind can leave its mark
        // past the end), at most the last REVIEW_WINDOW entries.
        let since = if since > last { 0 } else { since };
        let from = entries.len().saturating_sub(REVIEW_WINDOW);
        let from_seq = entries.get(from).map(|e| e.seq).unwrap_or(0).max(since);
        let new = &entries[entries.partition_point(|e| e.seq < from_seq)..];
        if too_little_to_review(new) {
            return;
        }
        let (mut transcript, _) =
            crate::util::head_tail(&context::transcript(&entries, from_seq, last), 60_000, 0.3);
        if since > 0 && from_seq == since {
            transcript.insert_str(0, "[What came before was reviewed already.]\n");
        }
        let parent_bot = self.speaker_bot(&parent.st.lock());
        let state = self.memory_for(parent_bot.as_ref()).state();
        // A chat of a few quick commands has no procedure in it: the review
        // isn't even offered the skill tools (unless the user asked for one).
        // Nor when it read pages from the web: text from a page is not to
        // become instructions every later chat loads.
        let may_skill = {
            let ev = evidence_from(new);
            (ev.user_asked || ev.tool_calls >= crate::skills::MIN_WORK) && !read_the_web(new)
        };
        let skills = may_skill
            .then(|| crate::skills::names_and_descriptions(&self.skills_for(Path::new(&cwd))));
        // A bot with its own memory: what it learned for its work stays its own.
        let own = parent_bot
            .as_ref()
            .filter(|b| b.own_memory && b.share_user_profile)
            .map(|b| format!("@{} ({})", b.handle, b.name));
        let prompt = crate::memory::review_prompt(
            &state.user,
            &state.memory,
            skills.as_deref(),
            &transcript,
            own.as_deref(),
        );
        // Unset: the chat's own model (a chat on Claude Code: its smallest).
        let model = self.memory_model().or_else(|| {
            let own = self.effective_model(&parent.st.lock());
            (self.providers.kind_of(&own.provider) == Some(ProviderKind::ClaudeCode))
                .then(|| self.utility_for(own))
        });
        let rt = {
            let pst = parent.st.lock();
            let bot = self.speaker_bot(&pst);
            // A helper, not a subagent: clients don't list it; what it
            // learned shows in the chat as a notice.
            match self.create_session_ext(
                Some(cwd),
                model,
                Some("Memory review".into()),
                SessionKind::Subagent,
                Some(&pst),
                Some(PermissionMode::Auto),
                bot.as_ref(),
                false,
                true,
                false,
                None,
            ) {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("memory review could not start: {e}");
                    return;
                }
            }
        };
        {
            let mut st = rt.st.lock();
            st.depth = 0;
            st.tools_only = Some(if may_skill {
                vec!["memory", "skill_manage", "skill"]
            } else {
                vec!["memory"]
            });
            // Its message holds everything it needs (memory included once).
            st.internal.system_prompt = Some(crate::memory::REVIEW_SYSTEM.into());
            st.info.thinking = ThinkingLevel::Low;
            st.running = true;
            st.next_turn_stops = Some(marks);
        }
        if self
            .append(
                &rt,
                EntryBody::User {
                    content: vec![ContentPart::text(prompt)],
                    author: None,
                },
            )
            .is_err()
        {
            return;
        }
        self.clone().run(rt.clone(), CancellationToken::new()).await;
        // Surface what was learned on the parent session (only calls that
        // succeeded: a full memory or a bad edit taught nothing).
        let id = rt.st.lock().info.id.clone();
        let entries = self.store.entries(&id, 0).unwrap_or_default();
        let failed: std::collections::HashSet<&str> = entries
            .iter()
            .filter_map(|e| match &e.body {
                EntryBody::ToolResult {
                    call_id,
                    is_error: true,
                    ..
                } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        let learned: Vec<LearnedItem> = entries
            .iter()
            .filter_map(|e| match &e.body {
                EntryBody::Assistant { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter_map(|p| match p {
                AssistantPart::ToolCall {
                    id,
                    name,
                    arguments,
                } if !failed.contains(id.as_str()) => learned_item(name, arguments),
                _ => None,
            })
            .collect();
        if !learned.is_empty() {
            let text = format!(
                "Learned from this conversation — {}",
                learned
                    .iter()
                    .map(|l| format!(
                        "{}: {}",
                        match l.kind {
                            LearnedKind::Profile => "about you",
                            LearnedKind::Note => "note",
                            LearnedKind::Skill => "skill",
                        },
                        l.text
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            let _ = self.append(
                &parent,
                EntryBody::Notice {
                    level: NoticeLevel::Info,
                    text,
                    learned,
                },
            );
        }
    }

    // ---- subagents ----------------------------------------------------------------

    /// Subagents only: once the step or time budget is spent, ask for the
    /// report and take the tools away so the next reply is the answer.
    fn check_budget(&self, rt: &Rt) {
        let mut st = rt.st.lock();
        let Some(b) = st.budget.as_mut() else { return };
        if b.wrapped
            || ((b.max_steps == helper_limits::UNLIMITED
                || b.used < b.max_steps
                || b.calls.values().any(|c| c.pending))
                && b.soft_deadline
                    .is_none_or(|d| std::time::Instant::now() < d))
        {
            return;
        }
        b.wrapped = true;
        st.no_tool_calls = true;
        let stops = st.turn_stops.clone();
        st.steer.push(Waiting {
            id: new_id(),
            content: vec![ContentPart::text(
                "[Budget reached] You have used the step/time budget for this task. Do not call any more tools. \
                 Write your final report now: what you found (with sources or file paths), what you could not verify, \
                 and what you would do next.",
            )],
            author: Some("harness".into()),
            stops,
        });
    }

    /// A model by `provider/model`, id, part of its name, or `@role`.
    fn resolve_model(&self, want: &str) -> Result<ModelRef, String> {
        let want = want.trim();
        if let Some(role) = want.strip_prefix('@') {
            let target = self.config.read().role(role)?;
            // A role names its model on purpose: any model of a known
            // provider will do, listed or not.
            if let Ok(m) = target.parse::<ModelRef>()
                && m.as_role().is_none()
                && self.providers.exists(&m.provider)
            {
                return Ok(m);
            }
            return self
                .resolve_model(&target)
                .map_err(|e| format!("@{role} names `{target}`: {e}"));
        }
        let models: Vec<ModelInfo> = self
            .providers
            .infos()
            .into_iter()
            .filter(|p| p.authenticated)
            .flat_map(|p| p.models)
            .collect();
        let lower = want.to_ascii_lowercase();
        let exact = models.iter().find(|m| {
            format!("{}/{}", m.provider, m.id).eq_ignore_ascii_case(want)
                || m.id.eq_ignore_ascii_case(want)
        });
        let fuzzy = || {
            let norm = |s: &str| s.to_ascii_lowercase().replace([' ', '-', '_', '.'], "");
            let w = norm(&lower);
            models
                .iter()
                .find(|m| norm(&m.id).contains(&w) || norm(&m.name).contains(&w))
        };
        match exact.or_else(fuzzy) {
            Some(m) => Ok(ModelRef {
                provider: m.provider.clone(),
                model: m.id.clone(),
            }),
            None => Err(format!(
                "unknown model `{want}`; available: {}",
                models
                    .iter()
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

struct Prepared {
    author: Option<String>,
    model: ModelRef,
    request: LlmRequest,
    estimate: u64,
}

/// Why call `k` of a reply isn't run under the reply guard (see
/// `run_tools`): the same call came earlier in the reply, or it changes
/// something or runs a program after `failed` (a change or command of the
/// reply) failed. Looking (read, grep, glob…) still runs.
fn held_back(
    calls: &[(String, String, Value)],
    k: usize,
    failed: Option<&str>,
    tool: Option<&dyn crate::tools::Tool>,
) -> Option<String> {
    let (_, name, args) = &calls[k];
    if calls[..k].iter().any(|(_, n, a)| n == name && a == args) {
        return Some(
            "Not run: the same call is earlier in this reply; its result is above.".into(),
        );
    }
    let changes = tool.is_some_and(|t| {
        matches!(
            t.kind(),
            crate::tools::ToolKind::Write | crate::tools::ToolKind::Exec
        )
    });
    match failed {
        Some(f) if changes => Some(format!(
            "Not run: `{f}` before it in this reply failed. Look at that result first, then make this call again if it still applies."
        )),
        _ => None,
    }
}

/// A multi-bot room (as opposed to a bot-to-bot DM, which has a `bot_id`).
fn is_room(info: &SessionInfo) -> bool {
    info.kind == SessionKind::Channel && info.bot_id.is_none()
}

/// Who an agent of `st` writes as when its words land in another session
/// (a subagent's task, a message to a bot): its bot, or the chat itself.
fn agent_author(st: &RtState) -> String {
    st.speaker
        .clone()
        .or_else(|| st.info.bot_id.clone())
        .unwrap_or_else(|| format!("chat:{}", st.info.id))
}

/// How the model sees who wrote a message (`[label]: …`); empty: no label.
/// Its own words go unlabelled (a subagent's task from its own bot), as do
/// the agent that started it (`chat` gives no title for that one) and notes
/// that say what they are (goals, reports, a scheduled job's prompt).
fn author_label(
    author: &str,
    speaker: Option<&str>,
    bots: &[Bot],
    chat: impl Fn(&str) -> Option<String>,
) -> String {
    if Some(author) == speaker {
        return String::new();
    }
    if let Some(b) = bots.iter().find(|b| b.id == author) {
        return format!("@{}", b.handle);
    }
    if author == "harness" {
        return "@harness".into();
    }
    if author == "goal" || author.starts_with("subagent:") || author.starts_with("job:") {
        return String::new();
    }
    match author.strip_prefix("chat:") {
        Some(id) => chat(id)
            .map(|t| format!("the user's agent in the chat \"{t}\""))
            .unwrap_or_default(),
        // A bot since removed.
        None => "@bot".into(),
    }
}

/// A message that instructs the agent the way the user's own does: the
/// user's, the chat that handed it a task, or a scheduled job's prompt.
fn instructs(author: Option<&str>) -> bool {
    author.is_none_or(|a| a.starts_with("chat:") || a.starts_with("job:"))
}

/// The store key for the thinking level the user last picked per model.
const MODEL_THINKING_KEY: &str = "model_thinking";

/// A chat the user drives (not a job, room, subagent or helper): the
/// thinking level picked in it is remembered for its model.
fn user_chat(info: &SessionInfo) -> bool {
    info.kind == SessionKind::Chat && info.parent_id.is_none() && !info.internal
}

/// What a successful `memory` or `skill_manage` call taught, for the
/// "learned from this conversation" notice.
/// The notes `set_goal` adds to the conversation.
const GOAL_SET: &str = "[The user set the goal for this session]";
const GOAL_CLEARED: &str = "[The user cleared the session goal]";

/// A user entry as the chat's one-line preview: a goal note reads as the
/// goal, and clearing one leaves the preview as it was.
fn user_preview(content: &[ContentPart]) -> Option<String> {
    let text = plain_text(content);
    if let Some(goal) = text.strip_prefix(GOAL_SET) {
        return Some(format!("Goal: {}", goal.trim()));
    }
    (!text.starts_with(GOAL_CLEARED)).then_some(text)
}

/// Recent entries the memory review (and a new skill's checks) look at.
const REVIEW_WINDOW: usize = 60;

/// What the recent entries of a conversation show about a skill someone
/// wants to save from it.
fn evidence_from(entries: &[Entry]) -> crate::skills::Evidence {
    let recent = &entries[entries.len().saturating_sub(REVIEW_WINDOW)..];
    crate::skills::Evidence {
        tool_calls: tool_work(recent),
        user_asked: recent.iter().any(|e| match &e.body {
            EntryBody::User { content, author } if instructs(author.as_deref()) => {
                crate::skills::asks_for_skill(&plain_text(content))
            }
            _ => false,
        }),
    }
}

/// Tools whose results are pages or search results from the web (Tessera's
/// and Claude Code's).
const WEB_TOOLS: [&str; 5] = [
    "web_fetch",
    "web_search",
    "browser",
    "WebFetch",
    "WebSearch",
];

/// The entries hold something read from the web.
fn read_the_web(entries: &[Entry]) -> bool {
    entries.iter().any(|e| {
        matches!(&e.body, EntryBody::ToolResult { tool, .. } if WEB_TOOLS.contains(&tool.as_str()))
    })
}

/// The nudges a turn has had (each at most once).
#[derive(Default)]
struct Nudged {
    /// After a reply with nothing in it.
    empty: bool,
    /// After a reply cut off at the output limit.
    cut: bool,
    /// After a reply cut off because it kept repeating itself.
    repeat: bool,
    /// After a final reply that said it would act and called nothing.
    action: bool,
}

/// Whether an assistant entry says anything (text that isn't blank).
fn entry_said(entry: &Entry) -> bool {
    matches!(&entry.body, EntryBody::Assistant { content, .. }
        if content.iter().any(|p| matches!(p, AssistantPart::Text { text } if !text.trim().is_empty())))
}

/// The agent's own bookkeeping, as opposed to work on the task.
const BOOKKEEPING: [&str; 6] = ["memory", "skill", "skill_manage", "recall", "docs", "task"];

/// What a turn did so far, for deciding whether the completion check is
/// worth an extra step.
#[derive(Default)]
struct TurnWork {
    /// Any tool call at all.
    tools: bool,
    /// A call that was work on the task (not bookkeeping).
    work: bool,
    /// The latest batch brought the ledger's statuses up to date.
    reviewed: bool,
    /// Subagents started in the background: their reports come later, and
    /// the agent was told not to wait for them.
    delegated: bool,
}

impl TurnWork {
    fn batch(&mut self, calls: &[(String, String, Value)]) {
        self.tools = true;
        self.work |= calls
            .iter()
            .any(|(_, name, _)| !BOOKKEEPING.contains(&name.as_str()));
        self.reviewed = calls.iter().any(|(_, name, args)| {
            name == "task" && (args.get("update").is_some() || args.get("verify").is_some())
        });
        self.delegated |= calls.iter().any(|(_, name, args)| {
            name == "agent" && args.get("wait").and_then(Value::as_bool) != Some(true)
        });
    }

    /// Ask only after a turn that used tools, unless the agent did the work
    /// and then went over its ledger just before answering (it knows what is
    /// open, and asking again only repeats the answer), or left subagents
    /// working in the background.
    fn wants_check(&self) -> bool {
        self.tools && !self.delegated && !(self.work && self.reviewed)
    }
}

/// A short digest of a system prompt (see `SessionInternal::prompt_digest`).
fn prompt_digest(prompt: &str) -> u64 {
    use sha2::Digest as _;
    let d = sha2::Sha256::digest(prompt.as_bytes());
    u64::from_le_bytes(d[..8].try_into().unwrap_or_default())
}

/// Nothing in `entries` could teach anything lasting: the user said almost
/// nothing ("hi", "thanks", "ok") and no work was done.
fn too_little_to_review(entries: &[Entry]) -> bool {
    let words: usize = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User { content, .. } => Some(plain_text(content).split_whitespace().count()),
            _ => None,
        })
        .sum();
    words < 3 && tool_work(entries) == 0
}

/// Tool calls that were work on the task: not the agent's own bookkeeping.
fn tool_work(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter(|p| matches!(p, AssistantPart::ToolCall { name, .. } if !BOOKKEEPING.contains(&name.as_str())))
        .count()
}

fn learned_item(tool: &str, args: &Value) -> Option<LearnedItem> {
    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let (kind, action, text) = match tool {
        "memory" => {
            let kind = if arg("target") == Some("user") {
                LearnedKind::Profile
            } else {
                LearnedKind::Note
            };
            match arg("action")? {
                "add" => (kind, LearnedAction::Added, arg("content")?),
                "replace" => (kind, LearnedAction::Updated, arg("content")?),
                "remove" => (kind, LearnedAction::Removed, arg("old")?),
                _ => return None,
            }
        }
        "skill_manage" => {
            let action = match arg("action")? {
                "create" => LearnedAction::Added,
                "update" | "write_file" => LearnedAction::Updated,
                "delete" => LearnedAction::Removed,
                _ => return None,
            };
            (LearnedKind::Skill, action, arg("name")?)
        }
        _ => return None,
    };
    Some(LearnedItem {
        kind,
        action,
        text: one_line(text, 160),
    })
}

/// The lists of messages waiting for a busy agent.
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Queue {
    /// Sent now: the turn is stopping for them.
    Now,
    Steer,
    FollowUp,
}

impl RtState {
    fn queue(&mut self, q: Queue) -> &mut Vec<Waiting> {
        match q {
            Queue::Now => &mut self.send_now,
            Queue::Steer => &mut self.steer,
            Queue::FollowUp => &mut self.followups,
        }
    }
}

/// What the user sees queued, in the order it reaches the agent: their own
/// messages only (reports and notes just arrive).
fn user_queue(st: &RtState) -> impl Iterator<Item = (Queue, usize, &Waiting)> {
    [
        (Queue::Now, &st.send_now),
        (Queue::Steer, &st.steer),
        (Queue::FollowUp, &st.followups),
    ]
    .into_iter()
    .flat_map(|(q, list)| list.iter().enumerate().map(move |(i, w)| (q, i, w)))
    .filter(|(_, _, w)| w.author.is_none())
}

/// The list and position of the `index`-th message the user sees queued.
fn queued_target(st: &RtState, index: u32) -> Option<(Queue, usize)> {
    user_queue(st).nth(index as usize).map(|(q, i, _)| (q, i))
}

fn queued_messages(st: &RtState) -> Vec<QueuedMessage> {
    user_queue(st)
        .map(|(q, _, w)| {
            // What they typed, whole: editing starts from it, and `EditQueued`
            // replaces only that. Attached files and images are counted (a
            // text file's note holds the whole file).
            let note = |t: &str| t.starts_with("[Attached ");
            let typed: Vec<&str> = w
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } if !note(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            // An image saved to disk has a note beside its picture: one attachment.
            let attachments = w
                .content
                .iter()
                .filter(|p| match p {
                    ContentPart::Text { text } => {
                        note(text) && !text.starts_with("[Attached image:")
                    }
                    ContentPart::Image { .. } => true,
                })
                .count();
            QueuedMessage {
                text: typed.join("\n"),
                steer: q == Queue::Steer,
                interrupt: q == Queue::Now,
                attachments: attachments as u32,
            }
        })
        .collect()
}

fn no_longer_queued() -> ApiError {
    ApiError::new(ErrorCode::NotFound, "that message is no longer queued")
}

/// "Send now": stop the running turn so that `w` starts the next one (the
/// end of `run` hands over). When idle, the session is marked running and
/// `w` comes back: start a turn with it.
fn interrupt(st: &mut RtState, w: Waiting) -> Option<Waiting> {
    if !st.running {
        st.running = true;
        return Some(w);
    }
    if st.cancel.is_some() && st.depth == 0 {
        st.send_now.push(w);
        // Cancellation is applied only after update_queue commits admission.
    } else {
        // A subagent's turns belong to the agent that started it; or the
        // turn is only starting. Either way it reads this at its next step.
        st.steer.push(w);
    }
    None
}

/// `m:` followed by a short lowercase id, e.g. `m:laurel-beard`.
fn is_mosaic_avatar(a: &str) -> bool {
    a.strip_prefix("m:").is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 30
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// A message waiting for a busy agent: the user's own (`author: None`), or a
/// note from the harness such as a subagent report.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Waiting {
    id: String,
    content: Vec<ContentPart>,
    author: Option<String>,
    /// This item's admission, never refreshed merely by finalization.
    /// Old queues had no marks: zero is safe after any recorded Stop.
    #[serde(default)]
    stops: StopMarks,
}

impl Waiting {
    fn user(content: Vec<ContentPart>, stops: StopMarks) -> Self {
        Self {
            id: new_id(),
            content,
            author: None,
            stops,
        }
    }
}

/// A message to a bot, in its conversation and ready to be worked on
/// (`start_bot_message`).
struct BotMessage {
    dm: Arc<Rt>,
    dm_id: String,
    /// Persisted restart boundary only; answers use the captured terminal ID.
    from_seq: u64,
    to: Bot,
    receipt: HandoffReceipt,
}

fn apply_delta(s: &mut StreamingMessage, ev: &LlmEvent) -> StreamDelta {
    let ensure = |s: &mut StreamingMessage, index: usize, make: &dyn Fn() -> AssistantPart| {
        while s.content.len() <= index {
            s.content.push(AssistantPart::Text {
                text: String::new(),
            });
        }
        let mismatch = !matches!(
            (&s.content[index], make()),
            (AssistantPart::Text { .. }, AssistantPart::Text { .. })
                | (
                    AssistantPart::Thinking { .. },
                    AssistantPart::Thinking { .. }
                )
                | (
                    AssistantPart::ToolCall { .. },
                    AssistantPart::ToolCall { .. }
                )
        );
        if mismatch {
            s.content[index] = make();
        }
    };
    match ev {
        LlmEvent::Text { index, text } => {
            ensure(s, *index as usize, &|| AssistantPart::Text {
                text: String::new(),
            });
            if let AssistantPart::Text { text: t } = &mut s.content[*index as usize] {
                t.push_str(text);
            }
            StreamDelta::Text {
                index: *index,
                text: text.clone(),
            }
        }
        LlmEvent::Thinking { index, text } => {
            ensure(s, *index as usize, &|| AssistantPart::Thinking {
                text: String::new(),
                opaque: None,
            });
            if let AssistantPart::Thinking { text: t, .. } = &mut s.content[*index as usize] {
                t.push_str(text);
            }
            StreamDelta::Thinking {
                index: *index,
                text: text.clone(),
            }
        }
        LlmEvent::ToolCall {
            index,
            id,
            name,
            arguments,
        } => {
            ensure(s, *index as usize, &|| AssistantPart::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: Value::String(String::new()),
            });
            if let AssistantPart::ToolCall { arguments: a, .. } = &mut s.content[*index as usize]
                && let Value::String(acc) = a
            {
                acc.push_str(arguments);
            }
            StreamDelta::ToolCall {
                index: *index,
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }
        }
    }
}

fn list_dir(path: &str) -> Result<Vec<DirEntry>> {
    let p = if path.trim().is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        crate::util::expand_home(path)
    };
    let mut out = Vec::new();
    for e in std::fs::read_dir(&p).with_context(|| format!("reading {}", p.display()))? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(DirEntry {
            path: e.path().display().to_string(),
            name,
            is_dir,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// Why `name` can't be a folder's name, in words for the person who typed it.
fn folder_name_problem(name: &str) -> Option<&'static str> {
    // Directionality marks and zero-width characters make one name look like another.
    let hidden = |c: char| matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    if name.is_empty() {
        Some("Give the folder a name.")
    } else if name == "." || name == ".." {
        Some("That isn't a name a folder can have.")
    } else if name.contains(['/', '\\']) {
        Some(
            "A folder's name can't hold a slash. To make a folder inside another, open that one first.",
        )
    } else if name.chars().any(|c| c.is_control() || hidden(c)) {
        Some("A folder's name can't hold invisible or control characters.")
    } else if name.len() > 255 {
        Some("That name is too long; use a shorter one.")
    } else {
        None
    }
}

/// "New folder": one folder, `name`, made inside the existing folder `parent`. The name is a plain
/// name (never a path), the folder is made with a single `mkdir` (so a symbolic link or anything
/// else of that name already there is refused, never followed) and not inside Tessera's own data
/// or settings, which agents never get.
fn create_dir(
    parent: &str,
    name: &str,
    protected: &crate::tools::Protected,
) -> Result<DirEntry, ApiError> {
    let name = name.trim();
    if let Some(why) = folder_name_problem(name) {
        return Err(bad(why));
    }
    let raw = parent.trim();
    let dir = crate::util::expand_home(raw);
    if raw.contains('\0') || !dir.is_absolute() {
        return Err(bad(
            "Choose where to make it first (a full path, like /home/you).",
        ));
    }
    let canon = dir
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| bad(format!("There is no folder {raw}.")))?;
    if protected
        .canonical()
        .roots
        .iter()
        .any(|r| canon.starts_with(r))
    {
        return Err(bad(
            "That is inside Tessera's own data or settings; folders for agents go elsewhere.",
        ));
    }
    let made = canon.join(name);
    std::fs::create_dir(&made).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => {
            let what = match std::fs::symlink_metadata(&made) {
                Ok(m) if m.is_dir() => "A folder",
                Ok(m) if m.file_type().is_symlink() => "A link",
                _ => "A file",
            };
            ApiError::new(
                ErrorCode::Conflict,
                format!("{what} called {name} is already in {raw}."),
            )
        }
        std::io::ErrorKind::PermissionDenied => bad(format!("You can't make folders in {raw}.")),
        _ => bad(format!("Couldn't make the folder: {e}")),
    })?;
    Ok(DirEntry {
        name: name.to_string(),
        path: made.display().to_string(),
        is_dir: true,
    })
}

/// Rounds of `agent.max_steps` a single turn may run (with a progress check
/// between rounds) before it pauses for the user.
const LONG_TURN_ROUNDS: u32 = 4;

/// The longest a turn waits for a usage limit to reset (a weekly limit is
/// for the user to decide about).
const LIMIT_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// A pause before a new try: "8s", "2 min".
fn pause_text(d: std::time::Duration) -> String {
    if d.as_secs() < 100 {
        format!("{}s", d.as_secs())
    } else {
        human_wait(d)
    }
}

/// "2 h 5 min", "35 min".
fn human_wait(d: std::time::Duration) -> String {
    let m = d.as_secs().div_ceil(60).max(1);
    match m {
        0..=59 => format!("{m} min"),
        _ => format!("{} h {} min", m / 60, m % 60),
    }
}

/// Tool-facing view of the engine.
fn actor_permission(parent: PermissionMode, read_only: bool) -> PermissionMode {
    if read_only {
        PermissionMode::ReadOnly
    } else {
        parent
    }
}

struct Host(Arc<Inner>, bool);

impl ToolHost for Host {
    fn store(&self) -> &Store {
        &self.0.store
    }
    fn memory(&self, session_id: &str) -> Arc<Memory> {
        let bot = self
            .0
            .rt(session_id)
            .ok()
            .and_then(|rt| self.0.speaker_bot(&rt.st.lock()));
        self.0.memory_for(bot.as_ref())
    }
    fn session_bot(&self, session_id: &str) -> Option<String> {
        self.0
            .rt(session_id)
            .ok()
            .and_then(|rt| self.0.speaker_id(&rt.st.lock()))
    }
    fn message_bot(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(self.0.clone().message_bot(
            caller.to_string(),
            target.to_string(),
            message.to_string(),
            cancel,
        ))
    }
    fn may_hand_off_in_background(&self, session_id: &str) -> bool {
        self.0.may_hand_off_in_background(session_id)
    }
    fn hand_to_bot(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        call_id: &str,
    ) -> Result<crate::tools::HandedOff, String> {
        self.0
            .hand_to_bot(caller, target, message.to_string(), call_id)
    }
    fn skills(&self, cwd: &Path) -> Vec<crate::skills::Skill> {
        self.0.skills_for(cwd)
    }
    fn skill_used(&self, name: &str) {
        self.0.count_skill_use(name)
    }
    fn skill_evidence(&self, session_id: &str) -> crate::skills::Evidence {
        self.0.skill_evidence(session_id)
    }
    fn skills_made_this_turn(&self, session_id: &str) -> usize {
        self.0.skills_made_this_turn(session_id)
    }
    fn user_skills_dir(&self) -> PathBuf {
        self.0.paths.skills_dir()
    }
    fn agent_defs(&self, cwd: &Path) -> Vec<crate::agents::AgentDef> {
        self.0.agent_defs(cwd)
    }
    fn user_agents_dir(&self) -> PathBuf {
        self.0.paths.agents_dir()
    }
    fn helper_limits(&self, session_id: &str) -> (u32, u32) {
        self.0.helper_budget(session_id)
    }
    fn helper_network(&self, session_id: &str) -> bool {
        let Ok(rt) = self.0.rt(session_id) else {
            return false;
        };
        let (agent, cwd) = {
            let st = rt.st.lock();
            let agent = st.internal.subagent.as_ref().and_then(|s| s.agent.clone());
            (agent, st.info.cwd.clone())
        };
        agent.is_some_and(|name| {
            self.0
                .agent_defs(Path::new(&cwd))
                .into_iter()
                .any(|d| d.name == name && d.network)
        })
    }
    fn ledger(&self, session_id: &str) -> Option<TaskLedger> {
        self.0
            .rt(session_id)
            .ok()
            .and_then(|rt| rt.st.lock().info.task.clone())
    }
    fn set_ledger(&self, session_id: &str, ledger: TaskLedger) -> Result<()> {
        let rt = self.0.rt(session_id).map_err(|e| anyhow!(e.message))?;
        {
            let mut st = rt.st.lock();
            let RtState { info, internal, .. } = &mut *st;
            crate::claude_code::reconcile_tasks(
                info.task.as_ref().map_or(&[], |task| task.plan.as_slice()),
                &mut internal.claude_tasks,
                &ledger.plan,
            );
            info.task = Some(ledger);
        }
        self.0.save(&rt);
        Ok(())
    }
    fn run_subagents(
        &self,
        parent: &str,
        call_id: &str,
        tasks: Vec<SubTask>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Vec<SubResult>> {
        Box::pin(
            self.0
                .clone()
                .subagents(parent.to_string(), call_id.to_string(), tasks, cancel),
        )
    }
    fn spawn_subagents(
        &self,
        parent: &str,
        call_id: &str,
        tasks: Vec<SubTask>,
    ) -> Result<Vec<crate::tools::meta::Started>, String> {
        self.0.spawn_background(parent, call_id, tasks)
    }
    fn subagent_status(&self, parent: &str) -> String {
        self.0.agents_status(parent)
    }
    fn stop_subagents(&self, parent: &str, name: Option<&str>) -> usize {
        self.0.stop_background(parent, name)
    }
    fn subagent_report(&self, parent: &str, which: &str) -> Result<String, String> {
        self.0.subagent_report(parent, which)
    }
    fn subagent_control(
        &self,
        parent: &str,
        action: crate::tools::meta::SubAction,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(self.0.clone().subagent_control(parent.to_string(), action))
    }
    fn board_show(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: BoardContent,
        tile: bool,
    ) -> Result<Whiteboard, String> {
        self.0.show_board(session_id, id, title, content, tile)
    }
    fn board_show_on(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: BoardContent,
        tile: bool,
        host: Option<Option<String>>,
    ) -> Result<Whiteboard, String> {
        self.0
            .show_board_on(session_id, id, title, content, tile, host)
    }
    fn board_ask(
        &self,
        session_id: &str,
        id: Option<String>,
        title: Option<String>,
        prompt: String,
        input: AskInput,
        tile: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<AskAnswer, String>> {
        Box::pin(self.0.clone().ask_board(
            session_id.to_string(),
            id,
            title,
            prompt,
            input,
            tile,
            cancel,
        ))
    }
    fn board_list(&self, session_id: &str) -> Vec<crate::tools::board::BoardView> {
        self.0.board_views(session_id)
    }
    fn board_local_get(
        &self,
        session_id: &str,
        id: &str,
    ) -> BoxFuture<'static, Result<crate::tools::board::LocalPage, String>> {
        let (inner, sid, id) = (self.0.clone(), session_id.to_string(), id.to_string());
        Box::pin(async move { inner.board_local_get(&sid, &id).await })
    }
    fn board_look(
        &self,
        session_id: &str,
        id: &str,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SnapshotImage, String>> {
        let (inner, sid, id) = (self.0.clone(), session_id.to_string(), id.to_string());
        Box::pin(async move { inner.look_board(&sid, &id, cancel).await })
    }
    fn notes(&self, session_id: &str) -> Vec<NoteInfo> {
        self.0
            .store
            .notes()
            .unwrap_or_default()
            .iter()
            .filter(|n| self.0.note_visible(session_id, n))
            .map(notes::note_info)
            .collect()
    }
    fn note(&self, session_id: &str, id: &str) -> Option<Note> {
        self.0
            .store
            .note(id)
            .ok()
            .flatten()
            .filter(|n| self.0.note_visible(session_id, n))
    }
    fn may_recall(&self, session_id: &str, other: &str) -> bool {
        self.0.may_recall(session_id, other)
    }
    fn recall_refusal(&self, session_id: &str, other: &str) -> Option<String> {
        self.0.recall_refusal(session_id, other)
    }
    fn note_save(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: &str,
        content: String,
        base_version: Option<u64>,
    ) -> Result<Note, String> {
        if self.permission(session_id) == PermissionMode::ReadOnly {
            return Err("This actor is read-only; the note was not changed.".into());
        }
        if let Some(id) = id
            && self.note(session_id, id).is_none()
        {
            return Err(format!("no note `{id}`"));
        }
        self.0
            .save_note(id, title, content, base_version, Some(session_id), false)
            .map_err(|e| e.message)
    }
    fn note_show(&self, session_id: &str, id: &str) -> Result<(), String> {
        if self.note(session_id, id).is_none() {
            return Err(format!("no note `{id}`"));
        }
        self.0.show_note(session_id, id)
    }
    fn note_picture(
        &self,
        session_id: &str,
        note_id: &str,
        name: &str,
    ) -> Option<(&'static str, Vec<u8>)> {
        let n = self.note(session_id, note_id)?;
        notes::asset_refs(&n.content)
            .any(|r| r == name)
            .then(|| self.0.note_asset_bytes(name))
            .flatten()
    }
    fn note_picture_put(&self, session_id: &str, bytes: &[u8]) -> Result<String, String> {
        if self.permission(session_id) == PermissionMode::ReadOnly {
            return Err("This actor is read-only; the picture was not saved.".into());
        }
        self.0
            .put_note_asset(bytes)
            .map(|a| a.name)
            .map_err(|e| e.message)
    }
    fn sees_images(&self, session_id: &str) -> bool {
        self.0.rt(session_id).is_ok_and(|rt| {
            let model = self.0.effective_model(&rt.st.lock());
            self.0.providers.sees_images(&model)
        })
    }
    fn board_delete(&self, session_id: &str, id: &str) -> Result<(), String> {
        self.0.own_board_pub(session_id, id)?;
        self.0.delete_board(id);
        Ok(())
    }
    fn board_tile(&self, session_id: &str, id: &str) -> Result<(), String> {
        self.0.board_tile(session_id, id)
    }
    fn board_save_signature(&self, session_id: &str, png: &str) -> Result<PathBuf, String> {
        self.0.save_signature(session_id, png)
    }
    fn jobs(&self) -> Vec<Job> {
        self.0.store.jobs().unwrap_or_default()
    }
    fn save_job(&self, job: Job) -> Result<Job> {
        self.0.save_job(job)
    }
    fn delete_job(&self, id: &str) -> Result<bool> {
        let gate = self.0.job_gate(id);
        let _admission = gate.lock();
        let ok = self.0.store.delete_job(id)?;
        if ok {
            let _ = self.0.store.forget_job_runs(id);
            let _ = self.0.events.send(Event::JobRemoved { id: id.to_string() });
        }
        Ok(ok)
    }
    fn end_job_schedule(&self, session_id: &str, reason: &str) -> Result<String, String> {
        self.0.end_own_schedule(session_id, reason)
    }
    fn web_config(&self) -> crate::config::WebConfig {
        self.0.config.read().web.clone()
    }
    fn search_headers(&self) -> Vec<(String, String)> {
        self.0.auth.search_headers().into_iter().collect()
    }
    fn http(&self) -> reqwest::Client {
        self.0.providers.http().clone()
    }
    fn secrets(&self) -> Vec<String> {
        self.0.redaction_secrets()
    }
    fn secret_env(
        &self,
        session_id: &str,
        command: &str,
        place: crate::auth::SecretPlace,
    ) -> crate::auth::SecretUse {
        self.0.secret_env(session_id, command, place)
    }
    fn mark_secrets_used(&self, names: &[String]) {
        self.0.auth.mark_used(names)
    }
    fn bot_has_own_secrets(&self, session_id: &str, bot: &str) -> bool {
        self.0.bot_has_own_secrets(session_id, bot)
    }
    fn bot_handoff(&self, session_id: &str) -> BotHandoff {
        // Bots reach bots by their own lists; the setting is for chats.
        if self
            .0
            .rt(session_id)
            .is_ok_and(|rt| self.0.speaker_bot(&rt.st.lock()).is_some())
        {
            return BotHandoff::On;
        }
        self.0.config.read().security.bot_handoff
    }
    fn protected_paths(&self) -> crate::tools::Protected {
        self.0.protected_paths()
    }
    fn unconfined_commands_ask(&self) -> bool {
        self.0.config.read().security.sandbox == crate::config::SandboxMode::Auto
            && !crate::sandbox::support().complete()
    }
    fn sandbox(&self) -> Option<(crate::sandbox::Spec, bool)> {
        self.0.sandbox_spec()
    }
    fn session_sandbox(&self, session_id: &str) -> Option<(crate::sandbox::Spec, bool)> {
        self.0.session_sandbox(session_id)
    }
    fn other_chats_upload(&self, session_id: &str, path: &Path) -> bool {
        self.0.other_chats_upload(session_id, path)
    }
    fn write_access(&self) -> crate::tools::WriteAccess {
        self.0.write_access()
    }
    fn authorized_cwd(&self, session_id: &str) -> Result<Option<PathBuf>, String> {
        let rt = self.0.rt(session_id).map_err(|e| e.message)?;
        self.0.helper_authorized_cwd(&rt)
    }
    fn authorized_cwd_scope(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::tools::file_object::DirectoryScope>, String> {
        let rt = self.0.rt(session_id).map_err(|e| e.message)?;
        self.0.helper_authorized_scope(&rt)
    }
    fn process_directory(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<crate::tools::file_object::ProcessDirectory, String> {
        let rt = self.0.rt(session_id).map_err(|e| e.message)?;
        self.0.process_directory(&rt, cwd)
    }
    fn check_chat_secret(
        &self,
        session_id: &str,
        secret: &crate::tools::secrets::ChatSecret,
    ) -> Result<crate::config::SecretSaving, String> {
        self.0.check_chat_secret(session_id, secret)
    }
    fn save_chat_secret(
        &self,
        session_id: &str,
        secret: crate::tools::secrets::ChatSecret,
    ) -> Result<String, String> {
        self.0.save_chat_secret(session_id, secret)
    }
    fn list_secrets(&self, session_id: &str) -> String {
        let block = self
            .0
            .rt(session_id)
            .ok()
            .and_then(|rt| self.0.secrets_prompt(&rt.st.lock()));
        block.unwrap_or_else(|| "No saved passwords or keys this chat may use.".into())
    }
    fn settings(&self, session_id: &str) -> Result<String, String> {
        let mut text = self.0.settings_text();
        if let Some(chat) = self.0.chat_helper_text(session_id) {
            text.push('\n');
            text.push_str(&chat);
        }
        Ok(text)
    }
    fn change_setting(
        &self,
        session_id: &str,
        key: &[String],
        value: &Value,
    ) -> Result<String, String> {
        if self.permission(session_id) == PermissionMode::ReadOnly {
            return Err("This actor is read-only; settings were not changed.".into());
        }
        if key.first().is_some_and(|k| k == "chat") {
            return self.0.change_chat_setting(session_id, key, value);
        }
        self.0.change_setting(key, value)
    }
    fn bash_timeout(&self) -> u64 {
        self.0.config.read().agent.bash_timeout
    }
    fn pass_env(&self) -> Vec<String> {
        self.0.config.read().agent.pass_env.clone()
    }
    fn quick_checks(&self) -> bool {
        self.0.config.read().agent.quick_checks
    }
    fn max_tool_output(&self) -> usize {
        self.0.config.read().context.max_tool_output
    }
    fn tool_output_limit(&self, session_id: &str) -> usize {
        self.0.tool_output_limit(session_id)
    }
    fn utility_parallel(&self, session_id: &str) -> bool {
        self.0.utility_parallel(session_id)
    }
    fn more_tools(&self, session_id: &str) -> Option<bool> {
        self.0.enable_more_tools(session_id)
    }
    fn permission(&self, session_id: &str) -> PermissionMode {
        actor_permission(
            self.0
                .rt(session_id)
                .map(|rt| self.0.effective_permission(&rt.st.lock()))
                .unwrap_or(PermissionMode::ReadOnly),
            self.1,
        )
    }
    fn bot_permission(&self, bot: &str) -> Option<PermissionMode> {
        self.0.find_bot(bot).map(|b| b.permission)
    }
    fn search_key(&self, provider: &str) -> Option<String> {
        match self.0.auth.get(&crate::tools::web::key_id(provider)) {
            Some(Credential::ApiKey { key }) if !key.trim().is_empty() => Some(key),
            _ => None,
        }
    }
    fn utility_complete(
        &self,
        session_id: &str,
        system: String,
        prompt: String,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>> {
        let inner = self.0.clone();
        let sid = session_id.to_string();
        Box::pin(async move {
            inner
                .utility_complete(&sid, &system, &prompt, &cancel)
                .await
        })
    }
    fn lsp(&self) -> Option<Arc<crate::lsp::LspManager>> {
        self.0.lsp.read().clone()
    }
    fn file_cache(&self) -> Option<&crate::tools::filecache::FileCache> {
        Some(&self.0.files)
    }
    fn turn(&self, session_id: &str) -> u32 {
        self.0
            .rt(session_id)
            .map(|rt| rt.st.lock().internal.user_turns)
            .unwrap_or(0)
    }
    fn browser(&self) -> Option<crate::browser::Browsers> {
        Some(self.0.browser.clone())
    }
    fn web_board(
        &self,
        session_id: &str,
        title: Option<&str>,
        url: &str,
        host: Option<String>,
    ) -> Result<Whiteboard, String> {
        self.0.web_board(session_id, title, url, host)
    }
    fn approve(&self, req: crate::tools::Approve) -> BoxFuture<'static, bool> {
        let answer = self.0.clone().approve_now(req);
        Box::pin(async move { answer.await.is_ok() })
    }
    fn approve_reason(&self, req: crate::tools::Approve) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(self.0.clone().approve_now(req))
    }
    fn notify(
        &self,
        session_id: &str,
        n: crate::tools::notify::Outgoing,
    ) -> Result<crate::tools::notify::Sent, String> {
        self.0.tool_notify(session_id, n)
    }
    fn cancel_notification(&self, session_id: &str, id: u64) -> Result<(), String> {
        self.0.cancel_scheduled_notification(session_id, id)
    }
    fn share_file(
        &self,
        session_id: &str,
        path: &Path,
        file: &std::fs::File,
        name: &str,
        held_ok: bool,
    ) -> Result<SharedFile, crate::tools::share::Refusal> {
        self.0.share_file(session_id, path, file, name, held_ok)
    }
    fn machines(&self) -> Vec<MachineInfo> {
        self.0.machine_list()
    }
    fn machine_call(
        &self,
        call: crate::tools::machines::MachineCall,
    ) -> BoxFuture<'static, Result<NodeResult, String>> {
        Box::pin(self.0.clone().machine_call(call))
    }
    fn machine_access(
        &self,
        session_id: &str,
        machine: &MachineInfo,
    ) -> Result<MachineAccess, String> {
        self.0
            .machine_access(session_id, &machine.id, &machine.scope)
    }
    fn bot_more_machines(&self, session_id: &str, bot: &str) -> Option<String> {
        self.0.bot_more_machines(session_id, bot)
    }
    fn machine_refused(&self, session_id: &str, machine: &str, action: NodeAction, reason: &str) {
        self.0.machine_refused(session_id, machine, action, reason)
    }
    fn stopped_since_turn(&self, session_id: &str) -> Option<String> {
        let rt = self.0.rt(session_id).ok()?;
        (!self.0.turn_running_here(&rt)).then(|| STOPPED_SINCE.to_string())
    }
}

#[cfg(test)]
mod request_layout_tests {
    use super::{Engine, Request};
    use crate::browser::Viewer;

    // Infer opaque future layouts from UNCALLED factories: constructing a
    // regressed future to measure it could itself overflow the test stack.
    fn handle_bytes<'a, R>(_: impl FnOnce(&'a Engine, Request, Option<&'a str>) -> R) -> usize {
        std::mem::size_of::<R>()
    }
    fn viewer_bytes<'a, R>(
        _: impl FnOnce(&'a Engine, Request, Option<&'a str>, Option<&'a Viewer>) -> R,
    ) -> usize {
        std::mem::size_of::<R>()
    }

    #[test]
    fn dispatcher_future_layouts_fit_a_small_stack_budget() {
        const BUDGET: usize = 16 * 1024;
        let ordinary = handle_bytes(Engine::handle);
        let viewer = viewer_bytes(Engine::handle_as);
        assert!(
            ordinary <= BUDGET,
            "ordinary request future is {ordinary} bytes"
        );
        assert!(viewer <= BUDGET, "viewer request future is {viewer} bytes");
    }
}
#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn goal_notes_read_as_the_goal_in_previews() {
        let text = |t: &str| vec![ContentPart::text(t)];
        assert_eq!(
            user_preview(&text(&format!("{GOAL_SET}\nPlan the garden"))).as_deref(),
            Some("Goal: Plan the garden")
        );
        assert_eq!(user_preview(&text(GOAL_CLEARED)), None);
        assert_eq!(user_preview(&text("hello")).as_deref(), Some("hello"));
    }
}
