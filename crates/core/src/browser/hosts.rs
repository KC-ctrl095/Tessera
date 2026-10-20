//! Where web views run: this server's browser, or a machine's ("Web views
//! here"). [`Browsers`] holds them all: a board's requests go to its host's
//! browser (`Whiteboard::host`), the agent's tool finds its tabs on every
//! host, and each connection has one viewer that gets frames from any host.
//!
//! Who may use a machine is checked here for every agent use of a tab on it
//! (`Host::machine_ok`, the machine's own "who can use it"), and the
//! machine checks and logs it again (`Tessera.act`), the same way as its
//! commands. This is the browser tool's own path, so a Claude Code turn
//! (which calls the same tool) gets the same answer.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_proto::{ApiError, ErrorCode, Event, NodeBot, NodeBrowserUp, PermissionMode, Request};
use tokio::sync::{broadcast, mpsc};

use super::agent::describe;
use super::{AgentTab, Browser, Host, Shared, Tab, VIEWER_QUEUE, Viewer};

/// The connections watching web views, shared by every host's browser.
pub(crate) struct Hub {
    viewers: Mutex<HashMap<u64, mpsc::Sender<Event>>>,
    /// The frame size each connection settled on while pages moved (a
    /// step of `tab::LADDER`): the next tab it shows starts there.
    pub(crate) levels: Mutex<HashMap<u64, usize>>,
    ids: AtomicU64,
    browsers: Mutex<Vec<Weak<Shared>>>,
}

impl Hub {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            viewers: Mutex::new(HashMap::new()),
            levels: Mutex::new(HashMap::new()),
            ids: AtomicU64::new(1),
            browsers: Mutex::new(Vec::new()),
        })
    }

    /// A number no viewer or tab has had.
    pub(crate) fn next_id(&self) -> u64 {
        self.ids.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn viewer_tx(&self, id: u64) -> Option<mpsc::Sender<Event>> {
        self.viewers.lock().get(&id).cloned()
    }

    pub(crate) fn register(&self, s: &Arc<Shared>) {
        let mut b = self.browsers.lock();
        b.retain(|w| w.strong_count() > 0);
        b.push(Arc::downgrade(s));
    }

    pub(crate) fn drop_viewer(&self, id: u64) {
        self.viewers.lock().remove(&id);
        self.levels.lock().remove(&id);
        let browsers: Vec<Arc<Shared>> = self
            .browsers
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for s in browsers {
            Browser(s).detach_viewer(id);
        }
    }
}

/// Who acts for a session, as a machine is told (for its own check of who
/// may use it, and its log).
#[derive(Debug, Clone)]
pub struct ActingAs {
    /// The chat's title.
    pub chat: String,
    pub mode: PermissionMode,
    pub bot: Option<NodeBot>,
}

impl Default for ActingAs {
    fn default() -> Self {
        Self {
            chat: String::new(),
            mode: PermissionMode::ReadOnly,
            bot: None,
        }
    }
}

/// A tab this agent can't use now (e.g. Tessera was stopped on its machine
/// since its turn began): listed, but nothing of its page.
fn hidden_unless(mut t: AgentTab, ok: &Result<(), String>) -> AgentTab {
    if let Err(why) = ok {
        t.title.clear();
        t.url = format!("(not available: {why})");
        t.popup = None;
    }
    t
}

/// Every browser web views run in.
#[derive(Clone)]
pub struct Browsers(Arc<Fleet>);

struct Fleet {
    host: Weak<dyn Host>,
    events: broadcast::Sender<Event>,
    data_dir: PathBuf,
    hub: Arc<Hub>,
    server: Browser,
    machines: Mutex<HashMap<String, Browser>>,
    /// The tab each session's agent used last.
    recent: Mutex<HashMap<String, String>>,
    /// Headless tabs that became a board's (`t3` → its board id), so the
    /// agent's old name for one keeps working.
    moved: Mutex<HashMap<String, String>>,
    /// The device the user last used each chat on (by its root session).
    presence: Mutex<HashMap<String, String>>,
    /// Each machine's news, put back in order.
    order: Mutex<HashMap<String, Order>>,
}

/// A machine's batches of news: the next one due (none: whichever comes
/// first on a new connection), and those that came early.
#[derive(Default)]
struct Order {
    next: Option<u64>,
    early: BTreeMap<u64, Vec<NodeBrowserUp>>,
}

/// Batches that may come early before a missing one is given up on.
const MAX_EARLY: usize = 64;

impl Browsers {
    pub fn new(host: Weak<dyn Host>, events: broadcast::Sender<Event>, data_dir: &Path) -> Self {
        let hub = Hub::new();
        let server = Browser::local(host.clone(), events.clone(), data_dir, hub.clone());
        Self(Arc::new(Fleet {
            host,
            events,
            data_dir: data_dir.to_path_buf(),
            hub,
            server,
            machines: Mutex::new(HashMap::new()),
            recent: Mutex::new(HashMap::new()),
            moved: Mutex::new(HashMap::new()),
            presence: Mutex::new(HashMap::new()),
            order: Mutex::new(HashMap::new()),
        }))
    }

    /// This server's own browser.
    pub fn server(&self) -> &Browser {
        &self.0.server
    }

    fn host(&self) -> Result<Arc<dyn Host>, String> {
        self.0
            .host
            .upgrade()
            .ok_or_else(|| "the server is shutting down".to_string())
    }

    /// A new connection's view (see [`Viewer`]).
    pub fn viewer(&self) -> (Viewer, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(VIEWER_QUEUE);
        let id = self.0.hub.next_id();
        self.0.hub.viewers.lock().insert(id, tx);
        (
            Viewer {
                id,
                hub: Arc::downgrade(&self.0.hub),
            },
            rx,
        )
    }

    /// Every browser made so far.
    fn all(&self) -> Vec<Browser> {
        let mut v = vec![self.0.server.clone()];
        v.extend(self.0.machines.lock().values().cloned());
        v
    }

    /// The browser of machine `id` (made on first use).
    pub fn machine(&self, id: &str) -> Result<Browser, String> {
        if let Some(b) = self.0.machines.lock().get(id) {
            return Ok(b.clone());
        }
        let link = self
            .host()?
            .remote(id)
            .ok_or_else(|| "that machine isn't known here (any more)".to_string())?;
        // What the server keeps of each machine's browser (its measured
        // speed): a folder named for the machine, safely.
        let dir: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(64)
            .collect();
        let b = Browser::remote(
            self.0.host.clone(),
            self.0.events.clone(),
            self.0.data_dir.join("browser-machines").join(dir),
            self.0.hub.clone(),
            link,
        );
        Ok(self
            .0
            .machines
            .lock()
            .entry(id.to_string())
            .or_insert(b)
            .clone())
    }

    /// " on Desktop" for a machine's id, "" for this server.
    pub fn on_where(&self, machine: Option<&str>) -> String {
        machine
            .and_then(|m| self.machine(m).ok())
            .map(|b| b.on_where())
            .unwrap_or_default()
    }

    /// The browser on `host` (none: this server).
    pub fn on(&self, host: Option<&str>) -> Result<Browser, String> {
        match host {
            None => Ok(self.0.server.clone()),
            Some(id) => self.machine(id),
        }
    }

    /// The browser a board's web view runs in.
    pub fn for_board(&self, board_id: &str) -> Result<Browser, String> {
        let target = self.host()?.board_target(board_id)?;
        self.on(target.host.as_deref())
    }

    /// The browser holding a tab for `key` now (a board id or a tab).
    fn holding(&self, key: &str) -> Option<Browser> {
        self.all().into_iter().find(|b| b.find(key).is_some())
    }

    /// A client's `browser_*` request, for its board's host.
    pub async fn request(&self, req: Request, viewer: Option<&Viewer>) -> Result<Value, ApiError> {
        let board = match &req {
            Request::BrowserAttach { board_id, .. }
            | Request::BrowserDetach { board_id }
            | Request::BrowserInput { board_id, .. }
            | Request::BrowserNav { board_id, .. }
            | Request::BrowserPage { board_id, .. }
            | Request::BrowserNewTab { board_id, .. }
            | Request::BrowserUpload { board_id, .. }
            | Request::BrowserAck { board_id, .. }
            | Request::BrowserDialogAnswer { board_id, .. }
            | Request::BrowserChooseFiles { board_id, .. } => board_id.clone(),
            _ => {
                return Err(ApiError::new(
                    ErrorCode::BadRequest,
                    "not a browser request",
                ));
            }
        };
        // Leaving or acknowledging goes to wherever its tab is.
        if matches!(
            req,
            Request::BrowserDetach { .. } | Request::BrowserAck { .. }
        ) {
            return match self.holding(&board) {
                Some(b) => b.request(req, viewer).await,
                None => Ok(Value::Null),
            };
        }
        let host = self
            .host()
            .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
        let target = host
            .board_target(&board)
            .map_err(|e| ApiError::new(ErrorCode::BadRequest, e))?;
        if let Some(m) = &target.host {
            // The user's own devices: the machine must be there and still
            // allow web views (who may use it is about agents).
            host.machine_ok(None, m)
                .map_err(|e| ApiError::new(ErrorCode::BadRequest, e))?;
        }
        let b = self
            .on(target.host.as_deref())
            .map_err(|e| ApiError::new(ErrorCode::BadRequest, e))?;
        b.request(req, viewer).await
    }

    /// "Show on <machine>'s screen", asked by the user: the web view's page
    /// in an ordinary browser window on the machine its browser runs on,
    /// with the same sign-ins, for what only a person can finish there (a
    /// site's verification check). `show` false closes that window, and
    /// the tile carries on with what was done there. The machine checks
    /// it too (web views on, a desktop to show it on).
    pub async fn show_on_screen(&self, board_id: &str, show: bool) -> Result<(), String> {
        let host = self.host()?;
        let target = host.board_target(board_id)?;
        let Some(machine) = target.host.clone() else {
            return Err("this web view runs on the server, which has no screen to show it on. Open the page in your own browser instead (sign-ins there stay in that browser), or open web views on one of your computers (Web views here)".into());
        };
        host.machine_ok(None, &machine)?;
        let b = self.machine(&machine)?;
        let remote = b.0.remote.clone().ok_or("not a machine's browser")?;
        let url = show.then(|| {
            // The page as it is now (a popup on top: that one).
            b.find_state(board_id)
                .map(|t| t.url)
                .filter(|u| u.starts_with("http"))
                .unwrap_or(target.url)
        });
        remote.screen(url).await?;
        if !show {
            b.reopen_watched().await;
        }
        Ok(())
    }

    /// A board's address changed: its tab follows (or closes, if the board
    /// now belongs on another host). `agent`: the chat whose agent changed
    /// it, with its count of Stops on that machine then; a machine is asked
    /// first, right before the page goes there, as for any agent action.
    pub fn board_changed(&self, board_id: &str, agent: Option<(String, Option<u32>)>) {
        let Some(b) = self.holding(board_id) else {
            return;
        };
        let host = self.host().and_then(|h| h.board_target(board_id));
        match host {
            Ok(t) if t.host == b.machine() => match (agent, b.machine(), b.find(board_id)) {
                (Some((session, stops)), Some(_), Some(tab)) => {
                    let me = self.clone();
                    let board = board_id.to_string();
                    tokio::spawn(async move {
                        match me
                            .before_act_as(&tab, &session, stops, "navigate", &t.url)
                            .await
                        {
                            Ok(admitted) => b.board_changed_on(&board, admitted),
                            Err(e) => tab.notice(
                                tessera_proto::NoticeLevel::Warning,
                                &format!("The page wasn't opened: {e}"),
                            ),
                        }
                    });
                }
                _ => b.board_changed(board_id),
            },
            // Not a web page there any more: its page there closes (for an
            // agent's change, as that machine allows it, on that run).
            _ => match (agent, b.machine(), b.find(board_id).filter(|t| t.is_live())) {
                (Some((session, stops)), Some(_), Some(tab)) => {
                    let me = self.clone();
                    let board = board_id.to_string();
                    tokio::spawn(async move {
                        match me.before_act_as(&tab, &session, stops, "close", "").await {
                            Ok(admitted) => {
                                super::on_run(admitted, async { b.board_removed(&board) }).await
                            }
                            Err(e) => tab.notice(
                                tessera_proto::NoticeLevel::Warning,
                                &format!("The page wasn't closed: {e}"),
                            ),
                        }
                    });
                }
                _ => b.board_removed(board_id),
            },
        }
    }

    /// For `session`'s agent closing board `board_id`: when its page is open
    /// in a machine's browser, that machine is asked first (with the chat's
    /// turn's count of Stops there, taken now) and the run it allowed.
    pub async fn admit_board(
        &self,
        board_id: &str,
        session: &str,
        action: &str,
    ) -> Result<Option<u64>, String> {
        let Some(tab) = self
            .holding(board_id)
            .filter(|b| b.machine().is_some())
            .and_then(|b| b.find(board_id))
            .filter(|t| t.is_live())
        else {
            return Ok(None);
        };
        let mark = self.turn_mark(&tab, session);
        self.before_act_as(&tab, session, mark, action, "").await
    }

    /// A board went away (or moved to another host): its tab closes.
    pub fn board_removed(&self, board_id: &str) {
        for b in self.all() {
            b.board_removed(board_id);
        }
        self.0.moved.lock().retain(|_, b| b != board_id);
    }

    /// A chat went away: its agent's own tabs close everywhere.
    pub fn session_removed(&self, session_id: &str) {
        for b in self.all() {
            b.session_removed(session_id);
        }
        self.0.recent.lock().remove(session_id);
    }

    /// What a web view board's tab shows now, if it has a live one.
    pub async fn board_screenshot(&self, board_id: &str) -> Option<tessera_proto::SnapshotImage> {
        self.holding(board_id)?.board_screenshot(board_id).await
    }

    /// The same for `session`'s agent, whose turn's count of Stops on the
    /// board's machine is `stops`: asked of the machine first, as any agent
    /// action there, and taken only on the run it allowed. `Ok(None)`: no
    /// live page to look at.
    pub async fn board_screenshot_for(
        &self,
        board_id: &str,
        session: &str,
        stops: Option<u32>,
    ) -> Result<Option<tessera_proto::SnapshotImage>, String> {
        let Some(b) = self.holding(board_id) else {
            return Ok(None);
        };
        let Some(tab) = b.find(board_id).filter(|t| t.is_live()) else {
            return Ok(None);
        };
        let admitted = self
            .before_act_as(&tab, session, stops, "snapshot", "")
            .await?;
        Ok(super::on_run(admitted, b.board_screenshot(board_id)).await)
    }

    /// What Settings shows about this server's browser.
    pub async fn info(&self) -> tessera_proto::BrowserInfo {
        self.0.server.info().await
    }

    pub async fn quality_changed(&self) {
        for b in self.all() {
            b.quality_changed().await;
        }
    }

    pub async fn stop_if_unwatched(&self) {
        for b in self.all() {
            b.stop_if_unwatched().await;
        }
    }

    pub async fn shutdown(&self) {
        for b in self.all() {
            b.shutdown().await;
        }
    }

    /// Machine `id` went offline, was paused or revoked, or switched web
    /// views off (`why`, in plain words): its browser is gone.
    pub fn machine_gone(&self, id: &str, why: &str) {
        let b = self.0.machines.lock().get(id).cloned();
        if let Some(b) = b {
            b.remote_gone(why, false);
        }
    }

    /// News from machine `id`'s browser: batch `seq` of its connection
    /// (from 1), handled in order.
    pub fn machine_news(&self, id: &str, seq: u64, msgs: Vec<NodeBrowserUp>) {
        let ready = {
            let mut order = self.0.order.lock();
            let o = order.entry(id.to_string()).or_default();
            if seq <= 1 {
                // The machine counts from the start again.
                *o = Order::default();
            }
            let next = *o.next.get_or_insert(seq);
            if seq < next {
                return;
            }
            o.early.insert(seq, msgs);
            if o.early.len() > MAX_EARLY {
                tracing::warn!("news from machine {id} went missing; going on without it");
                o.next = o.early.keys().next().copied();
            }
            let mut ready = Vec::new();
            while let Some(n) = o.next
                && let Some(m) = o.early.remove(&n)
            {
                ready.push(m);
                o.next = Some(n + 1);
            }
            ready
        };
        let b = self.0.machines.lock().get(id).cloned();
        if let Some(b) = b {
            for m in ready {
                b.remote_feed(m);
            }
        }
    }

    /// Machine `id` said hello on a new connection: its news counts afresh,
    /// and a browser it ran before is gone (it stops one when its
    /// connection drops).
    pub fn machine_hello(&self, id: &str) {
        self.0.order.lock().remove(id);
        let b = self.0.machines.lock().get(id).cloned();
        if let Some(b) = b {
            b.remote_gone("it connected again", true);
        }
    }

    /// The user did something with chat `root` on `device`.
    pub fn note_presence(&self, root: &str, device: &str) {
        let mut p = self.0.presence.lock();
        if p.get(root).map(String::as_str) != Some(device) {
            if p.len() > 10_000 {
                p.clear();
            }
            p.insert(root.to_string(), device.to_string());
        }
    }

    /// The device the user last used chat `root` on.
    pub fn presence(&self, root: &str) -> Option<String> {
        self.0.presence.lock().get(root).cloned()
    }

    // ---- the agent's side ---------------------------------------------------

    /// Where `session`'s new web view goes: `on` (a machine's name, or
    /// "server"), else the machine of the device the user is using the chat
    /// on (when it allows it), else this server. `Err` when `on` names a
    /// machine that can't be used.
    pub fn place(&self, session: &str, on: Option<&str>) -> Result<Option<String>, String> {
        let host = self.host()?;
        match on.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) if is_server_word(s) => Ok(None),
            Some(name) => {
                let id = host
                    .find_machine(name)
                    .ok_or_else(|| format!("there is no machine called {name}"))?;
                host.machine_ok(Some(session), &id)?;
                Ok(Some(id))
            }
            None => Ok(host
                .placement(session)
                .filter(|m| host.machine_ok(Some(session), m).is_ok())),
        }
    }

    /// May `session`'s agent still use `tab` now (its machine's own rules
    /// and this chat's access there, checked again after waiting)?
    pub fn still_usable(&self, tab: &Tab, session: &str) -> Result<(), String> {
        match tab.host_browser() {
            Some(b) => self.usable(&b, session),
            None => Ok(()),
        }
    }

    /// May `session`'s agent use the tab in `b` now?
    fn usable(&self, b: &Browser, session: &str) -> Result<(), String> {
        match b.machine() {
            None => Ok(()),
            Some(m) => self.host()?.machine_ok(Some(session), &m),
        }
    }

    /// Tabs this session's agent may use: its own headless ones (on every
    /// host) and the web view boards it can see (`boards`, in its order).
    pub fn agent_tabs(&self, session: &str, boards: &[String]) -> Vec<AgentTab> {
        let mut out: Vec<AgentTab> = self
            .all()
            .iter()
            .flat_map(|b| {
                let ok = self.usable(b, session);
                b.own_tabs(session)
                    .into_iter()
                    .map(move |t| hidden_unless(describe(&t), &ok))
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        let Ok(host) = self.host() else { return out };
        for id in boards {
            match self.holding(id).and_then(|b| b.find(id).map(|t| (b, t))) {
                Some((b, t)) => out.push(hidden_unless(describe(&t), &self.usable(&b, session))),
                None => {
                    if let Ok(target) = host.board_target(id) {
                        let ok = match target.host.as_deref() {
                            Some(m) => host.machine_ok(Some(session), m),
                            None => Ok(()),
                        };
                        out.push(hidden_unless(
                            AgentTab {
                                key: id.clone(),
                                board: Some(id.clone()),
                                machine: target.host,
                                title: String::new(),
                                url: target.url,
                                watched: false,
                                live: false,
                                popup: None,
                            },
                            &ok,
                        ));
                    }
                }
            }
        }
        out
    }

    /// The tab an agent means: `key` (a board id or one of its own tabs),
    /// else the one it used last, else the only web view it can see. A tab
    /// on a machine must be usable by this session.
    pub fn agent_tab(
        &self,
        session: &str,
        key: Option<&str>,
        boards: &[String],
    ) -> Result<Arc<Tab>, String> {
        let key = match key.map(str::trim).filter(|k| !k.is_empty()) {
            // A headless tab that was put on screen goes by its board's id.
            Some(k) => match self.0.moved.lock().get(k) {
                Some(b) if boards.contains(b) => b.clone(),
                _ => k.to_string(),
            },
            None => match self.0.recent.lock().get(session).cloned() {
                Some(k) => k,
                None if boards.len() == 1 => boards[0].clone(),
                None if boards.is_empty() => {
                    return Err("no tab is open: `open` a url first".into());
                }
                None => {
                    return Err(format!(
                        "several web views are open ({}): pass `tab`",
                        boards.join(", ")
                    ));
                }
            },
        };
        if boards.contains(&key) {
            let b = self.for_board(&key)?;
            self.usable(&b, session).map_err(|e| web_view_on(&b, &e))?;
            let t = b.board_tab(&key)?;
            self.0.recent.lock().insert(session.to_string(), key);
            return Ok(t);
        }
        let own = self.all().into_iter().find_map(|b| {
            b.find(&key)
                .filter(|t| t.owner().as_deref() == Some(session))
        });
        match own {
            Some(t) => {
                if let Some(b) = t.host_browser() {
                    self.usable(&b, session).map_err(|e| web_view_on(&b, &e))?;
                }
                self.0.recent.lock().insert(session.to_string(), key);
                Ok(t)
            }
            None => {
                self.0.recent.lock().remove(session);
                match self.all().iter().find_map(|b| b.why_closed(&key, session)) {
                    Some(why) => Err(why),
                    None => Err(format!("no tab `{key}` here (`tabs` lists them)")),
                }
            }
        }
    }

    /// The tab of a board the agent just made (on its host).
    pub fn board_tab_for(&self, session: &str, board: &str) -> Result<Arc<Tab>, String> {
        let b = self.for_board(board)?;
        self.usable(&b, session).map_err(|e| web_view_on(&b, &e))?;
        let t = b.board_tab(board)?;
        self.0
            .recent
            .lock()
            .insert(session.to_string(), board.to_string());
        Ok(t)
    }

    /// A new headless tab for this session's agent on `host`.
    pub fn new_agent_tab(
        &self,
        session: &str,
        url: &str,
        host: Option<&str>,
    ) -> Result<Arc<Tab>, String> {
        let b = self.on(host)?;
        self.usable(&b, session)?;
        let tab = b.new_agent_tab(session, url)?;
        self.0.recent.lock().insert(session.to_string(), tab.key());
        Ok(tab)
    }

    /// Put one of an agent's headless tabs on a board (its tile), keeping
    /// the page as it is (the board must be on the tab's host). The agent's
    /// name for the tab keeps working, and its current tab stays the same.
    pub fn adopt(&self, tab: &Arc<Tab>, board: &str, chat: &str, session: &str) {
        let old = tab.key();
        if let Some(b) = tab.host_browser() {
            b.adopt(tab, board, chat);
        }
        if old != board {
            self.0.moved.lock().insert(old.clone(), board.to_string());
        }
        let mut recent = self.0.recent.lock();
        if recent.get(session).is_none_or(|k| *k == old) {
            recent.insert(session.to_string(), board.to_string());
        }
    }

    /// The board a headless tab became (`show`n or watched), if it did.
    pub fn moved_to(&self, key: &str) -> Option<String> {
        self.0.moved.lock().get(key).cloned()
    }

    /// One of `session`'s agent's own headless tabs, by its key (its
    /// current tab stays the same).
    pub fn own_tab(&self, session: &str, key: &str) -> Option<Arc<Tab>> {
        self.all().into_iter().find_map(|b| {
            b.find(key)
                .filter(|t| t.owner().as_deref() == Some(session))
        })
    }

    /// Close one of an agent's headless tabs.
    pub fn close_agent_tab(&self, session: &str, key: &str) -> bool {
        {
            let mut recent = self.0.recent.lock();
            if recent.get(session).map(String::as_str) == Some(key) {
                recent.remove(session);
            }
        }
        self.all().iter().any(|b| b.close_own_tab(session, key))
    }

    pub fn forget_recent(&self, session: &str) {
        self.0.recent.lock().remove(session);
    }

    /// Before the agent acts on `tab` (`action` on `target`): on a machine,
    /// may this session use it now, and does the machine agree (it checks
    /// who may use it and its own state, and logs the action)?
    pub async fn before_act(
        &self,
        tab: &Tab,
        session: &str,
        action: &str,
        target: &str,
    ) -> Result<(), String> {
        let mark = self.turn_mark(tab, session);
        self.before_act_as(tab, session, mark, action, target)
            .await
            .map(|_| ())
    }

    /// The count of the user's Stops on `tab`'s machine that `session`'s
    /// latest turn was accepted under: taken when an agent's work begins,
    /// so it stays that work's even if another turn starts meanwhile.
    pub fn turn_mark(&self, tab: &Tab, session: &str) -> Option<u32> {
        let machine = tab.host_browser()?.machine()?;
        self.host().ok()?.turn_stops(session, &machine)
    }

    /// `before_act` for work that began under `stops` (`turn_mark`). Asked
    /// of the machine's browser itself, before anything is loaded: a tab to
    /// bring back isn't loaded until this is allowed. The run of that
    /// browser it was allowed on (`None`: this server's browser): the work
    /// goes on only on that run (`Tab::ensure_live_on`).
    pub async fn before_act_as(
        &self,
        tab: &Tab,
        session: &str,
        stops: Option<u32>,
        action: &str,
        target: &str,
    ) -> Result<Option<u64>, String> {
        let Some(b) = tab.host_browser() else {
            return Ok(None);
        };
        self.admit_in(&b, session, stops, action, target, tab.board())
            .await
    }

    /// The count of Stops on `host`'s machine `session`'s latest turn was
    /// accepted under (`None`: this server's browser, or unknown).
    pub fn turn_mark_on(&self, host: Option<&str>, session: &str) -> Option<u32> {
        self.host().ok()?.turn_stops(session, host?)
    }

    /// `before_act_as` before there is a tab: for a new one on `host` (its
    /// room made only on the run allowed).
    pub async fn admit_on(
        &self,
        host: Option<&str>,
        session: &str,
        stops: Option<u32>,
        action: &str,
        target: &str,
    ) -> Result<Option<u64>, String> {
        let b = self.on(host)?;
        self.admit_in(&b, session, stops, action, target, None)
            .await
    }

    async fn admit_in(
        &self,
        b: &Browser,
        session: &str,
        stops: Option<u32>,
        action: &str,
        target: &str,
        board: Option<String>,
    ) -> Result<Option<u64>, String> {
        let Some(machine) = b.machine() else {
            return Ok(None);
        };
        let host = self.host()?;
        host.machine_ok(Some(session), &machine)
            .map_err(|e| web_view_on(b, &e))?;
        let who = host.acting_as(session);
        let live = b.live().await?;
        live.cdp
            .call(
                None,
                "Tessera.act",
                json!({
                    "session": session,
                    "chat": who.chat,
                    "mode": who.mode,
                    "bot": who.bot,
                    "stops": stops,
                    "action": action,
                    "target": crate::util::one_line(target, 300),
                    "board": board,
                }),
            )
            .await
            .map(|_| Some(live.generation))
            .map_err(|e| format!("{} refused: {e}", b.on_where().trim_start_matches(" on ")))
    }
}

/// "The web view runs on Desktop, which …": an error about a tab on a
/// machine, saying which.
fn web_view_on(b: &Browser, why: &str) -> String {
    match b.on_where() {
        w if w.is_empty() => why.to_string(),
        w => format!("this web view runs{w}: {why}"),
    }
}

/// "server", "the server", "here", "my-server (server)"…: this server.
fn is_server_word(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "server" | "the server" | "here" | "this server" | "tessera"
    )
}

impl Browser {
    /// A machine's browser: news from it.
    pub(crate) fn remote_feed(&self, msgs: Vec<NodeBrowserUp>) {
        if let Some(r) = &self.0.remote {
            r.feed(msgs);
            // The user closed the window on its screen: the tiles carry on.
            if r.screen_closed.swap(false, Ordering::Relaxed) {
                let b = self.clone();
                tokio::spawn(async move { b.reopen_watched().await });
            }
        }
    }

    /// A machine's browser: the machine is gone (offline, paused, revoked,
    /// web views off).
    pub(crate) fn remote_gone(&self, why: &str, reopen: bool) {
        if let Some(r) = &self.0.remote {
            r.gone(why, reopen);
        }
    }
}
