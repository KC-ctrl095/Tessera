//! The server-side browser behind web view tiles and the `browser` tool.
//!
//! One Chromium-family browser per server, driven over CDP through
//! `--remote-debugging-pipe` (no port: nothing else on the machine, the
//! agent's sandboxed commands included, can reach it). It starts on first
//! use with a persistent profile in the data directory (`browser/`, 0700),
//! headless unless `[browser] headless = false`, and stops after
//! `idle_minutes` with nobody watching and no agent use; logins persist.
//!
//! Every open web view board (`Url`, or `Local` = `http://127.0.0.1:<port>`)
//! has a live tab. Devices attach with their tile's size and get JPEG
//! screencast frames on their own connection only, within a window that
//! follows their connection (see `tab.rs`), so a slow phone never queues
//! stale pictures, a far one isn't held to one frame per round trip, and an
//! idle page costs nothing. Input, navigation and zoom go to the same tab,
//! so every device and the agent share one page. The agent's `browser` tool
//! also opens headless tabs of its own (see `agent.rs`).
//!
//! A web view may also run on one of the user's machines ("Web views
//! here", see `hosts.rs` and `crate::node::browser`): the same code drives
//! that machine's browser through its connection instead of a pipe.

mod agent;
pub(crate) mod agent_upload;
pub(crate) mod cdp;
mod hosts;
pub(crate) mod keys;
pub(crate) mod page_actions;
pub(crate) mod process;
mod remote;
mod render;
mod tab;
mod tabs;
pub(crate) mod upload;
mod upload_stream;
// Only Unix machines ask their server to close pages (see `node::browser`).
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use remote::MEMORY_HIGH;
#[cfg(test)]
mod r5_tests;
#[cfg(test)]
pub(crate) use tab::NO_KEYS_JS;
pub(crate) use tab::{as_agent, on_run};

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tessera_proto::{ApiError, BrowserQuality, ErrorCode, Event, NoticeLevel, Request};
use tokio::sync::{broadcast, mpsc};

pub use agent::{AgentTab, Changes, ElementInfo, FormInfo, Shot, Snapshot};
use cdp::{Cdp, CdpEvent};
pub use hosts::{ActingAs, Browsers};
pub use remote::Remote;
pub use tab::Tab;
use tab::{Takes, Viewport};

/// `[browser]` in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BrowserConfig {
    /// Web view tiles and the `browser` tool use a browser on this server.
    pub enabled: bool,
    /// The browser to run (default: the first of chromium, google-chrome,
    /// brave or microsoft-edge found).
    pub path: Option<String>,
    /// Run it without a window. `false` shows its windows on the server's
    /// desktop (e.g. to solve something by hand there).
    pub headless: bool,
    /// Stop it after this long with no tile showing it and no agent use.
    pub idle_minutes: f64,
    /// Tabs open at once (web view tiles and agents' own).
    pub max_tabs: usize,
    /// Pause a page nobody watches or uses after this many seconds (no
    /// scripts or timers until it is looked at again; 0 = never).
    pub freeze_seconds: f64,
    /// The most pixels per CSS pixel moving frames are drawn at, 1–3: 2
    /// keeps text sharp on phones and dense screens. Still pictures match
    /// the device.
    pub scale: f64,
    /// How sharp moving pictures are: `auto` (each device as sharp as this
    /// server draws smoothly, measured), `sharp` (the device's own
    /// sharpness, up to `scale`) or `smooth` (normal resolution).
    pub quality: BrowserQuality,
    /// Headless: each tab renders at its own scale on a virtual screen
    /// (`false`: one scale for every tab, picked the same way).
    pub virtual_screens: bool,
    /// How fast this server draws, as sharp frames a second (1–60), instead
    /// of measuring it when the browser first starts.
    pub speed: Option<f64>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        // A small machine doesn't run one unless asked to (a browser takes
        // 350–430 MB), and fewer tabs at once (see `budget.rs`).
        let budget = crate::budget::get();
        Self {
            enabled: budget.web_views,
            path: None,
            headless: true,
            idle_minutes: 10.0,
            max_tabs: budget.max_tabs,
            freeze_seconds: 60.0,
            scale: 2.0,
            quality: BrowserQuality::Auto,
            virtual_screens: true,
            speed: None,
        }
    }
}

/// Whether a browser the server can run is installed (`path`: the
/// `[browser] path` setting).
pub fn installed(path: Option<&str>) -> bool {
    process::find(path).is_some()
}

/// What the browser needs from the engine.
pub trait Host: Send + Sync {
    fn browser_config(&self) -> BrowserConfig;
    /// Where a web view board points, or why it has no tab.
    fn board_target(&self, board_id: &str) -> Result<BoardTarget, String>;
    /// The tab of a board moved to `url`: remember it (no version bump).
    fn board_navigated(&self, board_id: &str, url: &str);
    /// Save an ordinary new tab alongside this board, on the same browser host.
    fn linked_board(
        &self,
        _board_id: &str,
        _url: &str,
    ) -> Result<tessera_proto::Whiteboard, String> {
        Err("new tabs are not available here".into())
    }
    /// Board-ID groups survive browser/server restarts; no page content is stored.
    fn tab_group(&self, _id: &str) -> Vec<String> {
        Vec::new()
    }
    fn save_tab_group(&self, _ids: &[String]) {}
    /// The port Tessera itself listens on (pages may not reach it).
    fn own_port(&self) -> Option<u16>;
    /// A download finished: offer it to the user (the tile and the chat).
    /// False when it wasn't (then a notice says where it is).
    fn downloaded(&self, _done: &Downloaded) -> bool {
        false
    }

    // ---- web views on machines ("Web views here") ----------------------

    /// The way to machine `id`'s browser, if `id` is a machine.
    fn remote(&self, _machine: &str) -> Option<Arc<dyn Remote>> {
        None
    }
    /// May web views run on machine `id` for `session` now (online, not
    /// paused, web views on there, and the session may use it)? `Err` says
    /// why not, in plain words. `None` session: the user's own devices.
    fn machine_ok(&self, _session: Option<&str>, _machine: &str) -> Result<(), String> {
        Err("there are no machines here".into())
    }
    /// Where `session`'s new web views go when nothing else says: the
    /// machine of the device the user is using the chat on, or this server.
    fn placement(&self, _session: &str) -> Option<String> {
        None
    }
    /// A machine's id from its name or id.
    fn find_machine(&self, _name: &str) -> Option<String> {
        None
    }
    /// Who acts for `session` (a machine checks and logs it).
    fn acting_as(&self, _session: &str) -> ActingAs {
        ActingAs::default()
    }
    /// The count of the user's Stops on `machine` that `session`'s latest
    /// turn was accepted under (the machine refuses its actions after a
    /// later Stop there). `None`: unknown (the machine doesn't check it).
    fn turn_stops(&self, _session: &str, _machine: &str) -> Option<u32> {
        None
    }
}

/// A finished download, and where it came from.
pub struct Downloaded {
    pub path: PathBuf,
    /// The web view board of the tab, if it has one.
    pub board: Option<String>,
    /// The chat the tab belongs to.
    pub session: Option<String>,
}

pub struct BoardTarget {
    pub url: String,
    /// The chat the board belongs to (notices about its tab go there).
    pub session_id: String,
    /// The machine whose browser shows it (none: this server's).
    pub host: Option<String>,
}

/// Measured use, for tests and diagnostics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub running: bool,
    pub pid: Option<u32>,
    pub tabs: usize,
    pub live_tabs: usize,
    pub viewers: usize,
    pub frames: u64,
    pub frame_bytes: u64,
    pub launches: u64,
}

#[derive(Clone)]
pub struct Browser(Arc<Shared>);

pub(crate) struct Shared {
    host: Weak<dyn Host>,
    events: broadcast::Sender<Event>,
    /// Its profile (here) or, for a machine's browser, where the server
    /// keeps what it measured of it.
    profile: PathBuf,
    downloads: PathBuf,
    /// A machine's browser: the way to it.
    remote: Option<Arc<remote::RemoteHost>>,
    /// Only published processes; never held across startup awaits.
    run: tokio::sync::Mutex<Option<Proc>>,
    starting: Mutex<Option<Arc<Startup>>>,
    /// Cleanup waits outside the start lock, but gates profile reuse.
    cleaning: RwLock<Option<tokio_util::sync::CancellationToken>>,
    profile_gate: Arc<tokio::sync::Mutex<()>>,
    /// Serializes admission through setup/publication of each Chrome page.
    page_admission: Arc<tokio::sync::Mutex<()>>,
    #[cfg(test)]
    measurement_times: Mutex<VecDeque<Instant>>,
    live: RwLock<Option<Arc<Live>>>,
    tabs: Mutex<HashMap<String, Arc<Tab>>>,
    /// The connections watching (shared by every host's browser).
    pub(crate) hub: Arc<hosts::Hub>,
    last_use: Mutex<Instant>,
    /// Unexpected exits and memory stops, shared by automatic and explicit starts.
    recovery: Mutex<Recovery>,
    /// Chats warned during this pressure episode; clears after30s healthy.
    pressure_warned: Mutex<WarningEpisode>,
    stopping: AtomicBool,
    transfers: Mutex<HashMap<String, Download>>,
    uploads: Mutex<upload::Uploads>,
    groups: Mutex<HashMap<String, Vec<String>>>,
    /// New pages the browser attached for us, waiting to be claimed by the
    /// tab that made them: target id → (session id, when).
    attached: Mutex<HashMap<String, (String, Instant)>>,
    /// Agents' tabs closed for them (to make room), and why: an agent
    /// asking for one by its name is told instead of "no tab here".
    closed: Mutex<VecDeque<ClosedTab>>,
    pub(crate) frame_ids: AtomicU64,
    /// The scale every tab renders at when tabs can't have their own (f64
    /// bits; see `render.rs`).
    pub(crate) scale: AtomicU64,
    /// The sharpest frames may be (`BrowserConfig::scale`, f64 bits).
    pub(crate) cap: AtomicU64,
    /// Virtual screens, one per render scale tabs use.
    pub(crate) screens: Mutex<render::Screens>,
    /// How fast the server draws (ms per megapixel), once measured.
    pub(crate) speed: Mutex<Option<f64>>,
    generation: AtomicU64,
    pub(crate) frames: AtomicU64,
    pub(crate) frame_bytes: AtomicU64,
    launches: AtomicU64,
}

struct ClosedTab {
    key: String,
    owner: String,
    machine: Option<String>,
    why: String,
}

/// The running browser: a child process here, or a machine's.
// Windows' pipe handles make the local one bigger; there is one per browser.
#[cfg_attr(windows, allow(clippy::large_enum_variant))]
pub(crate) enum Proc {
    Local(process::Process),
    Remote(remote::Run),
    #[cfg(test)]
    Fixture {
        cdp: Arc<Cdp>,
        exit: tokio_util::sync::CancellationToken,
        killed: Option<tokio::sync::oneshot::Sender<()>>,
    },
}

const START_CANCELLED: &str = "browser startup was stopped";
#[derive(Default)]
struct Startup {
    cancel: tokio_util::sync::CancellationToken,
    done: tokio_util::sync::CancellationToken,
}
struct Starting {
    shared: Arc<Shared>,
    attempt: Arc<Startup>,
}
impl Drop for Starting {
    fn drop(&mut self) {
        let mut current = self.shared.starting.lock();
        if current
            .as_ref()
            .is_some_and(|a| Arc::ptr_eq(a, &self.attempt))
        {
            *current = None;
        }

        self.attempt.done.cancel();
    }
}

/// A cleanup gate opens only when both exit and recovery/notices are ready.
/// Dropping a caller early still leaves it waiting for the detached reaper.
struct Cleanup {
    done: tokio_util::sync::CancellationToken,
    exit: Option<tokio_util::sync::CancellationToken>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(exit) = self.exit.take().filter(|t| !t.is_cancelled()) {
            let done = self.done.clone();
            tokio::spawn(async move {
                exit.cancelled().await;
                done.cancel();
            });
        } else {
            self.done.cancel();
        }
    }
}

impl Proc {
    fn completion(&self) -> Option<tokio_util::sync::CancellationToken> {
        match self {
            Self::Local(p) => p.lease.as_ref().map(|l| l.done.clone()),
            Self::Remote(_) => None,
            #[cfg(test)]
            Self::Fixture { exit, .. } => Some(exit.clone()),
        }
    }

    fn cdp(&self) -> &Arc<Cdp> {
        match self {
            Proc::Local(p) => &p.cdp,
            Proc::Remote(r) => &r.cdp,
            #[cfg(test)]
            Proc::Fixture { cdp, .. } => cdp,
        }
    }
    fn pid(&self) -> Option<u32> {
        match self {
            Proc::Local(p) => p.pid(),
            Proc::Remote(_) => None,
            #[cfg(test)]
            Proc::Fixture { .. } => None,
        }
    }
    async fn stop(self) {
        match self {
            Proc::Local(p) => p.stop().await,
            Proc::Remote(r) => r.stop().await,
            #[cfg(test)]
            Proc::Fixture { cdp, killed, .. } => {
                cdp.close();
                if let Some(killed) = killed {
                    let _ = killed.send(());
                }
            }
        }
    }
    fn kill(&mut self) {
        match self {
            Proc::Local(p) => p.kill(),
            Proc::Remote(r) => r.kill(),
            #[cfg(test)]
            Proc::Fixture { cdp, killed, .. } => {
                cdp.close();
                if let Some(killed) = killed.take() {
                    let _ = killed.send(());
                }
            }
        }
    }
}

/// How a browser is started: a program here, or through a machine.
enum Launcher {
    Local(PathBuf),
    Remote(Arc<remote::RemoteHost>),
}

impl Launcher {
    /// Start it; also says what its measured speed depends on.
    async fn launch(
        &self,
        profile: &Path,
        headless: bool,
        scale: Option<f64>,
        profile_gate: Arc<tokio::sync::Mutex<()>>,
    ) -> Result<(Proc, mpsc::UnboundedReceiver<CdpEvent>, String), remote::StartupFailure> {
        match self {
            Launcher::Local(bin) => {
                let (owned_bin, profile) = (bin.clone(), profile.to_path_buf());
                // Acquire in the caller, not in a detached startup task.
                let guard = process::bounded_admission(profile_gate.lock_owned())
                    .await
                    .map_err(|message| remote::StartupFailure {
                        kind: remote::StartupKind::Refused,
                        message,
                    })?;
                let lease = process::ExitLease::new(
                    Some(guard),
                    tokio_util::sync::CancellationToken::new(),
                );
                // The launch future owns its Process before its first await;
                // cancellation drops that owner into the whole-bundle reaper.
                let (p, events) = process::launch(&owned_bin, &profile, headless, scale, lease)
                    .await
                    .map_err(remote::StartupFailure::unexpected)?;
                Ok((Proc::Local(p), events, render::key(bin)))
            }
            Launcher::Remote(r) => {
                let (run, events, started) = r.launch(scale).await?;
                Ok((
                    Proc::Remote(run),
                    events,
                    format!("machine|{}|{}", started.product, started.speed_key),
                ))
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Launcher::Local(bin) => bin.display().to_string(),
            Launcher::Remote(r) => format!("the browser on {}", r.link.name()),
        }
    }
}

/// The browser while it runs.
pub(crate) struct Live {
    pub cdp: Arc<Cdp>,
    pub generation: u64,
    pub headless: bool,
    pub major: String,
    started: Instant,
    /// Confirmed old-generation teardown, including detached process exit.
    stopped: tokio_util::sync::CancellationToken,
}

/// An agent's action allowed on one run of a machine's browser, which
/// closed since (the user may have stopped Tessera there).
pub(crate) const RESTARTED: &str = "the browser on that machine restarted since this was allowed (Tessera may have been stopped there), so this wasn't done; don't retry, tell the user";

/// What an agent is told about its page closed while the computer ran
/// short of memory.
const CLOSED_FOR_MEMORY: &str = "This tab's page was closed while the computer ran short of memory; it opened again at its last address, so anything typed into it and not yet submitted is gone.";

struct PageVictim {
    tab: Arc<Tab>,
}
struct PagePlan {
    victims: Vec<PageVictim>,
    low: bool,
    max: usize,
}
#[derive(Default)]
struct WarningEpisode {
    chats: std::collections::HashSet<Option<String>>,
    healthy_since: Option<tokio::time::Instant>,
}
impl WarningEpisode {
    fn sample(&mut self, low: bool) {
        if low {
            self.healthy_since = None;
        } else {
            let since = self
                .healthy_since
                .get_or_insert_with(tokio::time::Instant::now);
            if since.elapsed() >= Duration::from_secs(30) {
                self.chats.clear();
            }
        }
    }
}

const LOW_MEMORY: &str = "The server is low on memory; close a page or finish a task first";

#[cfg(test)]
fn require_pressure_room(spare: Option<&Arc<Tab>>, idle: Duration) -> Result<(), String> {
    if spare.is_some_and(|t| t.shed(idle, CLOSED_FOR_MEMORY)) {
        Ok(())
    } else {
        Err(LOW_MEMORY.into())
    }
}

/// Stops two minutes apart must not keep resetting a two-minute counter.
/// Three retries are automatic; after that all callers share a minute's
/// cooldown, and a viewer must reload. Ten quiet minutes reset the count.
#[derive(Default)]
struct Recovery {
    failures: u32,
    last_failure: Option<Instant>,
    retry_at: Option<Instant>,
    /// An intentional stop cancels scheduled recovery, not its cooldown.
    cancelled: u64,
}

impl Recovery {
    fn failed(&mut self, now: Instant, ran: Duration) -> (u32, Duration) {
        let quiet = Duration::from_secs(600);
        if ran >= quiet
            || self
                .last_failure
                .is_some_and(|t| now.duration_since(t) >= quiet)
        {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        self.last_failure = Some(now);
        let delay = match self.failures {
            1 => Duration::from_millis(500),
            2 => Duration::from_secs(1),
            3 => Duration::from_secs(2),
            _ => Duration::from_secs(60),
        };
        self.retry_at = Some(now + delay);
        (self.failures, delay)
    }

    fn wait(&self, now: Instant) -> Option<Duration> {
        self.retry_at.and_then(|t| t.checked_duration_since(now))
    }
}

/// A file a tab is downloading.
#[derive(Debug, Clone)]
pub struct Download {
    pub guid: String,
    pub tab: String,
    pub name: String,
    pub url: String,
    pub started: Instant,
    pub done: Option<PathBuf>,
    pub failed: bool,
}

/// One connection's view of the browsers: frames, focus and dialogs of the
/// boards it attached to (on any host) arrive on its own channel. Dropping
/// it detaches everything.
pub struct Viewer {
    pub(crate) id: u64,
    hub: Weak<hosts::Hub>,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        if let Some(h) = self.hub.upgrade() {
            h.drop_viewer(self.id);
        }
    }
}

/// Frames waiting per connection at most (one per attached board in
/// practice: the next frame waits for the ack).
const VIEWER_QUEUE: usize = 32;

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, m)
}

impl Browser {
    /// The server's own browser (`data_dir/browser`, downloads in
    /// `data_dir/downloads`).
    pub(crate) fn local(
        host: Weak<dyn Host>,
        events: broadcast::Sender<Event>,
        data_dir: &Path,
        hub: Arc<hosts::Hub>,
    ) -> Self {
        Self::make(
            host,
            events,
            data_dir.join("browser"),
            data_dir.join("downloads"),
            None,
            hub,
        )
    }

    /// A machine's browser; `state` is where the server keeps what it
    /// measured of it.
    pub(crate) fn remote(
        host: Weak<dyn Host>,
        events: broadcast::Sender<Event>,
        state: PathBuf,
        hub: Arc<hosts::Hub>,
        link: Arc<dyn Remote>,
    ) -> Self {
        let downloads = state.join("downloads");
        Self::make(
            host,
            events,
            state,
            downloads,
            Some(remote::RemoteHost::new(link)),
            hub,
        )
    }

    fn make(
        host: Weak<dyn Host>,
        events: broadcast::Sender<Event>,
        profile: PathBuf,
        downloads: PathBuf,
        remote: Option<Arc<remote::RemoteHost>>,
        hub: Arc<hosts::Hub>,
    ) -> Self {
        let b = Self(Arc::new(Shared {
            uploads: Mutex::new(upload::Uploads::new(profile.join("tessera-uploads"))),
            groups: Mutex::new(HashMap::new()),
            host,
            events,
            profile,
            downloads,
            remote,
            run: tokio::sync::Mutex::new(None),
            starting: Mutex::new(None),
            cleaning: RwLock::new(None),
            profile_gate: Arc::new(tokio::sync::Mutex::new(())),
            page_admission: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            measurement_times: Mutex::new(VecDeque::new()),
            live: RwLock::new(None),
            tabs: Mutex::new(HashMap::new()),
            hub: hub.clone(),
            last_use: Mutex::new(Instant::now()),
            recovery: Mutex::new(Recovery::default()),
            pressure_warned: Mutex::new(WarningEpisode::default()),
            stopping: AtomicBool::new(false),
            transfers: Mutex::new(HashMap::new()),
            attached: Mutex::new(HashMap::new()),
            closed: Mutex::new(VecDeque::new()),
            frame_ids: AtomicU64::new(0),
            scale: AtomicU64::new(2.0f64.to_bits()),
            cap: AtomicU64::new(2.0f64.to_bits()),
            screens: Mutex::new(render::Screens::default()),
            speed: Mutex::new(None),
            generation: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            frame_bytes: AtomicU64::new(0),
            launches: AtomicU64::new(0),
        }));
        hub.register(&b.0);
        b
    }

    /// Where downloads are saved (this server's browser).
    pub fn downloads_dir(&self) -> &Path {
        &self.0.downloads
    }

    /// The machine this browser runs on (none: this server).
    pub fn machine(&self) -> Option<String> {
        self.0.remote.as_ref().map(|r| r.link.machine())
    }

    /// Where it runs, for messages: "on Desktop", or "" for this server.
    pub fn on_where(&self) -> String {
        self.0
            .remote
            .as_ref()
            .map(|r| format!(" on {}", r.link.name()))
            .unwrap_or_default()
    }

    /// Stop showing connection `id` anything (it went away).
    pub(crate) fn detach_viewer(&self, id: u64) {
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        for t in tabs.iter().flat_map(|t| t.chain()) {
            t.detach(id);
        }
    }

    pub fn stats(&self) -> Stats {
        let live = self.0.live.read().clone();
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        Stats {
            running: live.is_some(),
            pid: None,
            tabs: tabs.len(),
            live_tabs: tabs.iter().filter(|t| t.is_live()).count(),
            viewers: tabs
                .iter()
                .flat_map(|t| t.chain())
                .map(|t| t.viewer_count())
                .sum(),
            frames: self.0.frames.load(Ordering::Relaxed),
            frame_bytes: self.0.frame_bytes.load(Ordering::Relaxed),
            launches: self.0.launches.load(Ordering::Relaxed),
        }
    }

    /// The browser process id while it runs (tests measure its use).
    pub async fn pid(&self) -> Option<u32> {
        self.0.run.lock().await.as_ref().and_then(|p| p.pid())
    }

    /// A client's `browser_*` request.
    pub async fn request(&self, req: Request, viewer: Option<&Viewer>) -> Result<Value, ApiError> {
        let to =
            |t: tessera_proto::BrowserTab| serde_json::to_value(t).map_err(|e| bad(e.to_string()));
        self.touch();
        match req {
            Request::BrowserAttach {
                board_id,
                width,
                height,
                dpr,
                mobile,
                pipelined,
                formats,
            } => {
                let v = viewer.ok_or_else(|| bad("attach needs a connection of its own"))?;
                let tab = self.board_tab(&board_id).map_err(bad)?;
                if let Err(e) = tab.ensure_live().await {
                    return Err(ApiError::new(ErrorCode::Internal, e));
                }
                // A popup open on the tab is what the tile shows.
                let tab = tab.top();
                let view = Viewport::clamped(width, height, dpr, mobile);
                let takes = Takes {
                    pipelined,
                    webp: formats.iter().any(|f| f == "image/webp"),
                };
                tab.attach(v.id, view, takes);
                if let Err(e) = tab.ensure_live().await {
                    tab.detach(v.id);
                    return Err(ApiError::new(ErrorCode::Internal, e));
                }
                tab.wake().await;
                tab.show().await.map_err(bad)?;
                tab.sync_view().await;
                tab.greet(v.id);
                to(tab.state())
            }
            Request::BrowserDetach { board_id } => {
                let v = viewer.ok_or_else(|| bad("detach needs a connection of its own"))?;
                if let Some(t) = self.find(&board_id) {
                    for t in t.chain() {
                        t.detach(v.id);
                    }
                }
                Ok(Value::Null)
            }
            Request::BrowserAck { board_id, frame } => {
                if let (Some(v), Some(t)) = (viewer, self.find(&board_id)) {
                    for t in t.chain() {
                        t.ack(v.id, frame);
                    }
                }
                Ok(Value::Null)
            }
            Request::BrowserInput { board_id, event } => {
                let tab = self.board_tab(&board_id).map_err(bad)?;
                tab.ensure_live()
                    .await
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
                let tab = tab.top();
                tab.wake().await;
                tab.input(event).await.map_err(bad)?;
                Ok(Value::Null)
            }
            Request::BrowserNav { board_id, action } => {
                let tab = self.board_tab(&board_id).map_err(bad)?;
                tab.ensure_live()
                    .await
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
                let base = tab.clone();
                let tab = tab.top();
                tab.wake().await;
                tab.nav(action).await.map_err(bad)?;
                to(base.top().state())
            }
            Request::BrowserNewTab { board_id, url } => {
                viewer.ok_or_else(|| bad("open tabs from your device's web view"))?;
                let board = self.new_page(&board_id, &url).map_err(bad)?;
                serde_json::to_value(board).map_err(|e| bad(e.to_string()))
            }
            Request::BrowserPage { board_id, action } => {
                viewer.ok_or_else(|| bad("use the page from your device's web view"))?;
                let tab = self.board_tab(&board_id).map_err(bad)?.top();
                tab.ensure_live().await.map_err(bad)?;
                tab.wake().await;
                serde_json::to_value(tab.page_action(action).await.map_err(bad)?)
                    .map_err(|_| bad("the page could not answer"))
            }
            Request::BrowserUpload {
                board_id,
                chooser,
                action,
            } => {
                let viewer = viewer.ok_or_else(|| bad("choose files from your device's picker"))?;
                let tab = self.board_tab(&board_id).map_err(bad)?.top();
                serde_json::to_value(
                    tab.upload_stream(&chooser, viewer.id, action)
                        .await
                        .map_err(bad)?,
                )
                .map_err(|e| bad(e.to_string()))
            }
            Request::BrowserChooseFiles {
                board_id,
                chooser,
                files,
            } => {
                viewer.ok_or_else(|| bad("choose files from your device's web view"))?;
                let tab = self
                    .find(&board_id)
                    .ok_or_else(|| bad("that file chooser closed"))?
                    .top();
                tab.choose_files(&chooser, &files).await.map_err(bad)?;
                Ok(Value::Null)
            }
            Request::BrowserDialogAnswer {
                board_id,
                accept,
                text,
            } => {
                let tab = self
                    .find(&board_id)
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no open dialog"))?
                    .top();
                tab.answer_dialog(accept, text).await.map_err(bad)?;
                Ok(Value::Null)
            }
            _ => Err(bad("not a browser request")),
        }
    }

    pub(crate) fn touch(&self) {
        *self.0.last_use.lock() = Instant::now();
    }

    /// A tab's state as clients see it (`key`: a board id), if it has one.
    pub fn find_state(&self, key: &str) -> Option<tessera_proto::BrowserTab> {
        self.find(key).map(|t| t.top().state())
    }

    pub(crate) fn find(&self, key: &str) -> Option<Arc<Tab>> {
        self.0.tabs.lock().get(key).cloned()
    }

    /// The tab a popup was opened from (itself when it isn't a popup).
    fn base_of(&self, tab: &Arc<Tab>) -> Arc<Tab> {
        let mut t = tab.clone();
        for _ in 0..16 {
            match t.opener() {
                Some(p) => t = p,
                None => break,
            }
        }
        t
    }

    fn host(&self) -> Result<Arc<dyn Host>, String> {
        self.0
            .host
            .upgrade()
            .ok_or_else(|| "the server is shutting down".to_string())
    }

    /// The tab of a web view board (made on first use).
    pub(crate) fn board_tab(&self, board_id: &str) -> Result<Arc<Tab>, String> {
        let target = self.host()?.board_target(board_id)?;
        if let Some(t) = self.find(board_id) {
            return Ok(t);
        }
        let tab = Tab::new(
            &self.0,
            board_id.to_string(),
            Some(board_id.to_string()),
            None,
            Some(target.session_id),
            target.url,
        );
        let mut tabs = self.0.tabs.lock();
        Ok(tabs.entry(board_id.to_string()).or_insert(tab).clone())
    }

    /// Close the least recently used tab nobody is watching when at the
    /// limit, or (this server's browser) when memory runs low; `except` is
    /// never closed.
    fn pressure_sample(&self, low: bool, tabs: &[Arc<Tab>], except: Option<&str>) {
        let notices: Vec<_> = {
            let mut warned = self.0.pressure_warned.lock();
            warned.sample(low);
            if !low {
                return;
            }
            tabs.iter()
                .filter(|t| t.is_live() || except == Some(t.key().as_str()))
                .filter(|t| warned.chats.insert(t.chat().or_else(|| t.owner())))
                .cloned()
                .collect()
        };
        for tab in notices {
            tab.notice(NoticeLevel::Warning, LOW_MEMORY);
        }
    }

    async fn admit_page(
        &self,
        requester: Option<&Arc<Tab>>,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        self.admit_page_sample(requester, || self.0.remote.is_none() && memory_low())
            .await
    }

    async fn admit_page_sample(
        &self,
        requester: Option<&Arc<Tab>>,
        pressure: impl FnOnce() -> bool,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        let guard = process::bounded_admission(self.0.page_admission.clone().lock_owned()).await?;
        if requester.is_some_and(|t| !t.registered()) {
            return Err("This tab is closed; open a new tab.".into());
        }
        let except = requester.map(|t| t.key());
        self.make_room_for(except.as_deref(), requester.map(AsRef::as_ref), pressure())?;
        Ok(guard)
    }

    #[cfg(test)]
    fn make_room_sample(&self, except: Option<&str>, low: bool) -> Result<(), String> {
        self.make_room_for(except, None, low)
    }

    fn warn_requester(&self, requester: &Tab, low: bool) {
        if low {
            let fresh = {
                let mut episode = self.0.pressure_warned.lock();
                episode.sample(true);
                episode
                    .chats
                    .insert(requester.chat().or_else(|| requester.owner()))
            };
            if fresh {
                requester.notice(NoticeLevel::Warning, LOW_MEMORY);
            }
        }
    }

    fn make_room_for(
        &self,
        except: Option<&str>,
        requester: Option<&Tab>,
        low: bool,
    ) -> Result<(), String> {
        let plan = self.plan_page(except, requester, low)?;
        self.commit_page_plan(plan)
    }

    /// Selection is read-only. Only idle live root pages supply headroom.
    /// Per-chat handle caps refuse allocation rather than retiring a request.
    fn plan_page(
        &self,
        except: Option<&str>,
        requester: Option<&Tab>,
        low: bool,
    ) -> Result<PagePlan, String> {
        let mut max = self.host()?.browser_config().max_tabs.max(2);
        if let Some(r) = &self.0.remote {
            let theirs = r.max_tabs.load(Ordering::Relaxed);
            max = max.min(if theirs == 0 { 8 } else { theirs }).max(1);
        }
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        self.pressure_sample(low, &tabs, except);
        if let Some(requester) = requester {
            self.warn_requester(requester, low);
        }
        let mut victims: Vec<PageVictim> = Vec::new();
        let pick = |chosen: &[PageVictim]| {
            least_needed(
                tabs.iter()
                    .filter(|t| {
                        Some(t.key().as_str()) != except
                            && t.is_live()
                            && !chosen.iter().any(|v| Arc::ptr_eq(&v.tab, t))
                    })
                    .map(|t| {
                        let (protected, used) = t.memory_use();
                        (t, protected, used)
                    }),
                Duration::ZERO,
            )
            .cloned()
        };
        if low {
            victims.push(PageVictim {
                tab: pick(&victims).ok_or(LOW_MEMORY)?,
            });
        }
        let live = tabs.iter().filter(|t| t.is_live()).count();
        while live.saturating_sub(victims.len()) >= max {
            let victim = pick(&victims).ok_or_else(|| {
                format!("{max} tabs are open and all are in use; close one first")
            })?;
            victims.push(PageVictim { tab: victim });
        }
        if !victims.is_empty() && self.0.remote.is_some() && !tab::may_make_room(self.generation())
        {
            return Err(format!(
                "{max} tabs are open on that machine; close one first"
            ));
        }
        Ok(PagePlan { victims, low, max })
    }

    /// All action/tree/state guards are acquired and every victim revalidated
    /// before the first unlink/close. Refusal cannot leave a partial eviction.
    fn commit_page_plan(&self, plan: PagePlan) -> Result<(), String> {
        let note = if plan.low {
            CLOSED_FOR_MEMORY.to_string()
        } else {
            format!(
                "This tab's page was closed to open another ({} pages open at once at most); it opened again at its last address, so anything typed into it and not yet submitted is gone.",
                plan.max
            )
        };
        if !Tab::shed_all(&plan.victims, Duration::ZERO, &note) {
            return Err(if plan.low {
                LOW_MEMORY
            } else {
                "all tabs are in use; close one first"
            }
            .into());
        }
        Ok(())
    }

    /// A board's address changed (an agent or the user updated it): its tab
    /// follows.
    pub fn board_changed(&self, board_id: &str) {
        self.board_changed_on(board_id, None);
    }

    /// The same, for an agent's change allowed on run `admitted` of this
    /// browser: only while that run is the one going (a Stop on its machine
    /// since closed it; a new one isn't covered by that check).
    pub fn board_changed_on(&self, board_id: &str, admitted: Option<u64>) {
        let Some(tab) = self.find(board_id) else {
            return;
        };
        let Ok(target) = self.host().and_then(|h| h.board_target(board_id)) else {
            return;
        };
        if tab.url() == target.url {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            if admitted.is_some_and(|g| me.generation() != Some(g)) {
                tab.notice(NoticeLevel::Warning, RESTARTED);
                return;
            }
            if tab.is_live() {
                let _ = tab::on_run(admitted, tab.go(&target.url)).await;
            } else {
                tab.set_url(&target.url);
            }
        });
    }

    /// The run going now, if any (never starts one).
    pub(crate) fn generation(&self) -> Option<u64> {
        self.0
            .live
            .read()
            .as_ref()
            .filter(|l| !l.cdp.is_closed())
            .map(|l| l.generation)
    }

    /// A board went away: so does its tab.
    pub fn board_removed(&self, board_id: &str) {
        self.ungroup(board_id);
        let tab = self.0.tabs.lock().remove(board_id);
        if let Some(t) = tab {
            t.close();
        }
    }

    /// A chat went away: its agent's own tabs close.
    pub fn session_removed(&self, session_id: &str) {
        let gone: Vec<Arc<Tab>> = {
            let mut tabs = self.0.tabs.lock();
            let keys: Vec<String> = tabs
                .iter()
                .filter(|(_, t)| t.owner().as_deref() == Some(session_id))
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| tabs.remove(k)).collect()
        };
        for t in gone {
            t.close();
        }
    }

    /// A DevTools command on a board's page, as this server's code sends
    /// them (diagnostics and tests: what a machine refuses).
    #[doc(hidden)]
    pub async fn raw_cdp(
        &self,
        board_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let tab = self.board_tab(board_id)?;
        let link = tab.ensure_live().await?;
        let session = (!method.starts_with("Browser.") && !method.starts_with("Target."))
            .then_some(link.session.as_str());
        link.cdp.call(session, method, params).await
    }

    /// What a web view board's tab shows now, if it has a live one.
    pub async fn board_screenshot(&self, board_id: &str) -> Option<tessera_proto::SnapshotImage> {
        let tab = self.find(board_id).filter(|t| t.is_live())?.top();
        tab.wake().await;
        tab.screenshot(1600, 80).await.ok().map(|s| s.image)
    }

    /// What Settings shows about the browser.
    pub async fn info(&self) -> tessera_proto::BrowserInfo {
        let cfg = self.host().map(|h| h.browser_config()).unwrap_or_default();
        let path = process::find(cfg.path.as_deref());
        let live = self.running();
        let name = path.as_deref().map(|p| {
            let base = process::display_name(p);
            match &live {
                Some(l) => format!("{base} {}", l.major),
                None => base.to_string(),
            }
        });
        let tabs = self.0.tabs.lock().values().filter(|t| t.is_live()).count();
        // Measured at an earlier start, if the browser hasn't run yet.
        let mut speed = cfg
            .speed
            .filter(|f| f.is_finite())
            .map(render::ms_per_mpx)
            .or(*self.0.speed.lock());
        if speed.is_none()
            && let Some(bin) = &path
            && let Some(m) = render::load(&render::speed_file(&self.0.profile))
                .filter(|m| m.key == render::key(bin))
        {
            speed = m.ms_per_mpx;
        }
        let budget = crate::budget::get();
        tessera_proto::BrowserInfo {
            enabled: cfg.enabled,
            low_memory: budget.ram.filter(|_| budget.small),
            found: path.is_some(),
            path: path.map(|p| p.display().to_string()),
            name,
            running: live.is_some(),
            headless: cfg.headless,
            tabs: tabs as u32,
            quality: cfg.quality,
            speed: speed.map(|k| (render::fps(k) * 10.0).round() / 10.0),
        }
    }

    /// `[browser] quality` changed: open web views follow at once (when
    /// tabs have screens of their own; otherwise from the next start).
    pub async fn quality_changed(&self) {
        if self.running().is_none() {
            return;
        }
        if !self.screens_on() {
            self.stop_if_unwatched().await;
            return;
        }
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        for t in tabs.iter().flat_map(|t| t.chain()) {
            if t.viewer_count() > 0 {
                t.sync_view().await;
            }
        }
    }

    /// Stop the browser if no tile is showing it (settings changed); it
    /// starts again on next use.
    pub async fn stop_if_unwatched(&self) {
        let watched = self.0.tabs.lock().values().any(|t| t.watched());
        if !watched && self.running().is_some() {
            self.stop().await;
        }
    }

    /// Stop the browser (server shutdown).
    pub async fn shutdown(&self) {
        self.stop().await;
    }

    // ---- lifecycle ----------------------------------------------------------

    /// The running browser, started if need be. (Boxed: starting it
    /// spawns tasks that may start it again.)
    pub(crate) fn live(&self) -> futures::future::BoxFuture<'static, Result<Arc<Live>, String>> {
        let me = self.clone();
        Box::pin(async move { me.start().await })
    }

    fn begin_cleanup(&self, exit: Option<tokio_util::sync::CancellationToken>) -> Cleanup {
        self.begin_cleanup_with(tokio_util::sync::CancellationToken::new(), exit)
    }

    fn begin_run_cleanup(
        &self,
        live: &Live,
        exit: Option<tokio_util::sync::CancellationToken>,
    ) -> Cleanup {
        self.begin_cleanup_with(live.stopped.clone(), exit)
    }

    fn begin_cleanup_with(
        &self,
        done: tokio_util::sync::CancellationToken,
        exit: Option<tokio_util::sync::CancellationToken>,
    ) -> Cleanup {
        *self.0.cleaning.write() = Some(done.clone());
        Cleanup { done, exit }
    }

    fn start_failed(&self, kind: remote::StartupKind) {
        if kind == remote::StartupKind::Unexpected {
            self.0
                .recovery
                .lock()
                .failed(Instant::now(), Duration::ZERO);
        }
    }

    async fn launch_checked(
        &self,
        launcher: &Launcher,
        headless: bool,
        scale: Option<f64>,
    ) -> Result<(Proc, mpsc::UnboundedReceiver<CdpEvent>, String), String> {
        let result = launcher
            .launch(
                &self.0.profile,
                headless,
                scale,
                self.0.profile_gate.clone(),
            )
            .await;
        result.map_err(|error| {
            self.start_failed(error.kind);
            error.message
        })
    }

    async fn probe_screens(&self, proc: &Proc) -> Result<bool, String> {
        render::screens_supported(proc.cdp())
            .await
            .inspect_err(|_| {
                let kind = self
                    .0
                    .remote
                    .as_ref()
                    .and_then(|r| *r.startup_kind.lock())
                    .unwrap_or(remote::StartupKind::Unexpected);
                self.start_failed(kind);
            })
    }

    async fn start(&self) -> Result<Arc<Live>, String> {
        loop {
            let (attempt, owner) = {
                let mut starting = self.0.starting.lock();
                if let Some(l) = self.0.live.read().clone().filter(|l| !l.cdp.is_closed()) {
                    return Ok(l);
                }
                if let Some(attempt) = starting.as_ref() {
                    (attempt.clone(), false)
                } else {
                    let attempt = Arc::new(Startup::default());
                    *starting = Some(attempt.clone());
                    (attempt, true)
                }
            };
            if owner {
                let _owner = Starting {
                    shared: self.0.clone(),
                    attempt: attempt.clone(),
                };
                return tokio::select! {
                    biased;
                    _ = attempt.cancel.cancelled() => Err(START_CANCELLED.into()),
                    result = self.start_inner(&attempt) => result,
                };
            }
            process::bounded_admission(async {
                tokio::select! {
                    biased;
                    _ = attempt.cancel.cancelled() => Err(START_CANCELLED.to_string()),
                    _ = attempt.done.cancelled() => Ok(()),
                }
            })
            .await??;
        }
    }

    async fn start_inner(&self, attempt: &Arc<Startup>) -> Result<Arc<Live>, String> {
        if let Some(l) = self.0.live.read().clone()
            && !l.cdp.is_closed()
        {
            return Ok(l);
        }
        // The last run just ended and `events_loop` is still seeing to it
        // (its exit, its pages): that goes first.
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .0
            .live
            .read()
            .as_ref()
            .is_some_and(|l| l.cdp.is_closed())
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        process::bounded_admission(async {
            loop {
                let cleaning = self.0.cleaning.read().clone();
                if let Some(done) = cleaning.filter(|t| !t.is_cancelled()) {
                    done.cancelled().await;
                } else {
                    break;
                }
            }
        })
        .await?;
        if let Some(l) = self.0.live.read().clone()
            && !l.cdp.is_closed()
        {
            return Ok(l);
        }
        let (failures, wait) = {
            let r = self.0.recovery.lock();
            (r.failures, r.wait(Instant::now()))
        };
        if let Some(wait) = wait {
            if failures > 3 {
                return Err(format!(
                    "The web view's browser{} keeps stopping ({failures} times without ten quiet minutes). Its tabs and addresses are kept; wait {} seconds, then reload or use the tab again. Anything typed and not submitted is gone.",
                    self.on_where(),
                    wait.as_secs() + 1
                ));
            }
            // The first few attempts wait too, including agent-only tabs.
            tokio::time::sleep(wait).await;
        }
        let host = self.host()?;
        let cfg = host.browser_config();
        // A machine's browser runs headless whatever this server's does.
        let headless = cfg.headless || self.0.remote.is_some();
        let launcher = match &self.0.remote {
            Some(r) => {
                let machine = r.link.machine();
                host.machine_ok(None, &machine)?;
                Launcher::Remote(r.clone())
            }
            None => {
                if !cfg.enabled {
                    let budget = crate::budget::get();
                    return Err(match budget.ram.filter(|_| budget.small) {
                        Some(ram) => format!(
                            "web views are off on this server: it has {} of memory and a browser needs about 400 MB. Turn them on in Settings → Web & voice, or open web views on one of your computers",
                            crate::util::human_bytes(ram)
                        ),
                        None => "the web view browser is turned off (`[browser] enabled = false`)"
                            .into(),
                    });
                }
                let bin = process::find(cfg.path.as_deref()).ok_or_else(|| match &cfg.path {
                    Some(p) => format!("the browser `{p}` (`[browser] path`) isn't there"),
                    None => "no browser for web views on the server: run Tessera's installer again to download one (no sudo needed), install Google Chrome or Chromium, or set `[browser] path`".to_string(),
                })?;
                Launcher::Local(bin)
            }
        };
        if self.0.remote.is_none() {
            self.0.uploads.lock().reset()?;
        }
        private_dir(&self.0.profile)
            .map_err(|e| format!("can't create {}: {e}", self.0.profile.display()))?;
        if self.0.remote.is_none() {
            private_dir(&self.0.downloads)
                .map_err(|e| format!("can't create {}: {e}", self.0.downloads.display()))?;
            // Whatever was left half-downloaded when it last stopped.
            clean_partial(&self.0.downloads);
        }
        let cap = if cfg.scale.is_finite() {
            cfg.scale.clamp(1.0, 3.0)
        } else {
            2.0
        };
        self.0.cap.store(cap.to_bits(), Ordering::Relaxed);
        // How fast it draws, if measured before with this browser.
        let speed_file = render::speed_file(&self.0.profile);
        let local_key = match &launcher {
            Launcher::Local(bin) => Some(render::key(bin)),
            Launcher::Remote(_) => None,
        };
        let given = cfg
            .speed
            .filter(|f| f.is_finite() && self.0.remote.is_none())
            .map(render::ms_per_mpx);
        let saved = render::load(&speed_file);
        let known_for = |key: &str| saved.clone().filter(|m| m.key == key);
        let known = local_key.as_deref().and_then(known_for);
        *self.0.speed.lock() = given.or(known.as_ref().and_then(|m| m.ms_per_mpx));
        // Headless, each tab renders at its own scale on a virtual screen
        // when the browser has them; otherwise one scale for all.
        let try_screens =
            headless && cfg.virtual_screens && !known.as_ref().is_some_and(|m| m.no_screens);
        let one = render::global(cfg.quality, cap, *self.0.speed.lock());
        let forced = (headless && !try_screens).then_some(one);
        let (mut proc, mut events, key) = self.launch_checked(&launcher, headless, forced).await?;
        // A machine says what its speed depends on once it runs.
        let known = known.or_else(|| known_for(&key));
        if self.0.remote.is_some() {
            *self.0.speed.lock() = known.as_ref().and_then(|m| m.ms_per_mpx);
        }
        // The first remote launch resolves its cache key. Recompute the
        // global fallback only after loading that machine's measured speed.
        let one = render::global(cfg.quality, cap, *self.0.speed.lock());
        let try_screens = try_screens && !known.as_ref().is_some_and(|m| m.no_screens);
        let screens = if try_screens {
            match self.probe_screens(&proc).await {
                Ok(supported) => supported,
                Err(error) => {
                    let cleaning = self.begin_cleanup(proc.completion());
                    proc.stop().await;
                    drop(cleaning);
                    return Err(error); // next admission observes the counted backoff
                }
            }
        } else {
            false
        };
        if headless && !screens && forced != Some(one) {
            tracing::info!("this browser has no virtual screens: every tab renders at {one}x");
            // Remember even on a fresh profile or with an explicit speed,
            // before fallback/cancellation can interrupt startup.
            render::save(
                &speed_file,
                &render::Measured {
                    key: key.clone(),
                    ms_per_mpx: *self.0.speed.lock(),
                    no_screens: true,
                },
            );
            proc.stop().await;
            (proc, events, _) = self.launch_checked(&launcher, true, Some(one)).await?;
        }
        let scale = match (screens, headless) {
            (true, _) => 1.0,
            (false, true) => one,
            // A window on the server's desktop: its screen decides.
            (false, false) => cap,
        };
        self.0.scale.store(scale.to_bits(), Ordering::Relaxed);
        *self.0.screens.lock() = render::Screens::new(screens);
        let cdp = proc.cdp().clone();
        let setup = async {
            let version = cdp.call(None, "Browser.getVersion", json!({})).await?;
            let major = version["product"]
                .as_str()
                .unwrap_or_default()
                .split('/')
                .nth(1)
                .unwrap_or_default()
                .split('.')
                .next()
                .unwrap_or_default()
                .to_string();
            // A machine sets where downloads go and which pages it attaches
            // itself (it allows neither from here).
            if self.0.remote.is_none() {
                cdp.call(
                    None,
                    "Browser.setDownloadBehavior",
                    json!({
                        "behavior": "allowAndName",
                        "downloadPath": self.0.downloads.display().to_string(),
                        "eventsEnabled": true,
                    }),
                )
                .await?;
                cdp.call(None, "Target.setDiscoverTargets", json!({"discover": true}))
                    .await?;
                // Every new page waits for its page hooks and network guard.
                cdp.call(
                    None,
                    "Target.setAutoAttach",
                    json!({
                        "autoAttach": true,
                        "waitForDebuggerOnStart": true,
                        "flatten": true,
                        "filter": [{"type": "page"}, {"exclude": true}],
                    }),
                )
                .await?;
            }
            Ok::<_, String>(major)
        }
        .await;
        let major = match setup {
            Ok(major) => major,
            Err(error) => {
                self.start_failed(
                    self.0
                        .remote
                        .as_ref()
                        .and_then(|r| *r.startup_kind.lock())
                        .unwrap_or(remote::StartupKind::Unexpected),
                );
                // No partially configured process survives an error. Cleanup
                // is bounded and outside the start lock; profile reuse waits.
                let cleaning = self.begin_cleanup(proc.completion());
                proc.stop().await;
                drop(cleaning);
                return Err(error);
            }
        };
        let generation = self.0.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let live = Arc::new(Live {
            cdp,
            generation,
            major,
            headless,
            started: Instant::now(),
            stopped: tokio_util::sync::CancellationToken::new(),
        });
        match proc.pid() {
            Some(pid) => tracing::info!(
                "browser started: {} (pid {pid}, {})",
                launcher.describe(),
                if headless { "headless" } else { "windowed" }
            ),
            None => tracing::info!("browser started{} (headless)", self.on_where()),
        }

        // The first start with this browser on this computer: measure how
        // fast it draws (before any tab, so nothing else draws meanwhile).
        let no_screens = (try_screens && !screens) || known.as_ref().is_some_and(|m| m.no_screens);
        if headless && known.as_ref().and_then(|m| m.ms_per_mpx).is_none() && given.is_none() {
            // This inline pump is owned by startup, as is the provisional
            // Process. Cancellation drops both; publication hands the same
            // receiver to events_loop without lost or duplicated events.
            let k = match self.measure_with_events(&live, &mut events).await {
                Ok(k) => k,
                Err(error) => {
                    self.start_failed(remote::StartupKind::Unexpected);
                    let cleaning = self.begin_cleanup(proc.completion());
                    proc.stop().await;
                    drop(cleaning);
                    return Err(error);
                }
            };
            *self.0.speed.lock() = k;
            let m = render::Measured {
                key,
                ms_per_mpx: k,
                no_screens,
            };
            render::save(&speed_file, &m);
        } else if no_screens && known.is_some() {
            let m = render::Measured {
                key,
                ms_per_mpx: *self.0.speed.lock(),
                no_screens,
            };
            render::save(&speed_file, &m);
        }
        let mut run = self.0.run.lock().await;
        // Stop and publication are serialized only for this synchronous
        // commit. No profile/remote/probe/setup await holds the run lock.
        let starting = self.0.starting.lock();
        if attempt.cancel.is_cancelled()
            || !starting.as_ref().is_some_and(|s| Arc::ptr_eq(s, attempt))
        {
            return Err(START_CANCELLED.into());
        }
        if self
            .0
            .cleaning
            .read()
            .as_ref()
            .is_some_and(|t| !t.is_cancelled())
        {
            return Err(process::STILL_CLOSING.into());
        }
        *self.0.live.write() = Some(live.clone());
        *run = Some(proc);
        drop(starting);
        drop(run);
        self.0.launches.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(self.clone().events_loop(live.clone(), events));
        *self.0.last_use.lock() = Instant::now();
        tokio::spawn(self.clone().idle_watch(generation, cfg.idle_minutes));
        // A machine's browser is the machine's memory (it keeps its own).
        if self.0.remote.is_none() {
            tokio::spawn(self.clone().memory_watch(generation));
        }
        Ok(live)
    }

    async fn measure_with_events(
        &self,
        live: &Arc<Live>,
        events: &mut mpsc::UnboundedReceiver<CdpEvent>,
    ) -> Result<Option<f64>, String> {
        let measure = self.measure_speed(&live.cdp, self.global_scale());
        tokio::pin!(measure);
        loop {
            tokio::select! {
                biased;
                ev = events.recv() => match ev {
                    Some(ev) => self.on_browser_event(live, ev),
                    None => return Err("the browser closed while measuring its drawing speed".into()),
                },
                measured = &mut measure => return Ok(measured),
            }
        }
    }

    async fn stop(&self) {
        // Revoke every phase before touching the published process slot.
        if let Some(starting) = self.0.starting.lock().as_ref() {
            starting.cancel.cancel();
        }
        let mut run = self.0.run.lock().await;
        // Cancel a pending automatic reopen even when the process is gone.
        {
            let mut r = self.0.recovery.lock();
            r.cancelled = r.cancelled.wrapping_add(1);
        }
        let Some(proc) = run.take() else { return };
        self.0.stopping.store(true, Ordering::SeqCst);
        let live = self.0.live.write().take();
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        for t in &tabs {
            t.unlink();
        }
        let exit = proc.completion();
        let cleaning = match live.as_ref() {
            Some(live) => self.begin_run_cleanup(live, exit),
            None => self.begin_cleanup(exit),
        };
        drop(run);
        proc.stop().await;
        drop(cleaning);
        self.0.stopping.store(false, Ordering::SeqCst);
        tracing::info!("browser{} stopped", self.on_where());
    }

    /// A rollback cannot stop a replacement browser. Both identities are
    /// checked under the run lock before kill/EOF enters normal recovery.
    async fn recover_creation(&self, original: &Arc<Live>) {
        if self.creation_gone(original) {
            return;
        }
        {
            let mut run = self.0.run.lock().await;
            let same = self.0.live.read().as_ref().is_some_and(|live| {
                live.generation == original.generation && Arc::ptr_eq(&live.cdp, &original.cdp)
            });
            if same
                && run
                    .as_ref()
                    .is_none_or(|proc| Arc::ptr_eq(proc.cdp(), &original.cdp))
            {
                tracing::warn!(
                    generation = original.generation,
                    "page rollback timed out; recovering its browser generation"
                );
                if let Some(proc) = run.as_mut() {
                    proc.kill();
                }
                original.cdp.close();
            }
        }
        if !self.creation_gone(original) {
            original.stopped.cancelled().await;
        }
    }

    fn creation_gone(&self, original: &Live) -> bool {
        original.stopped.is_cancelled()
            || self.0.live.read().as_ref().is_some_and(|live| {
                live.generation != original.generation || !Arc::ptr_eq(&live.cdp, &original.cdp)
            })
    }
    /// While this server's browser runs: when the computer runs low on
    /// memory, close the page nobody looks at that went longest unused (it
    /// opens again when someone looks at it), one every few seconds, so
    /// pages give way before the system starts killing processes. A page
    /// used in the last half minute stays (an agent is at work in it).
    async fn memory_watch(self, generation: u64) {
        let step = Duration::from_secs(5);
        loop {
            tokio::time::sleep(step).await;
            let same = self
                .0
                .live
                .read()
                .as_ref()
                .is_some_and(|l| l.generation == generation);
            if !same {
                return;
            }
            let Some(free) = crate::budget::mem_available() else {
                return;
            };
            let low = free < crate::budget::low_memory_mark();
            let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
            self.pressure_sample(low, &tabs, None);
            if !low {
                continue;
            }
            let spare = least_needed(
                tabs.iter().filter(|t| t.is_live()).map(|t| {
                    let (protected, used) = t.memory_use();
                    (t, protected, used)
                }),
                Duration::from_secs(30),
            );
            if let Some(t) = spare
                && t.shed(Duration::from_secs(30), CLOSED_FOR_MEMORY)
            {
                tracing::info!(
                    "memory is low ({} MB free): closed web view page {} until someone looks at it",
                    free >> 20,
                    t.key()
                );
            }
        }
    }

    async fn idle_watch(self, generation: u64, minutes: f64) {
        let idle = Duration::from_secs_f64((minutes * 60.0).clamp(1.0, 7.0 * 86_400.0));
        let step = Duration::from_secs(30).min(idle / 2);
        loop {
            tokio::time::sleep(step).await;
            let same = self
                .0
                .live
                .read()
                .as_ref()
                .is_some_and(|l| l.generation == generation);
            if !same {
                return;
            }
            let watched = self.0.tabs.lock().values().any(|t| t.watched());
            if !watched && self.0.last_use.lock().elapsed() >= idle {
                tracing::info!(
                    "browser{} idle for {minutes} min; stopping it",
                    self.on_where()
                );
                self.stop().await;
                return;
            }
            // A machine stops its browser when it hears nothing for a while
            // (in case this server went away): it is still wanted.
            if self.0.remote.is_some()
                && let Some(l) = self.running()
            {
                l.cdp.send(None, "Browser.getVersion", json!({}));
            }
        }
    }

    async fn events_loop(self, live: Arc<Live>, mut rx: mpsc::UnboundedReceiver<CdpEvent>) {
        while let Some(ev) = rx.recv().await {
            self.on_browser_event(&live, ev);
        }
        // The pipe closed. Seen to under the run lock, with the run still
        // this one: a browser started meanwhile is never taken (and killed)
        // for this one. (It once was: a page's call that found the browser
        // gone started the next one while this was still being seen to.)
        let mut run = self.0.run.lock().await;
        let ours = self
            .0
            .live
            .read()
            .as_ref()
            .is_some_and(|l| l.generation == live.generation && Arc::ptr_eq(&l.cdp, &live.cdp));
        if !ours || self.0.stopping.load(Ordering::SeqCst) {
            return;
        }
        *self.0.live.write() = None;
        let proc = run.take();
        let cleaning = self.begin_run_cleanup(&live, proc.as_ref().and_then(Proc::completion));
        drop(run);
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        // The tabs that had a page on it (their agents are told), before
        // their pages go. Only this run's pages go: one a call opened on a
        // browser started meanwhile stays.
        let had: Vec<Arc<Tab>> = tabs
            .iter()
            .filter(|t| t.had_page_on(live.generation))
            .cloned()
            .collect();
        let open = had.iter().map(|t| t.chain().len()).sum::<usize>();
        tab::on_run(Some(live.generation), async {
            for t in &tabs {
                t.unlink();
            }
        })
        .await;
        // How it ended: this server's browser says (its exit status and
        // what it printed last); a machine's says why in its news.
        let cancelled = self.0.recovery.lock().cancelled;
        // Group teardown and bounded reaping never occupy the start lock.
        // The cleanup token still gates profile reuse and notice consumption.
        let ended = match proc {
            Some(Proc::Local(p)) => {
                let log = p.log.clone();
                let ran = p.started.elapsed();
                let how = p.reap(Duration::from_secs(2)).await;
                let words = process::last_words(&log);
                Some((how, words, ran))
            }
            Some(mut p) => {
                p.kill();
                None
            }
            None => None,
        };
        // A machine's: it went offline, was paused or stopped its browser
        // (and says why).
        let (reopen, delay) = if let Some(r) = &self.0.remote {
            let why = r.why.lock().take();
            let name = r.link.name();
            let usable = self
                .host()
                .is_ok_and(|h| h.machine_ok(None, &r.link.machine()).is_ok());
            tracing::info!(
                "the browser on {name} stopped ({open} page(s) open): {}",
                why.as_deref().unwrap_or("no reason given")
            );
            // A crash is worth starting again at once, and so is a stop for
            // using too much memory (the pages that cost most stay closed
            // until used again); a machine that stopped it on purpose
            // (switched off, paused, idle) or went away says why.
            let crashed = why.is_none() || r.crashed.swap(false, Ordering::Relaxed);
            let memory = !crashed && why.as_deref().is_some_and(remote::memory_stop);
            let (recent, delay) = if crashed || memory {
                self.0
                    .recovery
                    .lock()
                    .failed(Instant::now(), live.started.elapsed())
            } else {
                (0, Duration::ZERO)
            };
            let what = why.unwrap_or_else(|| "it stopped unexpectedly".into());
            let reopen = usable && (crashed || memory) && recent <= 3;
            let watched: Vec<&Arc<Tab>> = had.iter().collect();
            for t in &watched {
                t.notice(
                    NoticeLevel::Warning,
                    &if reopen {
                        format!(
                            "The web view's browser on {name} stopped ({what}); pages reopen when used after {} seconds. Anything typed and not submitted is gone.", delay.as_secs_f64()
                        )
                    } else {
                        format!("The web view's browser on {name} stopped: {what}. Tabs and addresses are kept; {}anything typed and not submitted is gone.", if recent > 3 { "wait 60 seconds, then reload to try again; " } else { "" })
                    },
                );
            }
            // What the agents working in its tabs are told on their next step.
            let note = if memory {
                format!(
                    "The browser on {name} was stopped because {what}. This tab opens again at its last address, so anything typed into it and not yet submitted is gone; close the tabs you no longer need (`close`) to stay under that limit."
                )
            } else {
                format!(
                    "The browser on {name} stopped ({what}). This tab opens again at its last address when used, so anything typed into it and not yet submitted is gone."
                )
            };
            for t in &had {
                t.interrupted(&note);
            }
            (reopen, delay)
        } else {
            let (how, words, ran) = ended
                .unwrap_or_else(|| ("it stopped unexpectedly".to_string(), None, Duration::ZERO));
            let (recent, delay) = self.0.recovery.lock().failed(Instant::now(), ran);
            let ran = if ran.as_secs() < 120 {
                format!("{} s", ran.as_secs())
            } else {
                format!("{} min", ran.as_secs() / 60)
            };
            let free = crate::budget::mem_available()
                .map(crate::util::human_bytes)
                .unwrap_or_else(|| "unknown".into());
            tracing::warn!(
                "the browser exited unexpectedly after {ran}: {how}; {open} page(s) open, {free} of memory available{} ({recent} stop(s) without ten quiet minutes)",
                words
                    .map(|w| format!("; it last printed: {w}"))
                    .unwrap_or_default()
            );
            for t in &had {
                t.notice(
                    NoticeLevel::Warning,
                    &if recent <= 3 {
                        format!(
                            "The web view's browser stopped unexpectedly ({how}); pages reopen when used after {} seconds. Anything typed and not submitted is gone.", delay.as_secs_f64()
                        )
                    } else {
                        format!(
                            "The web view's browser keeps stopping ({how}); tabs and addresses are kept. Wait 60 seconds, then reload to try again. Anything typed and not submitted is gone."
                        )
                    },
                );
            }
            let note = format!(
                "The browser stopped unexpectedly ({how}) and starts again when used. This tab opens again at its last address, so anything typed into it and not yet submitted is gone; look at the page before going on."
            );
            for t in &had {
                t.interrupted(&note);
            }
            (recent <= 3, delay)
        };
        // Cleanup, the restart gate and interruption notes are ready before
        // a caller may start the next run (or consume its interruption note).
        let run = self.0.run.lock().await;
        drop(cleaning);
        drop(run);
        let watched: Vec<Arc<Tab>> = tabs.into_iter().filter(|t| t.watched()).collect();
        if !reopen || watched.is_empty() {
            return;
        }
        tokio::time::sleep(delay).await;
        // A caller's new run still needs these watched pages. An intentional
        // stop, even with no running process, cancels this scheduled reopen.
        if self.0.recovery.lock().cancelled != cancelled {
            return;
        }
        for t in watched {
            if t.watched() && t.ensure_live().await.is_ok() {
                t.sync_view().await;
            }
        }
    }

    /// Open the pages being watched again, after their browser stopped on
    /// purpose (a machine showed a page on its screen meanwhile).
    pub(crate) async fn reopen_watched(&self) {
        let watched: Vec<Arc<Tab>> = self
            .0
            .tabs
            .lock()
            .values()
            .filter(|t| t.watched())
            .cloned()
            .collect();
        for t in watched {
            if t.ensure_live().await.is_ok() {
                t.sync_view().await;
            }
        }
    }

    /// Note that agent tab `key` was closed for its agent, and why.
    pub(crate) fn note_closed(&self, key: &str, owner: &str, why: String) {
        let mut c = self.0.closed.lock();
        if c.len() >= 64 {
            c.pop_front();
        }
        c.push_back(ClosedTab {
            key: key.to_string(),
            owner: owner.to_string(),
            machine: self.machine(),
            why,
        });
    }

    /// Why agent tab `key` was closed for its agent, if it was.
    pub(crate) fn why_closed(&self, key: &str, owner: &str) -> Option<String> {
        let c = self.0.closed.lock();
        let t = c.iter().rev().find(|t| t.key == key && t.owner == owner)?;
        if let Some(machine) = &t.machine {
            self.host().ok()?.machine_ok(Some(owner), machine).ok()?;
        }
        Some(t.why.clone())
    }

    /// The tab (or popup) showing page `target`.
    fn tab_by_target(&self, target: &str) -> Option<Arc<Tab>> {
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        tabs.iter()
            .flat_map(|t| t.chain())
            .find(|t| t.target().as_deref() == Some(target))
    }

    /// The session the browser attached to our new page `target`.
    pub(crate) async fn claim_attached(&self, target: &str) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some((session, _)) = self.0.attached.lock().remove(target) {
                return Ok(session);
            }
            if Instant::now() > deadline {
                return Err("the browser didn't open the tab".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn on_browser_event(&self, live: &Arc<Live>, ev: CdpEvent) {
        let p = &ev.params;
        match ev.method.as_str() {
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let info = &p["targetInfo"];
                if info["type"] != "page" {
                    return;
                }
                let id = info["targetId"].as_str().unwrap_or_default();
                if let Some(tab) = self.tab_by_target(id) {
                    tab.on_info(
                        info["title"].as_str().unwrap_or_default(),
                        info["url"].as_str().unwrap_or_default(),
                    );
                }
            }
            "Target.attachedToTarget" => {
                let info = &p["targetInfo"];
                let target = info["targetId"].as_str().unwrap_or_default().to_string();
                let session = p["sessionId"].as_str().unwrap_or_default().to_string();
                if info["type"] != "page" || target.is_empty() {
                    return;
                }
                if p["waitingForDebugger"] != true {
                    // A page that was there before (the start page): not ours.
                    live.cdp.send(
                        None,
                        "Target.detachFromTarget",
                        json!({"sessionId": session}),
                    );
                    return;
                }
                // A window a page opened (sign-in popups, target=_blank,
                // window.open): stacked on that page's tab.
                if let Some(opener) = info["openerId"]
                    .as_str()
                    .and_then(|o| self.tab_by_target(o))
                {
                    let base = self.base_of(&opener);
                    let url = info["url"].as_str().unwrap_or_default().to_string();
                    let can_access_opener = info["canAccessOpener"] == true;
                    let live = live.clone();
                    tokio::spawn(async move {
                        base.open_popup(live, target, session, url, can_access_opener)
                            .await;
                    });
                    return;
                }
                // One of ours (`Target.createTarget`): its tab claims it.
                let mut attached = self.0.attached.lock();
                let stale: Vec<String> = attached
                    .iter()
                    .filter(|(_, (_, t))| t.elapsed() > Duration::from_secs(30))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in stale {
                    attached.remove(&k);
                    live.cdp
                        .send(None, "Target.closeTarget", json!({"targetId": k}));
                }
                attached.insert(target, (session, Instant::now()));
            }
            "Target.detachedFromTarget" => {
                let id = p["targetId"].as_str().unwrap_or_default();
                if let Some(tab) = self.tab_by_target(id) {
                    tab.on_target_gone();
                }
            }
            "Target.targetDestroyed" => {
                let id = p["targetId"].as_str().unwrap_or_default();
                if let Some(tab) = self.tab_by_target(id) {
                    tab.on_target_gone();
                }
            }
            "Target.targetCrashed" => {
                let id = p["targetId"].as_str().unwrap_or_default();
                if let Some(tab) = self.tab_by_target(id) {
                    // "crashed", "killed" (out of memory, often), "oom"…
                    let free = match (&self.0.remote, crate::budget::mem_available()) {
                        (None, Some(f)) => {
                            format!("; {} of memory available", crate::util::human_bytes(f))
                        }
                        _ => String::new(),
                    };
                    tracing::info!(
                        "a web view page{} crashed: {} (code {}{free})",
                        self.on_where(),
                        crate::util::one_line(p["status"].as_str().unwrap_or("unknown"), 40),
                        p["errorCode"].as_i64().unwrap_or(0),
                    );
                    tab.on_crashed();
                }
            }
            remote::MEMORY_HIGH if self.0.remote.is_some() => self.memory_high(p),
            "Browser.downloadWillBegin" => self.download_begins(p),
            "Browser.downloadProgress" => self.download_progress(live, p),
            _ => {}
        }
    }

    /// A machine's browser is near the memory it may use there (it stops
    /// the browser past that): close the page nobody needs most (it opens
    /// again when used), or, with none to spare, warn the chats using it.
    fn memory_high(&self, p: &Value) {
        let gb = |k: &str| p[k].as_f64().unwrap_or(0.0) / (1u64 << 30) as f64;
        let (used, cap) = (gb("used"), gb("cap"));
        let name = self.on_where();
        let name = name.trim_start_matches(" on ");
        let tabs: Vec<Arc<Tab>> = self.0.tabs.lock().values().cloned().collect();
        let spare = least_needed(
            tabs.iter().filter(|t| t.is_live()).map(|t| {
                let (protected, used) = t.memory_use();
                (t, protected, used)
            }),
            Duration::from_secs(30),
        );
        let accounting = if p["accounting"].as_str() == Some("peak_commit") {
            " (peak committed memory, not shared-page PSS)"
        } else {
            ""
        };
        let warn = format!(
            "The browser on {name} is close to the memory it may use there ({used:.1} of {cap:.1} GB){accounting}: close the tabs you no longer need, or it will be stopped. Save unfinished sign-ins or forms now."
        );
        // Every active audience is warned before shedding a spare page: that
        // page may not release enough memory to avoid a later whole-browser stop.
        for t in tabs.iter().filter(|t| t.is_live()) {
            t.notice(NoticeLevel::Warning, &warn);
        }
        match spare {
            Some(t) if t.shed(Duration::from_secs(30), &format!(
                "The browser on {name} was close to the memory it may use there ({used:.1} of {cap:.1} GB), so this tab's page was closed; it opens again at its last address, but anything typed into it and not yet submitted is gone."
            )) => {
                tracing::info!(
                    "the browser on {name} uses {used:.1} of its {cap:.1} GB: closed web view page {} until it is used again",
                    t.key()
                );

            }
            _ => tracing::info!(
                "the browser on {name} uses {used:.1} of its {cap:.1} GB and every page is in use"
            ),
        }
    }

    // ---- downloads ----------------------------------------------------------

    fn download_begins(&self, p: &Value) {
        let guid = p["guid"].as_str().unwrap_or_default().to_string();
        let frame = p["frameId"].as_str().unwrap_or_default();
        let tab = self.tab_by_target(frame).or_else(|| {
            // A download from a frame inside the page: the most recently used tab.
            self.0
                .tabs
                .lock()
                .values()
                .filter(|t| t.is_live())
                .max_by_key(|t| t.last_used())
                .cloned()
        });
        let name = safe_name(p["suggestedFilename"].as_str().unwrap_or("download"));
        let d = Download {
            guid: guid.clone(),
            tab: tab.as_ref().map(|t| t.key()).unwrap_or_default(),
            name,
            url: p["url"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect(),
            started: Instant::now(),
            done: None,
            failed: false,
        };
        if let Some(t) = &tab {
            t.download_started(&guid);
        }
        let mut transfers = self.0.transfers.lock();
        transfers.retain(|_, d| d.started.elapsed() < Duration::from_secs(3600));
        transfers.insert(guid, d);
    }

    fn download_progress(&self, live: &Arc<Live>, p: &Value) {
        let guid = p["guid"].as_str().unwrap_or_default().to_string();
        let state = p["state"].as_str().unwrap_or_default();
        const MAX_DOWNLOAD: u64 = 4 << 30;
        if state == "inProgress" {
            if p["receivedBytes"].as_u64().unwrap_or(0) > MAX_DOWNLOAD {
                live.cdp
                    .send(None, "Browser.cancelDownload", json!({"guid": guid}));
            }
            return;
        }
        let Some(mut d) = self.0.transfers.lock().get(&guid).cloned() else {
            return;
        };
        let tab = self.find(&d.tab);
        if self.0.remote.is_some() {
            // On a machine: it names and keeps the file there itself.
            match state {
                "completed" => match p["tesseraPath"].as_str() {
                    Some(path) => {
                        d.done = Some(PathBuf::from(path));
                        if let Some(t) = &tab {
                            t.notice(
                                NoticeLevel::Info,
                                &format!("Downloaded {}{} ({path})", d.name, self.on_where()),
                            );
                        }
                    }
                    None => d.failed = true,
                },
                "canceled" => d.failed = true,
                _ => {}
            }
            self.0.transfers.lock().insert(guid, d);
            return;
        }
        if state == "completed" {
            let from = self.0.downloads.join(&guid);
            let to = unique_path(&self.0.downloads, &d.name);
            match std::fs::rename(&from, &to) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ =
                            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600));
                    }
                    d.done = Some(to.clone());
                    let offered = self.0.host.upgrade().is_some_and(|host| {
                        host.downloaded(&Downloaded {
                            path: to.clone(),
                            board: tab.as_ref().and_then(|t| t.board()),
                            session: tab.as_ref().and_then(|t| t.chat().or_else(|| t.owner())),
                        })
                    });
                    if let Some(t) = tab.as_ref().filter(|_| !offered) {
                        t.notice(
                            NoticeLevel::Info,
                            &format!("Downloaded {} to {}", d.name, to.display()),
                        );
                    }
                }
                Err(e) => {
                    d.failed = true;
                    tracing::warn!("download {}: {e}", d.name);
                }
            }
        } else if state == "canceled" {
            d.failed = true;
            let _ = std::fs::remove_file(self.0.downloads.join(&guid));
        }
        self.0.transfers.lock().insert(guid, d);
    }

    pub(crate) fn download(&self, guid: &str) -> Option<Download> {
        self.0.transfers.lock().get(guid).cloned()
    }

    /// Cancel a download (or delete it if it finished).
    pub(crate) async fn discard_download(&self, guid: &str) {
        let live = self.0.live.read().clone();
        if let Some(l) = &live {
            l.cdp
                .send(None, "Browser.cancelDownload", json!({"guid": guid}));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        if self.0.remote.is_some() {
            // The machine deletes it from its own downloads folder.
            self.0.transfers.lock().remove(guid);
            if let Some(l) = live {
                let _ = l
                    .cdp
                    .call(None, "Tessera.discardDownload", json!({"guid": guid}))
                    .await;
            }
            return;
        }
        let d = self.0.transfers.lock().remove(guid);
        let _ = std::fs::remove_file(self.0.downloads.join(guid));
        if let Some(p) = d.and_then(|d| d.done) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The live browser, if it runs (no start).
    pub(crate) fn running(&self) -> Option<Arc<Live>> {
        self.0.live.read().clone().filter(|l| !l.cdp.is_closed())
    }
}

impl Shared {
    pub(crate) fn host(&self) -> Option<Arc<dyn Host>> {
        self.host.upgrade()
    }
    pub(crate) fn events(&self) -> &broadcast::Sender<Event> {
        &self.events
    }
    pub(crate) fn viewer_tx(&self, id: u64) -> Option<mpsc::Sender<Event>> {
        self.hub.viewer_tx(id)
    }
}

/// Create a directory only its owner can use.
fn private_dir(d: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(d)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Downloads still named by their id were never finished.
fn clean_partial(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let guid_like = n.len() == 36
            && n.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            && n.matches('-').count() == 4;
        if guid_like {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A file name that is safe to create: no paths, no hidden files, no
/// control characters, at most 120 characters.
pub(crate) fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    s = s.trim().trim_start_matches('.').to_string();
    if s.chars().count() > 120 {
        let ext = Path::new(&s)
            .extension()
            .map(|e| e.to_string_lossy().chars().take(10).collect::<String>())
            .unwrap_or_default();
        let stem: String = s.chars().take(100).collect();
        s = if ext.is_empty() {
            stem
        } else {
            format!("{stem}.{ext}")
        };
    }
    if s.is_empty() { "download".into() } else { s }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for i in 1..1000 {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}{ext}", crate::util::now_ms()))
}

/// What the address bar's text means: an http(s) URL, a host, or words to
/// search for. Other schemes (`file:`, `javascript:`, `chrome:`…) are refused.
pub fn normalize_address(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("type an address".into());
    }
    if s == "about:blank" {
        return Ok(s.into());
    }
    if let Ok(u) = url::Url::parse(s)
        && u.has_host()
    {
        return match u.scheme() {
            "http" | "https" => Ok(u.to_string()),
            other => Err(format!("{other}: addresses can't be opened here")),
        };
    }
    if let Some((scheme, _)) = s.split_once(':')
        && !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !s.contains(' ')
        && !s[scheme.len() + 1..].starts_with(|c: char| c.is_ascii_digit())
        && scheme != "localhost"
    {
        return Err(format!("{scheme}: addresses can't be opened here"));
    }
    let hostish = !s.contains(char::is_whitespace)
        && (s.contains('.') || s.starts_with("localhost") || s.contains(':'));
    if hostish {
        let local = s.starts_with("localhost")
            || s.starts_with("127.")
            || s.starts_with("192.168.")
            || s.starts_with("10.")
            || s.starts_with('[');
        let u = format!("{}://{s}", if local { "http" } else { "https" });
        if let Ok(u) = url::Url::parse(&u)
            && u.has_host()
        {
            return Ok(u.to_string());
        }
    }
    Ok(format!(
        "https://duckduckgo.com/?q={}",
        tessera_proto::pct_encode(s)
    ))
}

/// A URL that points at Tessera's own server (pages may not reach it).
pub fn is_own_server(url: &str, own_port: Option<u16>) -> bool {
    let (Some(port), Ok(u)) = (own_port, url::Url::parse(url)) else {
        return false;
    };
    if u.port_or_known_default() != Some(port) {
        return false;
    }
    match u.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        None => false,
    }
}

/// This computer is short of memory (see `budget::low_memory_mark`).
fn memory_low() -> bool {
    crate::budget::mem_available().is_some_and(|f| f < crate::budget::low_memory_mark())
}

/// The page that can go with the least loss: nobody looks at it, unused
/// for at least `idle`, and of those the longest unused.
fn least_needed<T>(pages: impl Iterator<Item = (T, bool, Instant)>, idle: Duration) -> Option<T> {
    pages
        .filter(|(_, watched, used)| !watched && used.elapsed() >= idle)
        .min_by_key(|(_, _, used)| *used)
        .map(|(t, ..)| t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_minute_crashes_and_explicit_calls_share_backoff() {
        let start = Instant::now();
        let mut r = Recovery::default();
        for i in 0..7 {
            let now = start + Duration::from_secs(150 * i);
            let (count, delay) = r.failed(now, Duration::from_secs(145));
            assert_eq!(count, i as u32 + 1);
            assert_eq!(r.wait(now), Some(delay));
            assert!(r.wait(now + delay + Duration::from_millis(1)).is_none());
            if i >= 3 {
                assert_eq!(delay, Duration::from_secs(60), "no automatic reopen");
            }
        }
        // A stable run resets the counter; intentional idle stops don't add
        // to it (only crashes and memory stops call failed).
        assert_eq!(
            r.failed(start + Duration::from_secs(1500), Duration::from_secs(600))
                .0,
            1
        );
        assert_eq!(
            r.failed(start + Duration::from_secs(2100), Duration::ZERO)
                .0,
            1
        );
    }
    #[test]
    fn short_of_memory_the_page_least_needed_goes() {
        let now = Instant::now();
        let ago = |s: u64| now - Duration::from_secs(s);
        // (name, someone looks at it, last used)
        let pages = || {
            vec![
                ("shown", true, ago(900)),
                ("agent at work", false, ago(5)),
                ("old", false, ago(600)),
                ("older", false, ago(1200)),
            ]
            .into_iter()
        };
        // The longest unused that nobody looks at, never one on screen.
        assert_eq!(
            least_needed(pages(), Duration::from_secs(30)),
            Some("older")
        );
        // One in use within the half minute stays.
        let busy = vec![("agent at work", false, ago(5)), ("shown", true, ago(900))];
        assert_eq!(
            least_needed(busy.into_iter(), Duration::from_secs(30)),
            None
        );
        // Opening another page under pressure: any page nobody looks at.
        let fresh = vec![("just used", false, ago(1)), ("shown", true, ago(900))];
        assert_eq!(
            least_needed(fresh.into_iter(), Duration::ZERO),
            Some("just used")
        );
    }

    #[test]
    fn addresses_are_normalised_and_other_schemes_refused() {
        assert_eq!(
            normalize_address("example.com").unwrap(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_address("http://example.com/a?b=1").unwrap(),
            "http://example.com/a?b=1"
        );
        assert_eq!(
            normalize_address("localhost:3000/x").unwrap(),
            "http://localhost:3000/x"
        );
        assert_eq!(
            normalize_address("192.168.1.1").unwrap(),
            "http://192.168.1.1/"
        );
        assert!(
            normalize_address("rust borrow checker")
                .unwrap()
                .starts_with("https://duckduckgo.com/?q=rust%20borrow")
        );
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "chrome://settings",
            "view-source:https://x.com",
            "data:text/html,hi",
            "ftp://x.org/",
        ] {
            assert!(normalize_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_own_server_is_recognised_in_every_loopback_form() {
        let p = Some(7878);
        for u in [
            "http://127.0.0.1:7878/",
            "http://localhost:7878/api/ws",
            "http://[::1]:7878/",
            "http://app.localhost:7878/",
            "http://0.0.0.0:7878/",
        ] {
            assert!(is_own_server(u, p), "{u}");
        }
        for u in [
            "http://127.0.0.1:7879/",
            "https://example.com:7878/",
            "http://localhost/",
        ] {
            assert!(!is_own_server(u, p), "{u}");
        }
        assert!(!is_own_server("http://127.0.0.1:7878/", None));
    }

    #[test]
    fn download_names_are_made_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        assert_eq!(safe_name("a\u{0}b<c>.pdf"), "a_b_c_.pdf");
        assert_eq!(safe_name(""), "download");
        let long = format!("{}.pdf", "x".repeat(300));
        let s = safe_name(&long);
        assert!(s.chars().count() <= 111 && s.ends_with(".pdf"), "{s}");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (1).txt")
        );
        assert_eq!(unique_path(dir.path(), "b.txt"), dir.path().join("b.txt"));
    }

    pub(super) struct FixtureHost {
        config: Mutex<BrowserConfig>,
        machine_allowed: AtomicBool,
    }
    impl Host for FixtureHost {
        fn browser_config(&self) -> BrowserConfig {
            self.config.lock().clone()
        }
        fn board_target(&self, _: &str) -> Result<BoardTarget, String> {
            Err("no board".into())
        }
        fn board_navigated(&self, _: &str, _: &str) {}
        fn own_port(&self) -> Option<u16> {
            None
        }
        fn machine_ok(&self, _: Option<&str>, _: &str) -> Result<(), String> {
            if self.machine_allowed.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err("denied".into())
            }
        }
    }

    pub(super) fn fixture_browsers(
        dir: &Path,
        config: BrowserConfig,
    ) -> (Arc<FixtureHost>, Browsers) {
        let host = Arc::new(FixtureHost {
            config: Mutex::new(config),
            machine_allowed: AtomicBool::new(true),
        });
        let erased: Arc<dyn Host> = host.clone();
        let (events, _) = broadcast::channel(32);
        (host, Browsers::new(Arc::downgrade(&erased), events, dir))
    }

    #[test]
    fn closed_tabs_are_private_to_their_owner_and_authorised_host() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, browsers) = fixture_browsers(
            tmp.path(),
            BrowserConfig {
                max_tabs: 16,
                ..Default::default()
            },
        );
        let tab = browsers
            .new_agent_tab("chat-a", "https://private.invalid/my-account", None)
            .unwrap();
        let key = tab.key();
        assert!(browsers.close_agent_tab("chat-a", &key));
        let error = |session| browsers.agent_tab(session, Some(&key), &[]).err().unwrap();
        let own = error("chat-a");
        assert!(own.contains("was closed"));
        assert!(!own.contains("private.invalid") && !own.contains("https://"));
        let missing = format!("no tab `{key}` here (`tabs` lists them)");
        assert_eq!(error("chat-b"), missing);
        // Same record, with a machine's authorisation revoked since closure.
        browsers
            .server()
            .0
            .closed
            .lock()
            .back_mut()
            .unwrap()
            .machine = Some("desktop".into());
        host.machine_allowed.store(false, Ordering::SeqCst);
        assert_eq!(error("chat-a"), missing);
        assert_eq!(error("chat-b"), missing);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_probe_and_setup_failures_back_off_but_refusals_do_not() {
        use std::os::unix::fs::PermissionsExt;
        for stage in ["probe", "setup"] {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("fake-chrome");
            std::fs::write(&bin, format!(r##"#!/usr/bin/python3
import os,json
r=os.fdopen(3,'rb',buffering=0)
w=os.fdopen(4,'wb',buffering=0)
b=b''
while True:
 c=r.read(1)
 if not c: break
 if c!=b'\x00': b+=c; continue
 m=json.loads(b);b=b''
 if m['method']=='Browser.close': break
 if '{stage}'=='probe': break
 answer={{'id':m['id'],'result':{{'product':'Chrome/155'}}}}
 if m['method']=='Browser.setDownloadBehavior': answer={{'id':m['id'],'error':{{'message':'fixture setup failed'}}}}
 w.write(json.dumps(answer).encode()+b'\x00')
"##)).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let (host, browsers) = fixture_browsers(
                tmp.path(),
                BrowserConfig {
                    enabled: true,
                    path: Some(bin.display().to_string()),
                    virtual_screens: false,
                    speed: Some(60.0),
                    ..Default::default()
                },
            );
            let b = browsers.server();
            assert!(b.start().await.is_err());
            assert_eq!(b.0.recovery.lock().failures, 1, "{stage}");
            assert!(b.0.recovery.lock().wait(Instant::now()).is_some());
            assert!(b.start().await.is_err());
            assert_eq!(b.0.recovery.lock().failures, 2, "{stage}");
            b.0.recovery.lock().retry_at = None;
            host.config.lock().enabled = false;
            assert!(b.start().await.err().unwrap().contains("turned off"));
            assert_eq!(
                b.0.recovery.lock().failures,
                2,
                "refusal must not add failures"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_cleanup_waits_outside_the_start_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = fixture_browsers(tmp.path(), BrowserConfig::default());
        let b = browsers.server().clone();
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (cdp, _, events) = Cdp::channel();
        let live = Arc::new(Live {
            cdp: cdp.clone(),
            generation: 1,
            headless: true,
            major: "155".into(),
            started: Instant::now(),
            stopped: tokio_util::sync::CancellationToken::new(),
        });
        *b.0.run.lock().await = Some(Proc::Local(process::Process {
            cdp: cdp.clone(),
            child: Some(child),
            log: Arc::new(Mutex::new(Default::default())),
            started: Instant::now(),
            lease: Some(process::ExitLease::new(
                None,
                tokio_util::sync::CancellationToken::new(),
            )),
        }));
        *b.0.live.write() = Some(live.clone());
        let exit = tokio::spawn(b.clone().events_loop(live, events));
        cdp.close();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            b.0.run.try_lock().is_ok(),
            "stalled process cannot occupy start lock"
        );
        assert!(b.0.cleaning.read().is_some(), "profile reuse still gated");
        tokio::time::timeout(Duration::from_secs(5), exit)
            .await
            .unwrap()
            .unwrap();
        assert!(
            b.0.cleaning
                .read()
                .as_ref()
                .is_some_and(|t| t.is_cancelled())
        );
    }

    pub(super) struct RemoteFixture(
        pub(super) tokio::sync::mpsc::UnboundedSender<Vec<tessera_proto::NodeBrowserDown>>,
    );
    impl Remote for RemoteFixture {
        fn machine(&self) -> String {
            "fixture".into()
        }
        fn name(&self) -> String {
            "fixture desktop".into()
        }
        fn send(
            &self,
            msgs: Vec<tessera_proto::NodeBrowserDown>,
        ) -> futures::future::BoxFuture<'static, Result<(), String>> {
            let sent = self.0.send(msgs).map_err(|_| "offline".into());
            Box::pin(async move { sent })
        }
    }

    #[tokio::test]
    async fn remote_unexpected_stopped_during_start_counts_once_but_refusal_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = fixture_browsers(tmp.path(), BrowserConfig::default());
        let b = browsers.server();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let r = remote::RemoteHost::new(Arc::new(RemoteFixture(tx)));
        for crashed in [true, false] {
            let browser = b.clone();
            let host = r.clone();
            let task = tokio::spawn(async move {
                browser
                    .launch_checked(&Launcher::Remote(host), true, Some(1.0))
                    .await
                    .err()
                    .unwrap()
            });
            let batch = rx.recv().await.unwrap();
            let tessera_proto::NodeBrowserDown::Start { run, .. } = batch[0] else {
                panic!("expected Start")
            };
            r.feed(vec![tessera_proto::NodeBrowserUp::Stopped {
                run,
                reason: Some(
                    "fixture human text says paused, but category is authoritative".into(),
                ),
                crashed,
            }]);
            assert!(task.await.unwrap().contains("fixture human text"));
            r.feed(vec![tessera_proto::NodeBrowserUp::Failed {
                run,
                error: "late startup error".into(),
            }]);
            assert_eq!(
                b.0.recovery.lock().failures,
                1,
                "unexpected counted once, refusal not counted"
            );
            assert!(b.0.recovery.lock().retry_at.is_some());
        }
    }

    #[tokio::test]
    async fn cleanup_admission_waits_for_both_exit_and_recovery_publication() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = fixture_browsers(tmp.path(), BrowserConfig::default());
        let b = browsers.server();
        for exit_first in [true, false] {
            let exit = tokio_util::sync::CancellationToken::new();
            let policy = b.begin_cleanup(Some(exit.clone()));
            let gate = policy.done.clone();
            if exit_first {
                exit.cancel();
                assert!(
                    !gate.is_cancelled(),
                    "exit alone cannot outrun recovery/notes"
                );
                drop(policy);
            } else {
                drop(policy); // also models caller cancellation
                assert!(
                    !gate.is_cancelled(),
                    "publication alone cannot confirm exit"
                );
                exit.cancel();
            }
            tokio::time::timeout(Duration::from_secs(1), gate.cancelled())
                .await
                .unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn still_closing_admission_returns_without_blocking_stop_or_launching() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = fixture_browsers(tmp.path(), BrowserConfig::default());
        let b = browsers.server();
        let lease = process::ExitLease::new(
            Some(b.0.profile_gate.clone().lock_owned().await),
            tokio_util::sync::CancellationToken::new(),
        );
        let done = tokio_util::sync::CancellationToken::new();
        *b.0.cleaning.write() = Some(done);
        assert_eq!(b.start().await.err().unwrap(), process::STILL_CLOSING);
        assert_eq!(b.0.launches.load(Ordering::Relaxed), 0);
        tokio::time::timeout(Duration::from_secs(1), b.stop())
            .await
            .expect("admission cannot block Stop");
        assert!(b.0.profile_gate.try_lock().is_err());
        let error = Launcher::Local("/must-not-launch".into())
            .launch(tmp.path(), true, None, b.0.profile_gate.clone())
            .await
            .err()
            .unwrap();
        assert_eq!(error.message, process::STILL_CLOSING);
        assert_eq!(error.kind, remote::StartupKind::Refused);
        drop(lease);
        assert!(b.0.profile_gate.try_lock().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn started_then_stopped_during_screen_probe_counts_once_and_protocol_refusals_do_not() {
        for code in [None, Some(-32601), Some(-32000), Some(-32602), Some(987)] {
            let unsupported = code.is_some();
            let tmp = tempfile::tempdir().unwrap();
            let (_host, browsers) = fixture_browsers(tmp.path(), BrowserConfig::default());
            let b = browsers.server();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let r = remote::RemoteHost::new(Arc::new(RemoteFixture(tx)));
            let host = r.clone();
            let browser = b.clone();
            let launch = tokio::spawn(async move {
                browser
                    .launch_checked(&Launcher::Remote(host), true, None)
                    .await
                    .unwrap()
            });
            let batch = rx.recv().await.unwrap();
            let tessera_proto::NodeBrowserDown::Start { run, .. } = batch[0] else {
                panic!("expected Start")
            };
            r.feed(vec![tessera_proto::NodeBrowserUp::Started {
                run,
                product: "Chrome/155".into(),
                speed_key: "fixture".into(),
                max_tabs: 8,
            }]);
            let (proc, _events, _) = launch.await.unwrap();
            let browser = b.clone();
            let probe = tokio::spawn(async move {
                let result = browser.probe_screens(&proc).await;
                (proc, result)
            });
            let batch = rx.recv().await.unwrap();
            let tessera_proto::NodeBrowserDown::Cdp { ref messages, .. } = batch[0] else {
                panic!("expected probe")
            };
            let message: Value = serde_json::from_str(&messages[0]).unwrap();
            assert_eq!(message["method"], "Emulation.getScreenInfos");
            if unsupported {
                r.feed(vec![tessera_proto::NodeBrowserUp::Cdp { run, messages: vec![json!({"id": message["id"], "error":{"code":code.unwrap(),"message":"any human words","data":{"capability":"refused"}}}).to_string()] }]);
            } else {
                r.feed(vec![tessera_proto::NodeBrowserUp::Stopped {
                    run,
                    reason: Some("probe fixture exit".into()),
                    crashed: true,
                }]);
            }
            let (mut proc, result) = probe.await.unwrap();
            if unsupported {
                assert!(!result.unwrap());
            } else {
                assert!(result.is_err());
            }
            proc.kill();
            r.feed(vec![tessera_proto::NodeBrowserUp::Stopped {
                run,
                reason: None,
                crashed: true,
            }]);
            assert_eq!(b.0.recovery.lock().failures, u32::from(!unsupported));
            assert_eq!(b.0.recovery.lock().retry_at.is_some(), !unsupported);
        }
    }
}

#[cfg(test)]
mod r4_probe_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn fresh_default_profile_refusal_falls_back_and_remembers_no_screens() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = tests::fixture_browsers(tmp.path(), BrowserConfig::default());
        let server = browsers.server();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = Browser::remote(
            server.0.host.clone(),
            server.0.events.clone(),
            tmp.path().join("remote"),
            server.0.hub.clone(),
            Arc::new(tests::RemoteFixture(tx)),
        );
        let r = b.0.remote.clone().unwrap();
        let starts = Arc::new(AtomicU64::new(0));
        let count = starts.clone();
        let probes = Arc::new(AtomicU64::new(0));
        let probed = probes.clone();
        let driver = tokio::spawn(async move {
            while let Some(batch) = rx.recv().await {
                for command in batch {
                    use tessera_proto::{NodeBrowserDown as Down, NodeBrowserUp as Up};
                    match command {
                        Down::Start { run, scale, .. } => {
                            let n = count.fetch_add(1, Ordering::Relaxed);
                            assert_eq!(scale.is_some(), n % 2 == 1, "fallback uses global scale");
                            r.feed(vec![Up::Started {
                                run,
                                product: "Chrome/155".into(),
                                speed_key: "fixture".into(),
                                max_tabs: 8,
                            }]);
                        }
                        Down::Stop { run, .. } => r.feed(vec![Up::Stopped {
                            run,
                            reason: None,
                            crashed: false,
                        }]),
                        Down::Cdp { run, messages } => {
                            for raw in messages {
                                let m: Value = serde_json::from_str(&raw).unwrap();
                                let method = m["method"].as_str().unwrap();
                                let reply = match method {
                                    "Emulation.getScreenInfos" => {
                                        probed.fetch_add(1, Ordering::Relaxed);
                                        json!({"id":m["id"],"error":{"code":-32000,"message":"capability refused","data":{"detail":"optional"}}})
                                    }
                                    // Benchmark absence is fine in this protocol fixture.
                                    "Target.createTarget" => {
                                        json!({"id":m["id"],"error":{"code":-32000,"message":"fixture has no renderer"}})
                                    }
                                    _ => json!({"id":m["id"],"result":{"product":"Chrome/155"}}),
                                };
                                r.feed(vec![Up::Cdp {
                                    run,
                                    messages: vec![reply.to_string()],
                                }]);
                                if method == "Browser.close" {
                                    r.feed(vec![Up::Stopped {
                                        run,
                                        reason: None,
                                        crashed: false,
                                    }]);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        let live = b.start().await.unwrap();
        assert!(!live.cdp.is_closed() && !b.0.screens.lock().on);
        assert_eq!(starts.load(Ordering::Relaxed), 2);
        assert_eq!(b.0.recovery.lock().failures, 0);
        let saved = render::load(&render::speed_file(&b.0.profile)).unwrap();
        assert!(saved.no_screens && saved.key == "machine|Chrome/155|fixture");
        b.stop().await;
        let second = b.start().await.unwrap();
        assert!(!second.cdp.is_closed() && !b.0.screens.lock().on);
        assert_eq!(
            probes.load(Ordering::Relaxed),
            1,
            "cached refusal is not probed again"
        );
        assert_eq!(starts.load(Ordering::Relaxed), 4);
        assert_eq!(b.0.recovery.lock().failures, 0);
        b.stop().await;
        driver.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_screen_response_and_timeout_are_not_capability_refusals() {
        for raw in [
            Some(b"not JSON".as_slice()),
            Some(br#"{"id":1,"error":{"code":-32000}}"#.as_slice()),
            None,
        ] {
            let (cdp, mut out, _) = Cdp::channel();
            let probe_cdp = cdp.clone();
            let probe = tokio::spawn(async move { render::screens_supported(&probe_cdp).await });
            out.recv().await.unwrap();
            if let Some(raw) = raw {
                cdp.feed(raw);
            }
            assert!(probe.await.unwrap().is_err());
        }
    }
}

#[cfg(test)]
mod r4_stop_tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn stop_revokes_profile_admission_without_releasing_the_old_lease_or_launching() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = tests::fixture_browsers(
            tmp.path(),
            BrowserConfig {
                enabled: true,
                path: Some("/bin/sh".into()),
                ..Default::default()
            },
        );
        let b = browsers.server();
        let lease = process::ExitLease::new(
            Some(b.0.profile_gate.clone().lock_owned().await),
            tokio_util::sync::CancellationToken::new(),
        );
        let browser = b.clone();
        let start = tokio::spawn(async move { browser.start().await });
        tokio::task::yield_now().await; // task runs to its first profile-lock await
        let pending =
            b.0.starting
                .lock()
                .clone()
                .expect("profile admission pending");
        tokio::time::timeout(Duration::from_millis(100), b.stop())
            .await
            .unwrap();
        assert!(pending.cancel.is_cancelled());
        assert_eq!(start.await.unwrap().err().unwrap(), START_CANCELLED);
        assert!(b.0.profile_gate.try_lock().is_err());
        drop(lease);
        tokio::task::yield_now().await;
        assert!(b.0.profile_gate.try_lock().is_ok() && b.0.live.read().is_none());
        assert_eq!(b.0.launches.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_revokes_remote_start_and_pending_screen_probe_promptly() {
        use tessera_proto::{NodeBrowserDown as Down, NodeBrowserUp as Up};
        for during_probe in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let (_host, browsers) = tests::fixture_browsers(tmp.path(), BrowserConfig::default());
            let server = browsers.server();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let b = Browser::remote(
                server.0.host.clone(),
                server.0.events.clone(),
                tmp.path().join("remote"),
                server.0.hub.clone(),
                Arc::new(tests::RemoteFixture(tx)),
            );
            let remote = b.0.remote.clone().unwrap();
            let browser = b.clone();
            let start = tokio::spawn(async move { browser.start().await });
            let batch = rx.recv().await.unwrap();
            let Down::Start { run, .. } = batch[0] else {
                panic!("expected Start")
            };
            if during_probe {
                remote.feed(vec![Up::Started {
                    run,
                    product: "Chrome/155".into(),
                    speed_key: "fixture".into(),
                    max_tabs: 8,
                }]);
                let batch = rx.recv().await.unwrap();
                let Down::Cdp { ref messages, .. } = batch[0] else {
                    panic!("expected screen probe")
                };
                assert_eq!(
                    serde_json::from_str::<Value>(&messages[0]).unwrap()["method"],
                    "Emulation.getScreenInfos"
                );
            }
            let pending = b.0.starting.lock().clone().unwrap();
            tokio::time::timeout(Duration::from_millis(100), b.stop())
                .await
                .unwrap();
            assert!(pending.cancel.is_cancelled());
            assert_eq!(start.await.unwrap().err().unwrap(), START_CANCELLED);
            let batch = tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(batch[0], Down::Stop { run: stopped } if stopped == run));
            // A late start response after cancellation cannot publish a run.
            remote.feed(vec![Up::Started {
                run,
                product: "Chrome/155".into(),
                speed_key: "fixture".into(),
                max_tabs: 8,
            }]);
            assert!(b.0.live.read().is_none() && b.0.run.lock().await.is_none());
            assert_eq!(b.0.launches.load(Ordering::Relaxed), 0);
            assert_eq!(
                b.0.recovery.lock().failures,
                0,
                "intentional Stop is not a crash"
            );
        }
    }
}
