//! Web views: client requests (`browser_*`), the hooks that keep tabs in
//! step with their boards, approvals the `browser` tool asks for while it
//! runs, and where each web view runs.
//!
//! A web view runs in this server's browser, or in the browser of the
//! machine the user is using the chat on, when that machine has "Web views
//! here" on and the chat may use it (`Whiteboard::host`, decided once when
//! the board is made; see `crate::browser::hosts`). Which device the user
//! is using a chat on is the one that last did something with it here:
//! opened it, sent in it, answered an approval, made or looked at one of its
//! web views. The machine's own end of it is `crate::node::browser`.

use super::*;
use crate::browser::{ActingAs, BoardTarget, BrowserConfig, Browsers, Remote, Viewer};

impl Engine {
    /// A connection's own view of the browsers: frames, focus and dialogs
    /// of the web views it attaches to arrive on the receiver. Pass the
    /// viewer to [`Engine::handle_as`]; dropping it detaches.
    pub fn browser_viewer(&self) -> (Viewer, mpsc::Receiver<Event>) {
        self.0.browser.viewer()
    }

    /// [`Engine::handle`] for a connection with a browser view: its
    /// `browser_*` requests (attach, ack…) act for that connection.
    pub async fn handle_as(
        &self,
        req: Request,
        device: Option<&str>,
        viewer: Option<&Viewer>,
    ) -> ApiResult {
        if is_browser_request(&req) {
            // Another device's private chat's web view is as if it didn't exist.
            self.0.check_private(&req, device)?;
            self.0.note_presence(&req, device);
            return self.0.clone().browser_request(req, viewer).await;
        }
        self.handle(req, device).await
    }

    /// This server's own browser.
    pub fn browser(&self) -> &crate::browser::Browser {
        self.0.browser.server()
    }

    /// Every browser web views run in (this server's and machines').
    pub fn browsers(&self) -> &Browsers {
        &self.0.browser
    }
}

fn is_browser_request(req: &Request) -> bool {
    matches!(
        req,
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
            | Request::BrowserWatch { .. }
    )
}

impl Inner {
    pub(super) async fn browser_request(
        self: Arc<Self>,
        req: Request,
        viewer: Option<&Viewer>,
    ) -> ApiResult {
        match req {
            Request::BrowserToServer { board_id } => to_value(self.web_view_to_server(&board_id)?),
            Request::BrowserShowOnScreen { board_id, show } => {
                self.browser
                    .show_on_screen(&board_id, show)
                    .await
                    .map_err(bad)?;
                Ok(Value::Null)
            }
            Request::BrowserWatch { session_id, tab } => {
                to_value(self.watch_tab(&session_id, &tab).await.map_err(bad)?)
            }
            req => self.browser.request(req, viewer).await,
        }
    }

    /// The user watches one of the agent's background tabs (a `browser`
    /// step's "Watch"): it becomes a web view of theirs, the page as it is,
    /// and the agent carries on in it. Placed by the device that asked.
    async fn watch_tab(&self, session_id: &str, key: &str) -> Result<Whiteboard, String> {
        self.rt(session_id).map_err(|e| e.message)?;
        let root = self.root_session(session_id);
        // Already on a board (`show`n, or watched before): that board.
        let board = self
            .browser
            .moved_to(key)
            .unwrap_or_else(|| key.to_string());
        if let Ok(Some(b)) = self.store.board(&board)
            && self.root_session(&b.session_id) == root
        {
            return Ok(b);
        }
        let tab = self
            .browser
            .own_tab(session_id, key)
            .ok_or("that tab is closed")?;
        let url = tab.url();
        if !url.starts_with("http") {
            return Err("that tab has no page yet".into());
        }
        let title = Some(tab.title()).filter(|t| !t.is_empty());
        let board = self.put_watched_board(session_id, title.as_deref(), &url, tab.machine())?;
        self.browser.adopt(&tab, &board.id, &root, session_id);
        tab.sync_view().await;
        Ok(board)
    }

    /// Move a web view to this server's browser (its machine is offline or
    /// no longer lets it run there): it opens its address here.
    fn web_view_to_server(&self, board_id: &str) -> Result<Whiteboard, ApiError> {
        let mut b = self
            .store
            .board(board_id)
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "that web view is gone"))?;
        if !matches!(
            b.content,
            BoardContent::Url { .. } | BoardContent::Local { .. }
        ) {
            return Err(bad("that whiteboard isn't a web view"));
        }
        let Some(from) = b.host.take() else {
            return Ok(b);
        };
        b.version += 1;
        b.updated_at = now_ms();
        self.store.put_board(&b).map_err(internal)?;
        // Its tab there closes; the next device to show it opens it here.
        self.browser.board_removed(board_id);
        tracing::info!("web view {board_id} moved from machine {from} to this server");
        let _ = self.events.send(Event::BoardUpdated { board: b.clone() });
        Ok(b)
    }

    /// Ask the user mid-call (see [`ToolHost::approve`]).
    pub(super) async fn approve_now(
        self: Arc<Self>,
        q: crate::tools::Approve,
    ) -> Result<(), String> {
        let rt = self
            .rt(&q.session_id)
            .map_err(|_| ApprovalOutcome::Dismissed.message(&q.tool))?;
        let tool = q.tool.clone();
        // Nothing is allowed across a Stop on this computer: not while
        // stopped, not for a turn from before one, nor an answer given
        // after one.
        let stops_at = rt.st.lock().turn_stops.clone();
        if !self.running_here_since(&stops_at) {
            approval::audit(
                &q.session_id,
                &tool,
                &Answer::rule(ApprovalOutcome::Stopped),
            );
            return Err(ApprovalOutcome::Stopped.message(&tool));
        }
        let (mode, approvals_on) = {
            let st = rt.st.lock();
            (self.effective_permission(&st), st.approvals_on.clone())
        };
        // What the answer is given under: the chat's mode and its bot's
        // settings (its access to machines among them). Changed while it
        // waited, the answer allows nothing (asked again under the new ones).
        let policy = |st: &RtState| {
            (
                self.effective_permission(st),
                self.speaker_bot(st).map(|b| (b.id, b.updated_at)),
            )
        };
        let asked_under = policy(&rt.st.lock());
        if mode == PermissionMode::Auto && !q.always {
            return Ok(());
        }
        let keys = self.session_keys(&rt.st.lock(), &q.key);
        // Asked every time: nothing allowed for the session earlier counts.
        if !q.once && Self::session_allowed(&rt.st.lock(), &keys) {
            return Ok(());
        }
        let request = ApprovalRequest {
            id: new_id(),
            call_id: q.call_id,
            tool: q.tool,
            summary: crate::util::visible_controls(&q.summary),
            reason: crate::util::visible_controls(&q.reason),
            arguments: q.arguments,
            once: q.once,
        };
        let decision = self
            .ask_approval(&rt, &approvals_on, request, &q.cancel)
            .await;
        if decision.allowed() && !self.running_here_since(&stops_at) {
            self.notice(
                &rt,
                NoticeLevel::Warning,
                "Not done: Tessera was stopped on this computer while it waited for your answer.",
            );
            approval::audit(
                &q.session_id,
                &tool,
                &Answer::rule(ApprovalOutcome::Stopped),
            );
            return Err(ApprovalOutcome::Stopped.message(&tool));
        }
        if decision.allowed() && policy(&rt.st.lock()) != asked_under {
            self.notice(
                &rt,
                NoticeLevel::Warning,
                "Not done: this chat's permissions (or its bot's access) changed while it waited for your answer.",
            );
            let outcome = ApprovalOutcome::Rule("This chat's permissions or its bot's access changed; submit new work under the current settings.".into());
            approval::audit(&q.session_id, &tool, &Answer::rule(outcome.clone()));
            return Err(outcome.message(&tool));
        }
        match decision {
            ApprovalOutcome::Allowed(ApprovalDecision::AllowSession) if !q.once => {
                Self::allow_for_session(&mut rt.st.lock(), keys);
                self.save(&rt);
                Ok(())
            }
            ApprovalOutcome::Allowed(_) => Ok(()),
            outcome => Err(outcome.message(&tool)),
        }
    }

    // ---- where web views run ----------------------------------------------

    /// Note which device the user is using a chat on (the latest that
    /// opened it, sent in it, answered in it, or made or showed one of its
    /// web views). Reloads after a reconnect don't count.
    pub(super) fn note_presence(&self, req: &Request, device: Option<&str>) {
        let Some(device) = device else { return };
        let session = match req {
            Request::GetSession {
                session_id,
                since_seq: None,
            }
            | Request::GetSessionPage {
                session_id,
                before_seq: None,
                ..
            }
            | Request::Send { session_id, .. }
            | Request::RespondApproval { session_id, .. }
            | Request::SteerQueued { session_id, .. }
            | Request::InterruptQueued { session_id, .. }
            | Request::CreateBoard { session_id, .. } => session_id.clone(),
            Request::BrowserAttach { board_id, .. } | Request::BrowserNav { board_id, .. } => {
                match self.store.board(board_id) {
                    Ok(Some(b)) => b.session_id,
                    _ => return,
                }
            }
            _ => return,
        };
        self.browser
            .note_presence(&self.root_session(&session), device);
    }

    /// The host of a new board of `session` showing `content`: for a web
    /// page, the machine of the device the user is using the chat on when
    /// it allows it now; otherwise this server (`local` pages are the
    /// server's own loopback). `by_user`: the user's own (the chat's turns
    /// don't count for it).
    pub(super) fn web_host(
        &self,
        session_id: &str,
        content: &BoardContent,
        by_user: bool,
    ) -> Option<String> {
        if !matches!(content, BoardContent::Url { .. }) {
            return None;
        }
        use crate::browser::Host;
        self.placement(session_id).filter(|m| {
            self.machine_web_check(Some(session_id), m, !by_user)
                .is_ok()
        })
    }

    /// `session`'s latest turn's count of the user's Stops on `machine` (0
    /// for a machine it didn't know); `None` for no such chat.
    pub(super) fn turn_stops_on(&self, session: &str, machine: &str) -> Option<u32> {
        let rt = self.rt(session).ok()?;
        let st = rt.st.lock();
        Some(st.turn_stops.machines.get(machine).copied().unwrap_or(0))
    }

    /// A web page tile on `host` for the `browser` tool (already placed).
    pub(super) fn web_board(
        &self,
        session_id: &str,
        title: Option<&str>,
        url: &str,
        host: Option<String>,
    ) -> Result<Whiteboard, String> {
        if let Some(h) = &host {
            self.machine_web_ok(Some(session_id), h)?;
        }
        self.put_web_board(session_id, title, url, host)
    }

    /// May web views run on machine `id` now: it is online, not paused, has
    /// "Web views here" on and (for an agent, `session`) the chat may use
    /// it. The user's own devices (`None`) only need the first three.
    pub(super) fn machine_web_ok(&self, session: Option<&str>, id: &str) -> Result<(), String> {
        self.machine_web_check(session, id, true)
    }

    /// The same; `turns`: an agent's (its turn's Stop counts apply), not a
    /// web view the user opened themselves.
    fn machine_web_check(
        &self,
        session: Option<&str>,
        id: &str,
        turns: bool,
    ) -> Result<(), String> {
        let m = self
            .machine_list()
            .into_iter()
            .find(|m| m.id == id)
            .ok_or("that machine isn't known here any more")?;
        if !m.online {
            return Err(format!("{} is offline", m.name));
        }
        if m.paused {
            return Err(format!(
                "{} is paused; it can be resumed in Settings → Devices",
                m.name
            ));
        }
        if !m.scope.webviews {
            return Err(format!(
                "web views are off on {}; only that computer can turn them on",
                m.name
            ));
        }
        if let Some(s) = session {
            // An agent's work from before the user's latest Stop there (its
            // chat's latest turn, also something of it still going after that
            // turn ended) doesn't use its browser again, as for everything
            // else there (`machine_call`); the machine checks it too.
            let turn = turns.then(|| self.turn_stops_on(s, &m.id)).flatten();
            if turn.is_some_and(|t| t < m.scope.stops) {
                return Err(format!(
                    "{}: Tessera was stopped on that computer after this turn began, so this was not done. Don't retry; tell the user (a new message after they resume it there works as usual).",
                    m.name
                ));
            }
            let level = self
                .machine_access(s, &m.id, &m.scope)
                .map_err(|e| format!("{}: {e}", m.name))?;
            // A browser there acts on its network: more than reading.
            if level != MachineAccess::Allowed {
                let bot = self
                    .rt(s)
                    .ok()
                    .and_then(|rt| self.speaker_bot(&rt.st.lock()));
                return Err(format!(
                    "{}: {}",
                    m.name,
                    match bot {
                        Some(b) => crate::bots::read_only_machine(&b, "web views"),
                        None => "web views there need more than read-only access".into(),
                    }
                ));
            }
        }
        Ok(())
    }

    /// News from a machine's web view browser (its own connection only).
    pub(super) fn node_browser(&self, device: &str, seq: u64, msgs: Vec<NodeBrowserUp>) {
        self.browser.machine_news(device, seq, msgs);
    }
}

/// The way to one machine's web view browser: its live connection.
struct MachineLink {
    engine: Weak<Inner>,
    id: String,
}

impl Remote for MachineLink {
    fn machine(&self) -> String {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.engine
            .upgrade()
            .and_then(|e| {
                e.machine_list()
                    .into_iter()
                    .find(|m| m.id == self.id)
                    .map(|m| m.name)
            })
            .unwrap_or_else(|| "the machine".into())
    }

    fn send(&self, msgs: Vec<NodeBrowserDown>) -> BoxFuture<'static, Result<(), String>> {
        let tx = self.engine.upgrade().and_then(|e| e.machine_tx(&self.id));
        let name = self.name();
        Box::pin(async move {
            let tx = tx.ok_or_else(|| format!("{name} is offline"))?;
            tx.send(Event::NodeBrowser { msgs })
                .await
                .map_err(|_| format!("{name} went offline"))
        })
    }
}

impl crate::browser::Host for Inner {
    fn browser_config(&self) -> BrowserConfig {
        self.config.read().browser.clone()
    }

    fn board_target(&self, board_id: &str) -> Result<BoardTarget, String> {
        let b = self
            .store
            .board(board_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no web view `{board_id}`"))?;
        let (url, host) = match b.content {
            BoardContent::Url { url } => (url, b.host),
            // This server's own loopback, whatever made it.
            BoardContent::Local { port, path } => (format!("http://127.0.0.1:{port}{path}"), None),
            _ => return Err("that whiteboard isn't a web view".into()),
        };
        Ok(BoardTarget {
            url,
            session_id: self.root_session(&b.session_id),
            host,
        })
    }

    fn linked_board(&self, board_id: &str, url: &str) -> Result<tessera_proto::Whiteboard, String> {
        let b = self
            .store
            .board(board_id)
            .map_err(|_| "can't find this web view")?
            .ok_or("this web view closed")?;
        self.put_watched_board(&b.session_id, None, url, b.host)
    }

    fn tab_group(&self, id: &str) -> Vec<String> {
        let Ok(Some(board)) = self.store.board(id) else {
            return Vec::new();
        };
        let Ok(Some(raw)) = self.store.kv_get(&format!("webtabs:{id}")) else {
            return Vec::new();
        };
        let Ok(ids) = serde_json::from_str::<Vec<String>>(&raw) else {
            return Vec::new();
        };
        ids.into_iter()
            .take(16)
            .filter(|id| {
                self.store
                    .board(id)
                    .ok()
                    .flatten()
                    .is_some_and(|b| b.session_id == board.session_id && b.host == board.host)
            })
            .collect()
    }
    fn save_tab_group(&self, ids: &[String]) {
        if let Ok(raw) = serde_json::to_string(ids) {
            for id in ids {
                let _ = self.store.kv_set(&format!("webtabs:{id}"), &raw);
            }
        }
    }

    fn board_navigated(&self, board_id: &str, url: &str) {
        let Ok(Some(mut b)) = self.store.board(board_id) else {
            return;
        };
        let Ok(parsed) = url::Url::parse(url) else {
            return;
        };
        if !matches!(parsed.scheme(), "http" | "https") || url.len() > 2048 {
            return;
        }
        let changed = match &mut b.content {
            BoardContent::Url { url: u } if u != url => {
                *u = url.to_string();
                true
            }
            // A local app's tile follows its own pages only.
            BoardContent::Local { port, path }
                if matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
                    && parsed.port() == Some(*port) =>
            {
                let p = match parsed.query() {
                    Some(q) => format!("{}?{q}", parsed.path()),
                    None => parsed.path().to_string(),
                };
                let moved = *path != p;
                *path = p;
                moved
            }
            _ => false,
        };
        // Mere navigation: no version bump (tiles don't reload), and the
        // tab's `browser_state` already told every client.
        if changed {
            let _ = self.store.put_board(&b);
        }
    }

    fn own_port(&self) -> Option<u16> {
        Inner::own_port(self)
    }

    fn downloaded(&self, done: &crate::browser::Downloaded) -> bool {
        self.browser_downloaded(done)
    }

    fn remote(&self, machine: &str) -> Option<Arc<dyn Remote>> {
        self.machine_list()
            .iter()
            .any(|m| m.id == machine)
            .then(|| {
                Arc::new(MachineLink {
                    engine: self.me.clone(),
                    id: machine.to_string(),
                }) as Arc<dyn Remote>
            })
    }

    fn machine_ok(&self, session: Option<&str>, machine: &str) -> Result<(), String> {
        self.machine_web_ok(session, machine)
    }

    fn placement(&self, session: &str) -> Option<String> {
        // The machine of the device the user is using the chat on: the same
        // paired device (the desktop app hands its own pairing to the
        // machine it turns on).
        let device = self.browser.presence(&self.root_session(session))?;
        self.machine_list()
            .into_iter()
            .find(|m| m.id == device)
            .map(|m| m.id)
    }

    fn find_machine(&self, name: &str) -> Option<String> {
        let name = name.trim();
        let list = self.machine_list();
        list.iter()
            .find(|m| m.id == name)
            .or_else(|| list.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
            .map(|m| m.id.clone())
    }

    fn acting_as(&self, session: &str) -> ActingAs {
        let (chat, mode, bot) = self.chat_of(session);
        ActingAs { chat, mode, bot }
    }

    fn turn_stops(&self, session: &str, machine: &str) -> Option<u32> {
        self.turn_stops_on(session, machine)
    }
}
