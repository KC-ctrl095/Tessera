//! Whiteboards: what an agent shows the user in a tile of its own (a visual
//! explanation, a web page, an image, a web app it is building) and the
//! input it asks for there (a signature, a choice…).
//!
//! Board content is untrusted: clients render HTML on a separate origin with
//! no network access (see `tessera_proto::board`), and a `Local` board only
//! reaches one loopback port on this machine.

use std::time::Duration;

use base64::Engine as _;

use super::*;

const MAX_HTML: usize = 2 << 20;
const MAX_MARKDOWN: usize = 512 << 10;
const MAX_IMAGE: usize = 10 << 20;
const MAX_SIGNATURE: usize = 2 << 20;
const MAX_BOARDS: usize = 50;
const MAX_PROXIED: usize = 20 << 20;
const ASK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long `look` waits for a device to send what a board's tile shows.
const LOOK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SNAPSHOT: usize = 4 << 20;
const MAX_SNAPSHOT_SIDE: u32 = 1600;

/// Answers to one `board_snapshot_request`, from any device.
pub(super) type SnapshotTx = mpsc::UnboundedSender<Result<SnapshotImage, String>>;

/// A question waiting on a board.
pub(super) struct PendingAsk {
    board_id: String,
    tx: oneshot::Sender<AskAnswer>,
    identity: Arc<()>,
}

/// A dropped ask still releases its sender and its own stored question.
/// Identity checks keep an old future from removing a replacement request.
struct AskWait<'a> {
    inner: &'a Inner,
    board_id: String,
    ask_id: String,
    identity: Arc<()>,
}

impl Drop for AskWait<'_> {
    fn drop(&mut self) {
        {
            let mut pending = self.inner.board_asks.lock();
            match pending.get(&self.ask_id) {
                Some(p) if !Arc::ptr_eq(&p.identity, &self.identity) => return,
                Some(_) => {
                    pending.remove(&self.ask_id);
                }
                None => {} // answered or deleted: still clear this question, if present
            }
        }
        match self
            .inner
            .store
            .clear_board_ask(&self.board_id, &self.ask_id)
        {
            Ok(Some(board)) => {
                let _ = self.inner.events.send(Event::BoardUpdated { board });
            }
            Err(e) => tracing::warn!("clearing whiteboard question {}: {e}", self.ask_id),
            _ => {}
        }
    }
}

struct SnapshotWait<'a> {
    inner: &'a Inner,
    request_id: String,
    tx: SnapshotTx,
}

impl Drop for SnapshotWait<'_> {
    fn drop(&mut self) {
        let mut pending = self.inner.snapshots.lock();
        if pending
            .get(&self.request_id)
            .is_some_and(|tx| tx.same_channel(&self.tx))
        {
            pending.remove(&self.request_id);
        }
    }
}

impl Inner {
    pub(super) async fn board_request(
        self: Arc<Self>,
        req: Request,
        device: Option<&str>,
    ) -> ApiResult {
        match req {
            Request::ListBoards { session_id } => {
                let mut list = self.store.boards(session_id.as_deref()).map_err(internal)?;
                // Never another device's private chat's.
                list.retain(|b| self.device_sees(&b.session_id, device));
                to_value(list)
            }
            Request::GetBoard { id } => to_value(self.board(&id)?),
            Request::CreateBoard {
                session_id,
                title,
                content,
                placed,
            } => {
                self.rt(&session_id)?;
                to_value(
                    self.put_board(
                        &session_id,
                        None,
                        Some(&title),
                        content,
                        true,
                        !placed,
                        None,
                    )
                    .map_err(bad)?,
                )
            }
            Request::DeleteBoard { id } => {
                self.board(&id)?;
                self.delete_board(&id);
                Ok(Value::Null)
            }
            Request::AnswerBoard { id, ask_id, answer } => {
                self.answer_board(&id, &ask_id, answer)?;
                Ok(Value::Null)
            }
            Request::BoardFetch {
                id,
                method,
                path,
                headers,
                body,
            } => to_value(self.board_fetch(&id, &method, &path, headers, body).await?),
            Request::BoardSnapshot {
                request_id,
                image,
                error,
            } => {
                let tx = self
                    .snapshots
                    .lock()
                    .get(&request_id)
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::new(ErrorCode::NotFound, "that snapshot is no longer wanted")
                    })?;
                let answer = match (image, error) {
                    (Some(img), _) => Ok(check_snapshot(img).map_err(bad)?),
                    (None, e) => Err(one_line(
                        e.as_deref().unwrap_or("the device sent nothing"),
                        200,
                    )),
                };
                let _ = tx.send(answer);
                Ok(Value::Null)
            }
            Request::BoardLink { id } => {
                let b = self.board(&id)?;
                if !matches!(b.content, BoardContent::Html { .. }) {
                    return Err(bad("only HTML boards are served as pages"));
                }
                to_value(
                    json!({ "path": format!("/board/{id}/{}/", self.board_token(&id).map_err(internal)?) }),
                )
            }
            _ => Err(bad("not a whiteboard request")),
        }
    }

    fn board(&self, id: &str) -> Result<Whiteboard, ApiError> {
        self.store
            .board(id)
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no whiteboard `{id}`")))
    }

    /// The chat a board belongs with in the layout: subagents show theirs
    /// next to the conversation that started them.
    pub(super) fn root_session(&self, id: &str) -> String {
        let mut id = id.to_string();
        for _ in 0..8 {
            match self.store.get_session(&id) {
                Ok(Some((info, _))) if info.kind == SessionKind::Subagent => match info.parent_id {
                    Some(p) => id = p,
                    None => break,
                },
                _ => break,
            }
        }
        id
    }

    fn check_content(&self, content: BoardContent) -> Result<BoardContent, String> {
        Ok(match content {
            BoardContent::Html { html } => {
                if html.trim().is_empty() || html.len() > MAX_HTML {
                    return Err(format!("the page must be 1 byte to {} MB", MAX_HTML >> 20));
                }
                BoardContent::Html { html }
            }
            BoardContent::Markdown { markdown } => {
                if markdown.trim().is_empty() || markdown.len() > MAX_MARKDOWN {
                    return Err(format!(
                        "markdown must be 1 byte to {} KB",
                        MAX_MARKDOWN >> 10
                    ));
                }
                BoardContent::Markdown { markdown }
            }
            BoardContent::Image { mime, data } => {
                let mime = mime.trim().to_ascii_lowercase();
                if !matches!(
                    mime.as_str(),
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/svg+xml"
                ) {
                    return Err("images must be PNG, JPEG, GIF, WebP or SVG".into());
                }
                let data = data
                    .split_once(";base64,")
                    .map_or(data.as_str(), |(_, d)| d)
                    .split_whitespace()
                    .collect::<String>();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&data)
                    .map_err(|_| "the image isn't valid base64".to_string())?;
                if bytes.is_empty() || bytes.len() > MAX_IMAGE {
                    return Err(format!("images must be under {} MB", MAX_IMAGE >> 20));
                }
                BoardContent::Image { mime, data }
            }
            BoardContent::Url { url } => {
                let u = url::Url::parse(url.trim()).map_err(|_| format!("`{url}` isn't a URL"))?;
                if !matches!(u.scheme(), "http" | "https")
                    || u.host_str().is_none()
                    || url.len() > 2048
                {
                    return Err("only http(s) addresses can be shown".into());
                }
                BoardContent::Url { url: u.to_string() }
            }
            BoardContent::Local { port, path } => {
                let path = if path.trim().is_empty() {
                    "/".to_string()
                } else {
                    path.trim().to_string()
                };
                if !path.starts_with('/') || path.contains("://") || path.len() > 2048 {
                    return Err("`local_path` must start with /".into());
                }
                if port == 0 || Some(port) == self.own_port() {
                    return Err("choose the port your web app listens on".into());
                }
                BoardContent::Local { port, path }
            }
        })
    }

    pub(super) fn own_port(&self) -> Option<u16> {
        let bind = self
            .listen
            .read()
            .clone()
            .unwrap_or_else(|| self.config.read().server.bind.clone());
        bind.parse::<std::net::SocketAddr>().ok().map(|a| a.port())
    }

    /// A board of this conversation (or one of its subagents), for updating.
    fn own_board(&self, session_id: &str, id: &str) -> Result<Whiteboard, String> {
        let b = self
            .store
            .board(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no whiteboard `{id}`"))?;
        if self.root_session(&b.session_id) != self.root_session(session_id) {
            return Err(format!("no whiteboard `{id}` in this conversation"));
        }
        Ok(b)
    }

    /// An agent creates or updates a board. It shows in the chat as a card
    /// the user can open; `tile` also puts its tile beside the chat (only
    /// when the agent was asked to show it: an agent's work never pops one).
    pub(super) fn show_board(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: BoardContent,
        tile: bool,
    ) -> Result<Whiteboard, String> {
        self.put_board(session_id, id, title, content, false, tile, None)
    }

    /// `show_board`, a new board's page on `host` when given (decided and
    /// asked of that machine before, see `tools/board.rs`).
    pub(super) fn show_board_on(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: BoardContent,
        tile: bool,
        host: Option<Option<String>>,
    ) -> Result<Whiteboard, String> {
        self.put_board(session_id, id, title, content, false, tile, host)
    }

    /// A new web page tile whose browser runs on `host` (a machine's id;
    /// none: this server), already checked (see `engine/webview.rs`).
    pub(super) fn put_web_board(
        &self,
        session_id: &str,
        title: Option<&str>,
        url: &str,
        host: Option<String>,
    ) -> Result<Whiteboard, String> {
        let content = BoardContent::Url {
            url: url.to_string(),
        };
        self.put_board(session_id, None, title, content, false, true, Some(host))
    }

    /// The web view the user made of an agent's tab to watch it (see
    /// `watch_tab`): theirs, placed by their device.
    pub(super) fn put_watched_board(
        &self,
        session_id: &str,
        title: Option<&str>,
        url: &str,
        host: Option<String>,
    ) -> Result<Whiteboard, String> {
        let content = BoardContent::Url {
            url: url.to_string(),
        };
        self.put_board(session_id, None, title, content, true, false, Some(host))
    }

    /// Save a board; `place` also puts its tile beside its chat's. A new
    /// web view's browser runs on `host`, or where the user is using the
    /// chat (`None`; see `engine/webview.rs`).
    #[allow(clippy::too_many_arguments)]
    fn put_board(
        &self,
        session_id: &str,
        id: Option<&str>,
        title: Option<&str>,
        content: BoardContent,
        by_user: bool,
        place: bool,
        host: Option<Option<String>>,
    ) -> Result<Whiteboard, String> {
        if self.store.is_private(session_id) {
            return Err("whiteboards aren't available in private chats".into());
        }
        let content = self.check_content(content)?;
        let now = now_ms();
        let board = match id {
            Some(id) => {
                let mut b = self.own_board(session_id, id)?;
                // An agent sending a web view that runs on a machine somewhere
                // new acts through that machine's browser: only with access to it.
                if !by_user
                    && matches!(content, BoardContent::Url { .. })
                    && let Some(machine) = b.host.as_deref()
                {
                    self.machine_web_ok(Some(session_id), machine)?;
                }
                b.content = content;
                b.by_user &= by_user;
                if let Some(t) = title.map(|t| one_line(t, 80)).filter(|t| !t.is_empty()) {
                    b.title = t;
                }
                b.version += 1;
                b.updated_at = now;
                b
            }
            None => {
                if self
                    .store
                    .boards(Some(session_id))
                    .map(|l| l.len())
                    .unwrap_or(0)
                    >= MAX_BOARDS
                {
                    return Err("this chat has too many whiteboards; update or delete one".into());
                }
                Whiteboard {
                    id: format!("wb_{}", &new_id()[10..]),
                    session_id: session_id.to_string(),
                    title: title
                        .map(|t| one_line(t, 80))
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| default_title(&content).into()),
                    host: host.unwrap_or_else(|| self.web_host(session_id, &content, by_user)),
                    content,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                    ask: None,
                    by_user,
                }
            }
        };
        self.store.put_board(&board).map_err(|e| e.to_string())?;
        let _ = self.events.send(Event::BoardUpdated {
            board: board.clone(),
        });
        if id.is_some() {
            // A web view's live tab follows its new address (an agent's
            // change as its turn's, checked on the machine first).
            let agent = (!by_user).then(|| {
                let stops = board
                    .host
                    .as_deref()
                    .and_then(|m| self.turn_stops_on(session_id, m));
                (session_id.to_string(), stops)
            });
            self.browser.board_changed(&board.id, agent);
        }
        if place {
            self.ensure_board_tile(&self.root_session(session_id), &board.id);
        }
        Ok(board)
    }

    /// Put a board of this conversation (or one the user put beside it)
    /// back on screen beside its chat, when it isn't (the agent's explicit
    /// `show`).
    pub(super) fn board_tile(&self, session_id: &str, id: &str) -> Result<(), String> {
        if !self
            .board_views(session_id)
            .iter()
            .any(|v| v.board.id == id)
        {
            return Err(format!("no whiteboard `{id}` here"));
        }
        self.ensure_board_tile(&self.root_session(session_id), id);
        Ok(())
    }

    /// Put the board in the shared layout next to its chat (once).
    fn ensure_board_tile(&self, root: &str, board_id: &str) {
        self.ensure_tile(
            root,
            MosaicTile {
                id: new_id(),
                session_id: None,
                board_id: Some(board_id.to_string()),
                note_id: None,
            },
        );
    }

    /// Put a board or note tile in the shared layout right after `root`'s
    /// chat tile (or at the end of the first mosaic), unless it has one.
    pub(super) fn ensure_tile(&self, root: &str, tile: MosaicTile) {
        let _g = self.mosaics_lock.lock();
        let mut layout = self.mosaics();
        let same = |t: &MosaicTile| {
            (tile.board_id.is_some() && t.board_id == tile.board_id)
                || (tile.note_id.is_some() && t.note_id == tile.note_id)
        };
        if layout.mosaics.iter().any(|m| m.tiles.iter().any(same)) {
            return;
        }
        let beside = layout.mosaics.iter().enumerate().find_map(|(mi, m)| {
            m.tiles
                .iter()
                .position(|t| t.session_id.as_deref() == Some(root))
                .map(|ti| (mi, ti))
        });
        match beside {
            Some((mi, ti)) if layout.mosaics[mi].tiles.len() < 48 => {
                layout.mosaics[mi].tiles.insert(ti + 1, tile);
            }
            Some(_) => return,
            None => match layout.mosaics.first_mut() {
                Some(m) if m.tiles.len() < 48 => m.tiles.push(tile),
                Some(_) => return,
                None => layout.mosaics.push(Mosaic {
                    id: new_id(),
                    name: "Main".into(),
                    layout: "dwindle".into(),
                    tiles: vec![
                        MosaicTile {
                            id: new_id(),
                            session_id: Some(root.to_string()),
                            board_id: None,
                            note_id: None,
                        },
                        tile,
                    ],
                    geometry: None,
                }),
            },
        }
        self.store_layout(layout);
    }

    fn store_layout(&self, mut layout: MosaicLayout) {
        layout.version += 1;
        if let Ok(s) = serde_json::to_string(&layout)
            && self.store.kv_set("mosaics", &s).is_ok()
        {
            let _ = self.events.send(Event::MosaicsChanged { layout });
        }
    }

    /// Remove the tiles `gone` matches from every mosaic.
    pub(super) fn remove_tiles(&self, gone: impl Fn(&MosaicTile) -> bool) {
        let _g = self.mosaics_lock.lock();
        let mut layout = self.mosaics();
        let before: usize = layout.mosaics.iter().map(|m| m.tiles.len()).sum();
        for m in &mut layout.mosaics {
            m.tiles.retain(|t| !gone(t));
        }
        if layout.mosaics.iter().map(|m| m.tiles.len()).sum::<usize>() != before {
            self.store_layout(layout);
        }
    }

    /// Remove a board: its tiles go, and a question on it is cancelled.
    pub(super) fn delete_board(&self, id: &str) {
        let Ok(Some(b)) = self.store.board(id) else {
            return;
        };
        if let Some(ask) = &b.ask
            && let Some(p) = self.board_asks.lock().remove(&ask.id)
        {
            let _ = p.tx.send(AskAnswer::Cancel);
        }
        let _ = self.store.delete_board(id);
        self.browser.board_removed(id);
        self.remove_tiles(|t| t.board_id.as_deref() == Some(id));
        let _ = self.events.send(Event::BoardRemoved {
            id: id.to_string(),
            session_id: b.session_id,
        });
    }

    /// A chat is going away: so are its boards.
    pub(super) fn delete_session_boards(&self, session_id: &str) {
        for b in self.store.boards(Some(session_id)).unwrap_or_default() {
            self.delete_board(&b.id);
        }
    }

    /// Ask the user through a board and wait for the answer. The question
    /// shows in the chat; `tile` also puts the board on screen (an ask on a
    /// board the user must look at, or a web view handed to them).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn ask_board(
        self: Arc<Self>,
        session_id: String,
        id: Option<String>,
        title: Option<String>,
        prompt: String,
        input: AskInput,
        tile: bool,
        cancel: CancellationToken,
    ) -> Result<AskAnswer, String> {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() || prompt.len() > 2000 {
            return Err("the question must be 1–2000 characters".into());
        }
        if let AskInput::Choice { options, .. } = &input
            && (options.is_empty()
                || options.len() > 12
                || options.iter().any(|o| o.trim().is_empty() || o.len() > 200))
        {
            return Err("give 1–12 short options".into());
        }
        let mut board = match &id {
            Some(id) => self.own_board(&session_id, id)?,
            None => self.show_board(
                &session_id,
                None,
                Some(title.as_deref().unwrap_or("Your input")),
                BoardContent::Markdown {
                    markdown: prompt.clone(),
                },
                false,
            )?,
        };
        if board.ask.is_some() {
            return Err("that whiteboard is already waiting for an answer".into());
        }
        let ask_id = format!("ask_{}", &new_id()[10..]);
        let (tx, rx) = oneshot::channel();
        let identity = Arc::new(());
        self.board_asks.lock().insert(
            ask_id.clone(),
            PendingAsk {
                board_id: board.id.clone(),
                tx,
                identity: identity.clone(),
            },
        );
        let _waiting = AskWait {
            inner: &self,
            board_id: board.id.clone(),
            ask_id: ask_id.clone(),
            identity,
        };
        board.ask = Some(BoardAsk {
            id: ask_id.clone(),
            prompt,
            input,
        });
        board.version += 1;
        board.updated_at = now_ms();
        self.store.put_board(&board).map_err(|e| e.to_string())?;
        let _ = self.events.send(Event::BoardUpdated {
            board: board.clone(),
        });
        if tile {
            self.ensure_board_tile(&self.root_session(&session_id), &board.id);
        }
        let rt = self.rt(&session_id).ok();
        if let Some(rt) = &rt {
            self.set_status(rt, SessionStatus::AwaitingApproval);
        }
        let wait = async {
            tokio::select! {
                a = rx => a.map_err(|_| "the whiteboard was deleted".to_string()),
                _ = cancel.cancelled() => Err("stopped".to_string()),
                _ = tokio::time::sleep(ASK_TIMEOUT) => Err("no answer after 30 minutes".to_string()),
            }
        };
        let result = self
            .wait_for_user(&session_id, "Needs your input", &board.title, wait)
            .await;
        if let Some(rt) = &rt {
            self.set_status(rt, SessionStatus::Running);
        }
        result
    }

    fn answer_board(&self, id: &str, ask_id: &str, answer: AskAnswer) -> Result<(), ApiError> {
        let board = self.board(id)?;
        let gone = || ApiError::new(ErrorCode::NotFound, "that question was already answered");
        let ask = board.ask.filter(|a| a.id == ask_id).ok_or_else(gone)?;
        let fits = match (&ask.input, &answer) {
            (_, AskAnswer::Cancel) => true,
            (AskInput::Signature, AskAnswer::Signature { png }) => {
                base64::engine::general_purpose::STANDARD
                    .decode(png.trim())
                    .is_ok_and(|b| b.len() <= MAX_SIGNATURE && b.starts_with(b"\x89PNG\r\n\x1a\n"))
            }
            (AskInput::Text { .. }, AskAnswer::Text { text }) => text.len() <= 20_000,
            (AskInput::Choice { options, .. }, AskAnswer::Choice { index }) => {
                (*index as usize) < options.len()
            }
            (
                AskInput::Choice {
                    options,
                    multi: true,
                },
                AskAnswer::Choices { indexes },
            ) => {
                !indexes.is_empty()
                    && indexes.len() <= options.len()
                    && indexes.iter().all(|i| (*i as usize) < options.len())
                    && indexes
                        .iter()
                        .enumerate()
                        .all(|(k, i)| !indexes[..k].contains(i))
            }
            (AskInput::Confirm, AskAnswer::Confirm { .. }) => true,
            _ => false,
        };
        if !fits {
            return Err(bad("that answer doesn't fit the question"));
        }
        let pending = self.board_asks.lock().remove(ask_id).ok_or_else(gone)?;
        if pending.board_id != id {
            return Err(gone());
        }
        let _ = pending.tx.send(answer);
        Ok(())
    }

    /// Save a signature the user drew, for the agent to use.
    pub(super) fn save_signature(&self, session_id: &str, png: &str) -> Result<PathBuf, String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(png.trim())
            .map_err(|e| e.to_string())?;
        let dir = self.paths.uploads_dir().join(session_id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = dir.join(format!("signature-{}.png", now_ms()));
        crate::config::write_atomic(&path, &bytes, true).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// One request to a `Local` board's web app, on this machine only.
    async fn board_fetch(
        &self,
        id: &str,
        method: &str,
        path: &str,
        headers: Vec<(String, String)>,
        body: Option<String>,
    ) -> Result<BoardResponse, ApiError> {
        let body = body
            .map(|b| {
                base64::engine::general_purpose::STANDARD
                    .decode(b.trim())
                    .map_err(|_| bad("the body isn't valid base64"))
            })
            .transpose()?;
        let (status, headers, bytes) = self.local_fetch(id, method, path, headers, body).await?;
        Ok(BoardResponse {
            status,
            headers,
            body: base64::engine::general_purpose::STANDARD.encode(&bytes),
        })
    }

    /// [`board_fetch`](Self::board_fetch) with plain bodies.
    async fn local_fetch(
        &self,
        id: &str,
        method: &str,
        path: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Vec<(String, String)>, Vec<u8>), ApiError> {
        let BoardContent::Local { port, .. } = self.board(id)?.content else {
            return Err(bad("that whiteboard doesn't show a local web app"));
        };
        if Some(port) == self.own_port() || !path.starts_with('/') || path.contains("://") {
            return Err(bad("not allowed"));
        }
        let method = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .ok()
            .filter(|m| {
                matches!(
                    m.as_str(),
                    "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
                )
            })
            .ok_or_else(|| bad("unsupported method"))?;
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client")
        });
        let mut rb = client.request(method, format!("http://127.0.0.1:{port}{path}"));
        for (k, v) in headers {
            let lk = k.to_ascii_lowercase();
            // Hop-by-hop headers, and anything that would carry the
            // device-side proxy's token or pretend to be another site.
            if !matches!(
                lk.as_str(),
                "host"
                    | "connection"
                    | "keep-alive"
                    | "proxy-authorization"
                    | "proxy-connection"
                    | "te"
                    | "trailer"
                    | "transfer-encoding"
                    | "upgrade"
                    | "referer"
                    | "origin"
                    | "content-length"
            ) {
                rb = rb.header(k, v);
            }
        }
        if let Some(bytes) = body {
            if bytes.len() > MAX_PROXIED {
                return Err(bad("request too large"));
            }
            rb = rb.body(bytes);
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok((
                    502,
                    vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                    format!("Nothing answered on port {port} of the Tessera server: {e}")
                        .into_bytes(),
                ));
            }
        };
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "connection"
                        | "keep-alive"
                        | "transfer-encoding"
                        | "content-length"
                        | "content-encoding"
                        | "upgrade"
                        | "set-cookie"
                )
            })
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let bytes = resp.bytes().await.map_err(|e| bad(e.to_string()))?;
        if bytes.len() > MAX_PROXIED {
            return Err(bad("response too large"));
        }
        Ok((status, headers, bytes.to_vec()))
    }

    /// The boards an agent in `session_id` can see: its conversation's (and
    /// subagents'), and any the user put in a mosaic next to this chat.
    pub(super) fn board_views(&self, session_id: &str) -> Vec<crate::tools::board::BoardView> {
        let root = self.root_session(session_id);
        let layout = self.mosaics();
        let mut titles: HashMap<String, String> = HashMap::new();
        let mut title = |id: &str| {
            titles
                .entry(id.to_string())
                .or_insert_with(|| match self.store.get_session(id) {
                    Ok(Some((info, _))) => one_line(&info.title, 60),
                    _ => "a closed chat".into(),
                })
                .clone()
        };
        let beside_me: Vec<&str> = layout
            .mosaics
            .iter()
            .filter(|m| {
                m.tiles
                    .iter()
                    .any(|t| t.session_id.as_deref() == Some(&root))
            })
            .flat_map(|m| m.tiles.iter().filter_map(|t| t.board_id.as_deref()))
            .collect();
        let mut out = Vec::new();
        for board in self.store.boards(None).unwrap_or_default() {
            let owner = self.root_session(&board.session_id);
            if owner != root && !beside_me.contains(&board.id.as_str()) {
                continue;
            }
            let mut on_screen = Vec::new();
            for m in &layout.mosaics {
                for (i, t) in m.tiles.iter().enumerate() {
                    if t.board_id.as_deref() != Some(&board.id) {
                        continue;
                    }
                    // The chat tile it sits after (boards open right after their chat).
                    let chat = m.tiles[..i]
                        .iter()
                        .rev()
                        .chain(&m.tiles[i + 1..])
                        .find_map(|t| t.session_id.as_deref());
                    on_screen.push(match chat {
                        Some(c) if c == root => format!("in \"{}\" beside this chat", m.name),
                        Some(c) => format!("in \"{}\" beside \"{}\"", m.name, title(c)),
                        None => format!("in \"{}\"", m.name),
                    });
                }
            }
            out.push(crate::tools::board::BoardView {
                other_chat: (owner != root).then(|| title(&owner)),
                on_screen,
                board,
            });
        }
        out
    }

    /// What a board's tile shows on a device right now: asks every client;
    /// the first image within [`LOOK_TIMEOUT`] wins, and the wait ends early
    /// once every connected client has said it can't. Only this call gets it.
    pub(super) async fn look_board(
        &self,
        session_id: &str,
        id: &str,
        cancel: CancellationToken,
    ) -> Result<SnapshotImage, String> {
        let Some(view) = self
            .board_views(session_id)
            .into_iter()
            .find(|v| v.board.id == id)
        else {
            return Err(format!("no whiteboard `{id}` here"));
        };
        let web = matches!(
            view.board.content,
            BoardContent::Url { .. } | BoardContent::Local { .. }
        );
        // A web view on a machine: the agent looks only as that machine
        // allows its turn (as for anything else there), never at a device's
        // picture of it instead.
        if web && let Some(machine) = view.board.host.as_deref() {
            let mark = self.turn_stops_on(session_id, machine);
            return match self
                .browser
                .board_screenshot_for(id, session_id, mark)
                .await
            {
                Ok(Some(img)) => Ok(img),
                Ok(None) => Err("its page on that machine isn't open right now".into()),
                Err(e) => Err(e),
            };
        }
        // A web view's tab lives in the server's browser: no device needed.
        if web && let Some(img) = self.browser.board_screenshot(id).await {
            return Ok(img);
        }
        if view.on_screen.is_empty() {
            return Err("it isn't on screen".into());
        }
        let clients = self.clients.load(std::sync::atomic::Ordering::Relaxed);
        if clients == 0 {
            return Err("no device is connected".into());
        }
        let request_id = format!("snap_{}", &new_id()[10..]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.snapshots.lock().insert(request_id.clone(), tx.clone());
        let _waiting = SnapshotWait {
            inner: self,
            request_id: request_id.clone(),
            tx,
        };
        let _ = self.events.send(Event::BoardSnapshotRequest {
            request_id: request_id.clone(),
            board_id: id.to_string(),
        });
        let mut why = Vec::new();
        let deadline = tokio::time::sleep(LOOK_TIMEOUT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                a = rx.recv() => match a {
                    Some(Ok(img)) => break Ok(img),
                    Some(Err(e)) => {
                        why.push(e);
                        if why.len() >= clients {
                            break Err(why.join("; "));
                        }
                    }
                    None => break Err("no device answered".to_string()),
                },
                _ = &mut deadline => break Err(if why.is_empty() {
                    "no device showing it answered in time".to_string()
                } else {
                    why.join("; ")
                }),
                _ = cancel.cancelled() => break Err("stopped".to_string()),
            }
        }
    }

    /// A `Local` board's page, fetched the way its tile is, for an agent
    /// that can see the board.
    pub(super) async fn board_local_get(
        &self,
        session_id: &str,
        id: &str,
    ) -> Result<crate::tools::board::LocalPage, String> {
        let Some(view) = self
            .board_views(session_id)
            .into_iter()
            .find(|v| v.board.id == id)
        else {
            return Err(format!("no whiteboard `{id}` here"));
        };
        let BoardContent::Local { path, .. } = &view.board.content else {
            return Err("that whiteboard doesn't show a local web app".into());
        };
        let (status, headers, body) = self
            .local_fetch(id, "GET", path, Vec::new(), None)
            .await
            .map_err(|e| e.message)?;
        let ctype = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.to_ascii_lowercase())
            .unwrap_or_default();
        Ok((status, ctype, body))
    }

    fn board_key(&self) -> anyhow::Result<Vec<u8>> {
        let k = self
            .store
            .kv_get_or_insert("board_link_key", &crate::util::random_token(32))?;
        if k.len() < 32 {
            return Err(anyhow!("the stored whiteboard link key is invalid"));
        }
        Ok(k.into_bytes())
    }

    fn board_token(&self, id: &str) -> anyhow::Result<String> {
        let board = self
            .store
            .board(id)?
            .ok_or_else(|| anyhow!("no such whiteboard"))?;
        self.board_token_until(id, board.version, now_ms() + 86_400_000)
    }

    fn board_token_until(&self, id: &str, version: u64, expires: u64) -> anyhow::Result<String> {
        use hmac::{Hmac, Mac};
        let grant = format!("v1.{version}.{expires}");
        let mut mac = <Hmac<sha2::Sha256>>::new_from_slice(&self.board_key()?).expect("hmac key");
        mac.update(b"tessera-board-link\0");
        mac.update(id.as_bytes());
        mac.update(b"\0");
        mac.update(grant.as_bytes());
        let signature: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(format!("{grant}.{signature}"))
    }

    /// Page links last a day and cannot reveal a later revision of a board.
    /// The path remains opaque to clients, which request it anew when viewing.
    pub(super) fn board_page(&self, id: &str, token: &str) -> Option<String> {
        self.board_page_at(id, token, now_ms())
    }

    fn board_page_at(&self, id: &str, token: &str, now: u64) -> Option<String> {
        let parts: Vec<_> = token.split('.').collect();
        if parts.len() != 4 || parts[0] != "v1" || token.len() > 96 {
            return None;
        }
        let version: u64 = parts[1].parse().ok()?;
        let expires: u64 = parts[2].parse().ok()?;
        if expires <= now {
            return None;
        }
        let want = self.board_token_until(id, version, expires).ok()?;
        let ok = want.len() == token.len()
            && want
                .bytes()
                .zip(token.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        if !ok {
            return None;
        }
        let board = self.store.board(id).ok()??;
        if board.version != version {
            return None;
        }
        match board.content {
            BoardContent::Html { html } => Some(html),
            _ => None,
        }
    }
}

impl Inner {
    pub(super) fn own_board_pub(&self, session_id: &str, id: &str) -> Result<Whiteboard, String> {
        self.own_board(session_id, id)
    }
}

/// A device's snapshot, if it is a reasonable image of a tile.
fn check_snapshot(img: SnapshotImage) -> Result<SnapshotImage, String> {
    let mime = img.mime.trim().to_ascii_lowercase();
    if !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
        return Err("snapshots must be PNG, JPEG or WebP".into());
    }
    let side = img.width.max(img.height);
    if img.width == 0 || img.height == 0 || side > MAX_SNAPSHOT_SIDE {
        return Err(format!(
            "snapshots are at most {MAX_SNAPSHOT_SIDE} pixels on the long side"
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(img.data.trim())
        .map_err(|_| "the snapshot isn't valid base64".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_SNAPSHOT {
        return Err(format!("snapshots are at most {} MB", MAX_SNAPSHOT >> 20));
    }
    Ok(SnapshotImage {
        mime,
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        ..img
    })
}

fn default_title(c: &BoardContent) -> &'static str {
    match c {
        BoardContent::Html { .. } => "Whiteboard",
        BoardContent::Markdown { .. } => "Notes",
        BoardContent::Image { .. } => "Image",
        BoardContent::Url { .. } => "Web page",
        BoardContent::Local { .. } => "Web app",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn page_grants_expire_and_do_not_reveal_new_revisions() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let board = html_board(&engine, &rt, false);
        let token = engine
            .0
            .board_token_until(&board.id, board.version, 1000)
            .unwrap();
        assert!(engine.0.board_page_at(&board.id, &token, 999).is_some());
        assert!(engine.0.board_page_at(&board.id, &token, 1000).is_none());
        assert!(engine.0.board_page_at(&board.id, &token, 1001).is_none());
        assert!(engine.0.board_page_at("wb_other", &token, 999).is_none());
        assert!(
            engine
                .0
                .board_page_at(&board.id, &token.replace(".1000.", ".2000."), 999)
                .is_none()
        );
        assert!(
            engine
                .0
                .board_page_at(&board.id, &token.replace("v1.", "v2."), 999)
                .is_none()
        );
        for invalid in [
            "",
            "0123456789abcdef0123456789abcdef",
            "v1.1.1000.bad",
            "v1.1.no.date",
        ] {
            assert!(engine.0.board_page_at(&board.id, invalid, 999).is_none());
        }
        let sid = rt.st.lock().info.id.clone();
        let updated = engine
            .0
            .show_board(
                &sid,
                Some(&board.id),
                None,
                BoardContent::Html {
                    html: "<p>New revision</p>".into(),
                },
                false,
            )
            .unwrap();
        assert!(updated.version > board.version);
        assert!(engine.0.board_page_at(&board.id, &token, 999).is_none());
        let fresh = engine.0.board_token(&board.id).unwrap();
        assert_eq!(
            engine.board_page(&board.id, &fresh).as_deref(),
            Some("<p>New revision</p>")
        );
        engine
            .store()
            .kv_set("board_link_key", &crate::util::random_token(32))
            .unwrap();
        assert!(engine.board_page(&board.id, &fresh).is_none());
    }

    fn html_board(engine: &Engine, rt: &Rt, tile: bool) -> Whiteboard {
        let sid = rt.st.lock().info.id.clone();
        engine
            .0
            .show_board(
                &sid,
                None,
                Some("Fixture"),
                BoardContent::Html {
                    html: "<p>Fixture</p>".into(),
                },
                tile,
            )
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_links_use_the_persisted_key_and_survive_reopen() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let b = html_board(&engine, &rt, false);
        assert!(engine.store().kv_get("board_link_key").unwrap().is_none());
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let (inner, id, barrier) = (engine.0.clone(), b.id.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                inner
                    .board_request(Request::BoardLink { id }, None)
                    .await
                    .unwrap()["path"]
                    .as_str()
                    .unwrap()
                    .to_string()
            }));
        }
        let mut paths = Vec::new();
        for task in tasks {
            paths.push(task.await.unwrap());
        }
        assert!(paths.iter().all(|p| {
            let token = p.trim_end_matches('/').rsplit('/').next().unwrap();
            engine.board_page(&b.id, token).is_some()
        }));
        let token = paths[0].trim_end_matches('/').rsplit('/').next().unwrap();
        assert_eq!(
            engine.board_page(&b.id, token).as_deref(),
            Some("<p>Fixture</p>")
        );
        let reopened = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        assert_eq!(
            reopened.board_page(&b.id, token).as_deref(),
            Some("<p>Fixture</p>")
        );
    }

    #[tokio::test]
    async fn a_failed_first_key_write_returns_no_link_or_unpersisted_token() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let b = html_board(&engine, &rt, false);
        let conn = rusqlite::Connection::open(dir.path().join("tessera.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_board_key BEFORE INSERT ON meta WHEN new.key = 'board_link_key' BEGIN SELECT RAISE(ABORT, 'fixture key write failed'); END;").unwrap();
        assert!(
            engine
                .0
                .clone()
                .board_request(Request::BoardLink { id: b.id.clone() }, None)
                .await
                .is_err()
        );
        assert!(engine.store().kv_get("board_link_key").unwrap().is_none());
        assert!(engine.board_page(&b.id, "not-a-token").is_none());
        assert!(engine.store().kv_get("board_link_key").unwrap().is_none());
        conn.execute_batch("DROP TRIGGER refuse_board_key;")
            .unwrap();
        let token = engine.0.board_token(&b.id).unwrap();
        assert!(engine.store().kv_get("board_link_key").unwrap().is_some());
        assert!(engine.board_page(&b.id, &token).is_some());
    }

    #[tokio::test]
    async fn dropping_a_snapshot_wait_removes_only_its_sender() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let b = html_board(&engine, &rt, true);
        let _client = engine.client_connected();
        let sid = rt.st.lock().info.id.clone();
        let mut wait = Box::pin(engine.0.look_board(&sid, &b.id, CancellationToken::new()));
        assert!(futures::poll!(wait.as_mut()).is_pending());
        let id = engine.0.snapshots.lock().keys().next().unwrap().clone();
        drop(wait);
        assert!(engine.0.snapshots.lock().is_empty());
        assert!(
            engine
                .0
                .clone()
                .board_request(
                    Request::BoardSnapshot {
                        request_id: id.clone(),
                        image: None,
                        error: Some("late".into())
                    },
                    None
                )
                .await
                .is_err()
        );
        let (old, _rx) = mpsc::unbounded_channel();
        let guard = SnapshotWait {
            inner: &engine.0,
            request_id: id.clone(),
            tx: old.clone(),
        };
        engine.0.snapshots.lock().insert(id.clone(), old);
        let (new, _new_rx) = mpsc::unbounded_channel();
        engine.0.snapshots.lock().insert(id.clone(), new.clone());
        drop(guard);
        assert!(
            engine
                .0
                .snapshots
                .lock()
                .get(&id)
                .unwrap()
                .same_channel(&new)
        );
    }

    #[tokio::test]
    async fn dropping_an_ask_clears_its_question_without_losing_a_newer_board_edit() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let b = html_board(&engine, &rt, false);
        let sid = rt.st.lock().info.id.clone();
        let mut wait = Box::pin(engine.0.clone().ask_board(
            sid.clone(),
            Some(b.id.clone()),
            None,
            "Continue?".into(),
            AskInput::Confirm,
            false,
            CancellationToken::new(),
        ));
        assert!(futures::poll!(wait.as_mut()).is_pending());
        assert_eq!(engine.0.board_asks.lock().len(), 1);
        let edited = engine
            .0
            .show_board(
                &sid,
                Some(&b.id),
                None,
                BoardContent::Html {
                    html: "<p>Newer edit</p>".into(),
                },
                false,
            )
            .unwrap();
        drop(wait);
        assert!(engine.0.board_asks.lock().is_empty());
        let saved = engine.store().board(&b.id).unwrap().unwrap();
        assert!(saved.ask.is_none());
        assert_eq!(saved.version, edited.version + 1);
        assert!(
            matches!(saved.content, BoardContent::Html { html } if html == "<p>Newer edit</p>")
        );
    }

    #[tokio::test]
    async fn a_failed_ask_save_leaves_no_sender_or_question() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let b = html_board(&engine, &rt, false);
        let conn = rusqlite::Connection::open(dir.path().join("tessera.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_board_ask BEFORE INSERT ON boards BEGIN SELECT RAISE(ABORT, 'fixture ask write failed'); END;").unwrap();
        let sid = rt.st.lock().info.id.clone();
        let result = engine
            .0
            .clone()
            .ask_board(
                sid,
                Some(b.id.clone()),
                None,
                "Continue?".into(),
                AskInput::Confirm,
                false,
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(engine.0.board_asks.lock().is_empty());
        assert!(engine.store().board(&b.id).unwrap().unwrap().ask.is_none());
    }

    #[tokio::test]
    async fn old_ask_cleanup_does_not_clear_a_new_question_or_replaced_sender() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let b = html_board(&engine, &rt, false);
        let sid = rt.st.lock().info.id.clone();
        let mut wait = Box::pin(engine.0.clone().ask_board(
            sid,
            Some(b.id.clone()),
            None,
            "Old?".into(),
            AskInput::Confirm,
            false,
            CancellationToken::new(),
        ));
        assert!(futures::poll!(wait.as_mut()).is_pending());
        let mut newer = engine.store().board(&b.id).unwrap().unwrap();
        let old_id = newer.ask.as_ref().unwrap().id.clone();
        newer.ask.as_mut().unwrap().id = "ask_newer".into();
        newer.version += 1;
        engine.store().put_board(&newer).unwrap();
        drop(wait);
        assert_eq!(
            engine.store().board(&b.id).unwrap().unwrap().version,
            newer.version
        );
        assert_eq!(
            engine
                .store()
                .board(&b.id)
                .unwrap()
                .unwrap()
                .ask
                .unwrap()
                .id,
            "ask_newer"
        );
        let old_identity = Arc::new(());
        let guard = AskWait {
            inner: &engine.0,
            board_id: b.id.clone(),
            ask_id: old_id.clone(),
            identity: old_identity,
        };
        let (tx, _rx) = oneshot::channel();
        let new_identity = Arc::new(());
        engine.0.board_asks.lock().insert(
            old_id.clone(),
            PendingAsk {
                board_id: b.id,
                tx,
                identity: new_identity.clone(),
            },
        );
        drop(guard);
        assert!(Arc::ptr_eq(
            &engine.0.board_asks.lock().get(&old_id).unwrap().identity,
            &new_identity
        ));
    }
}
