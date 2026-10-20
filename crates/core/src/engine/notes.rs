//! The notes library: markdown documents the user keeps (like artifacts),
//! written by them or by an agent. Clients render them sanitised, never as
//! raw HTML; agents read them as data, scanned like other stored text.
//!
//! Pictures in notes are kept apart, by their content
//! (`<data dir>/note-assets/<16 hex>.<ext>`), and notes refer to them as
//! `![alt](assets/<name>)`. They reach the apps only through requests on the
//! authenticated connection (`read_note_asset`). One that no note and no kept
//! version uses any more goes after a day.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::*;

/// Largest note, in bytes.
pub(crate) const MAX_NOTE: usize = 200 << 10;
const MAX_NOTES: usize = 5_000;
/// A version is kept at least this often while someone keeps editing.
const VERSION_EVERY_MS: u64 = 10 * 60_000;
/// Versions kept per note, and their content at most.
const MAX_VERSIONS: usize = 50;
const MAX_VERSION_BYTES: usize = 2 << 20;
/// Preview pages (`PreviewPage`): their size, how many, for how long.
const MAX_PREVIEW: usize = 512 << 10;
const MAX_PREVIEWS: usize = 16;
const PREVIEW_TTL: Duration = Duration::from_secs(3600);
/// Pictures in notes: each at most this big, all of them at most this much.
pub(crate) const MAX_ASSET: usize = 8 << 20;
const ASSETS_QUOTA: u64 = 2 << 30;
/// Largest piece of a picture per `read_note_asset`.
const ASSET_CHUNK: u32 = 1 << 20;
/// A picture no note uses is kept this long (a note may be about to).
pub(crate) const ASSET_GRACE: Duration = Duration::from_secs(86_400);
/// Unused pictures are looked for at most this often.
const SWEEP_EVERY: Duration = Duration::from_secs(600);

/// The notes library's state in the engine.
#[derive(Default)]
pub(crate) struct NotesState {
    /// Saves one at a time, so a version check and its write are one step.
    lock: Mutex<()>,
    /// Preview pages: token, made at, HTML.
    previews: Mutex<VecDeque<(String, Instant, String)>>,
    /// When unused pictures were last looked for.
    swept: Mutex<Option<Instant>>,
}

impl Inner {
    pub(super) fn note_request(&self, req: Request) -> ApiResult {
        match req {
            Request::ListNotes => {
                // Each device asks when it connects: a good moment to tidy up.
                self.sweep_assets_soon(false);
                to_value(
                    self.store
                        .notes()
                        .map_err(internal)?
                        .iter()
                        .map(note_info)
                        .collect::<Vec<_>>(),
                )
            }
            Request::GetNote { id } => to_value(self.note(&id)?),
            Request::SaveNote {
                id,
                title,
                content,
                base_version,
                session_id,
            } => {
                if let Some(s) = &session_id {
                    self.rt(s)?;
                }
                to_value(self.save_note(
                    id.as_deref(),
                    &title,
                    content,
                    base_version,
                    session_id.as_deref(),
                    true,
                )?)
            }
            Request::DeleteNote { id } => {
                self.note(&id)?;
                self.delete_note(&id);
                Ok(Value::Null)
            }
            // The user's choice (a request from a device; agents have no way to it).
            Request::SetNoteAgents {
                id,
                keep_from_agents,
            } => to_value(self.keep_note_from_agents(&id, keep_from_agents)?),
            // Older apps: "Only bots I add" keeps it from agents (the stricter
            // reading of it), "All my bots" lets every agent read it.
            Request::SetNoteBots { id, all_bots } => {
                to_value(self.keep_note_from_agents(&id, !all_bots)?)
            }
            Request::NoteHistory { id } => {
                self.note(&id)?;
                let list: Vec<NoteVersion> = self
                    .store
                    .note_versions(&id)
                    .map_err(internal)?
                    .into_iter()
                    .map(|v| NoteVersion {
                        content: String::new(),
                        ..v
                    })
                    .collect();
                to_value(list)
            }
            Request::GetNoteVersion { id, version } => {
                let n = self.note(&id)?;
                if n.version == version {
                    return to_value(NoteVersion {
                        version,
                        title: n.title,
                        content: n.content,
                        updated_at: n.updated_at,
                        agent_edit: n.agent_edit,
                        edit_session: n.edit_session,
                    });
                }
                match self
                    .store
                    .note_version(&id, Some(version))
                    .map_err(internal)?
                {
                    Some(v) => to_value(v),
                    None => Err(ApiError::new(
                        ErrorCode::NotFound,
                        format!("version {version} of this note isn't kept"),
                    )),
                }
            }
            Request::PreviewPage { html } => self.preview_link(html),
            Request::PutNoteAsset { mime: _, data } => {
                use base64::Engine as _;
                // The size is checked again on the bytes; this keeps a huge one from being decoded.
                if data.len() > MAX_ASSET / 3 * 4 + 4 {
                    return Err(too_big());
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.trim())
                    .map_err(|_| bad("`data` isn't base64"))?;
                to_value(self.put_note_asset(&bytes)?)
            }
            Request::ReadNoteAsset { name, offset, len } => {
                to_value(self.read_note_asset(&name, offset, len)?)
            }
            Request::NoteBacklinks { id } => {
                let n = self.note(&id)?;
                to_value(
                    self.store
                        .notes()
                        .map_err(internal)?
                        .iter()
                        .filter(|o| o.id != n.id && links_to(&o.content, &n))
                        .map(note_info)
                        .collect::<Vec<_>>(),
                )
            }
            _ => Err(bad("not a notes request")),
        }
    }

    /// Keep a note from agents and bots, or not: no new version of its
    /// text, every device hears of it.
    fn keep_note_from_agents(&self, id: &str, keep: bool) -> Result<Note, ApiError> {
        let _one_at_a_time = self.notes.lock.lock();
        let mut n = self.note(id)?;
        if n.keep_from_agents != keep {
            n.keep_from_agents = keep;
            n.all_bots = !keep;
            self.store.put_note(&n).map_err(internal)?;
            let _ = self.events.send(Event::NoteUpdated {
                note: note_info(&n),
            });
        }
        Ok(n)
    }

    pub(super) fn note(&self, id: &str) -> Result<Note, ApiError> {
        self.store
            .note(id)
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no note `{id}`")))
    }

    /// Create (`id` None) or replace a note. A `base_version` that is no
    /// longer current is a conflict, unless the note already says exactly
    /// this (a save repeated after its answer was lost): nothing changes then,
    /// so no new version and no event. A private chat is never named.
    pub(super) fn save_note(
        &self,
        id: Option<&str>,
        title: &str,
        content: String,
        base_version: Option<u64>,
        session_id: Option<&str>,
        by_user: bool,
    ) -> Result<Note, ApiError> {
        if content.len() > MAX_NOTE {
            return Err(bad(format!("notes are at most {} KB", MAX_NOTE >> 10)));
        }
        let title = match one_line(title, 120) {
            t if t.is_empty() => default_note_title(&content),
            t => t,
        };
        let chat = session_id
            .filter(|s| !self.store.is_private(s))
            .map(|s| self.root_session(s));
        let edit_session = if by_user { None } else { chat.clone() };
        let now = now_ms();
        let _one_at_a_time = self.notes.lock.lock();
        let note = match id {
            Some(id) => {
                let mut n = self.note(id)?;
                if n.title == title && n.content == content {
                    return Ok(n);
                }
                if base_version.is_some_and(|v| v != n.version) {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "the note changed since it was opened; fetch it and merge",
                    ));
                }
                self.keep_note_version(&n, by_user);
                n.title = title;
                n.content = content;
                n.version += 1;
                n.updated_at = now;
                n.agent_edit = !by_user;
                n.edit_session = edit_session;
                n
            }
            None => {
                if self.store.note_count().map_err(internal)? >= MAX_NOTES {
                    return Err(bad("the notes library is full; delete some first"));
                }
                Note {
                    id: format!("nt_{}", &new_id()[10..]),
                    title,
                    content,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                    session_id: chat,
                    by_user,
                    agent_edit: !by_user,
                    edit_session,
                    // Every agent may read a new note, until the user keeps it from them.
                    all_bots: true,
                    keep_from_agents: false,
                }
            }
        };
        self.store.put_note(&note).map_err(internal)?;
        let _ = self.events.send(Event::NoteUpdated {
            note: note_info(&note),
        });
        self.sweep_assets_soon(false);
        Ok(note)
    }

    /// Keep `old` in the note's history before it is replaced: always when
    /// the writer changes (the user and an agent), so an agent's changes can
    /// be compared and undone, else once per stretch of editing.
    fn keep_note_version(&self, old: &Note, by_user: bool) {
        if old.content.trim().is_empty() {
            return;
        }
        let writer_changes = old.agent_edit == by_user;
        let stretch = self
            .store
            .note_version(&old.id, None)
            .ok()
            .flatten()
            .is_none_or(|last| old.updated_at.saturating_sub(last.updated_at) >= VERSION_EVERY_MS);
        if !writer_changes && !stretch {
            return;
        }
        let v = NoteVersion {
            version: old.version,
            title: old.title.clone(),
            content: old.content.clone(),
            updated_at: old.updated_at,
            agent_edit: old.agent_edit,
            edit_session: old.edit_session.clone(),
        };
        if let Err(e) = self
            .store
            .put_note_version(&old.id, &v, MAX_VERSIONS, MAX_VERSION_BYTES)
        {
            tracing::warn!("couldn't keep a version of note {}: {e:#}", old.id);
        }
    }

    /// A page for `html` (a note's HTML, previewed), like an HTML board's.
    fn preview_link(&self, html: String) -> ApiResult {
        if html.len() > MAX_PREVIEW {
            return Err(bad(format!(
                "previews are at most {} KB",
                MAX_PREVIEW >> 10
            )));
        }
        let token = crate::util::random_token(24);
        let mut pages = self.notes.previews.lock();
        pages.retain(|(_, at, _)| at.elapsed() < PREVIEW_TTL);
        while pages.len() >= MAX_PREVIEWS {
            pages.pop_front();
        }
        pages.push_back((token.clone(), Instant::now(), html));
        to_value(json!({ "path": format!("/preview/{token}/") }))
    }

    /// A preview page's HTML, while its token is good.
    pub(super) fn preview_page(&self, token: &str) -> Option<String> {
        let eq = |a: &str, b: &str| {
            a.len() == b.len()
                && a.bytes()
                    .zip(b.bytes())
                    .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                    == 0
        };
        self.notes
            .previews
            .lock()
            .iter()
            .find(|(t, at, _)| eq(t, token) && at.elapsed() < PREVIEW_TTL)
            .map(|(_, _, html)| html.clone())
    }

    /// Remove a note and its tiles.
    pub(super) fn delete_note(&self, id: &str) {
        if self.store.delete_note(id).unwrap_or(false) {
            self.remove_tiles(|t| t.note_id.as_deref() == Some(id));
            let _ = self.events.send(Event::NoteRemoved { id: id.to_string() });
            self.sweep_assets_soon(true);
        }
    }

    fn assets_dir(&self) -> PathBuf {
        self.paths.data_dir.join("note-assets")
    }

    /// Keep a picture for notes. The same picture again is the same file
    /// (made recent again, so a sweep leaves it for the note about to use it).
    pub(super) fn put_note_asset(&self, bytes: &[u8]) -> Result<NoteAsset, ApiError> {
        if bytes.len() > MAX_ASSET {
            return Err(too_big());
        }
        let (mime, ext) = picture_kind(bytes)
            .ok_or_else(|| bad("that isn't a PNG, JPEG, WebP or GIF picture"))?;
        let (width, height) = crate::tools::board::image_size(mime, bytes).unwrap_or((0, 0));
        let name = format!("{}.{ext}", &crate::util::sha256_hex(bytes)[..16]);
        let dir = self.assets_dir();
        let path = dir.join(&name);
        let asset = NoteAsset {
            name: name.clone(),
            mime: mime.into(),
            size: bytes.len() as u64,
            width,
            height,
        };
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            if let Ok(f) = std::fs::File::options().append(true).open(&path) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            return Ok(asset);
        }
        super::notify::make_private_dir(&dir).map_err(internal)?;
        if self.assets_size() + bytes.len() as u64 > ASSETS_QUOTA {
            self.sweep_note_assets(ASSET_GRACE);
            if self.assets_size() + bytes.len() as u64 > ASSETS_QUOTA {
                return Err(bad(format!(
                    "the notes' pictures take up {} already; remove some from notes first",
                    crate::util::human_bytes(ASSETS_QUOTA)
                )));
            }
        }
        // Written whole, then put in place: a reader never sees half a picture.
        let tmp = dir.join(format!(".tmp-{}", crate::util::random_token(8)));
        let written = (|| {
            use std::io::Write as _;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            let mut f = o.open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(internal(e));
        }
        Ok(asset)
    }

    /// A picture's bytes (for agents reading a note).
    pub(super) fn note_asset_bytes(&self, name: &str) -> Option<(&'static str, Vec<u8>)> {
        use std::io::Read as _;
        let path = self.asset_path(name)?;
        let opened = crate::tools::file_object::Opened::open_in(&path, &self.assets_dir()).ok()?;
        let mut bytes = Vec::new();
        (&opened.file)
            .take(MAX_ASSET as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        opened.check_path().ok()?;
        if bytes.len() > MAX_ASSET {
            return None;
        }
        Some((picture_kind(&bytes)?.0, bytes))
    }

    /// Where a picture is, if `name` is one and it is a plain file there.
    fn asset_path(&self, name: &str) -> Option<PathBuf> {
        if !asset_name_ok(name) {
            return None;
        }
        let path = self.assets_dir().join(name);
        std::fs::symlink_metadata(&path)
            .is_ok_and(|m| m.is_file())
            .then_some(path)
    }

    fn read_note_asset(&self, name: &str, offset: u64, len: u32) -> Result<FileChunk, ApiError> {
        use base64::Engine as _;
        use std::io::{Read, Seek, SeekFrom};
        let gone = || {
            ApiError::new(
                ErrorCode::NotFound,
                "this picture is no longer on the server",
            )
        };
        let path = self.asset_path(name).ok_or_else(gone)?;
        let opened = crate::tools::file_object::Opened::open_in(&path, &self.assets_dir())
            .map_err(|_| gone())?;
        let mut f = &opened.file;
        let size = f.metadata().map_err(|_| gone())?.len();
        if offset > size {
            return Err(bad("offset past the end of the picture"));
        }
        f.seek(SeekFrom::Start(offset)).map_err(internal)?;
        let want = if len == 0 {
            ASSET_CHUNK
        } else {
            len.min(ASSET_CHUNK)
        } as u64;
        let mut buf = Vec::with_capacity(want.min(size - offset) as usize);
        f.take(want).read_to_end(&mut buf).map_err(internal)?;
        opened.check_path().map_err(|_| gone())?;
        let end = offset + buf.len() as u64;
        Ok(FileChunk {
            data: base64::engine::general_purpose::STANDARD.encode(&buf),
            offset,
            size,
            eof: end >= size,
        })
    }

    fn assets_size(&self) -> u64 {
        std::fs::read_dir(self.assets_dir())
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.metadata().ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Remove the pictures that no note and no kept version uses, unless
    /// newer than `grace`. Nothing goes when the notes can't be read.
    pub(crate) fn sweep_note_assets(&self, grace: Duration) -> usize {
        let Ok(dir) = std::fs::read_dir(self.assets_dir()) else {
            return 0;
        };
        let mut used = HashSet::new();
        let read = self
            .store
            .each_note_text(|t| used.extend(asset_refs(t).map(str::to_string)));
        if read.is_err() {
            return 0;
        }
        let mut gone = 0;
        for e in dir.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            // Only our own files: pictures and what an interrupted write left.
            if used.contains(name) || !(asset_name_ok(name) || name.starts_with(".tmp-")) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let old = meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age >= grace);
            if meta.is_file() && old && std::fs::remove_file(e.path()).is_ok() {
                gone += 1;
            }
        }
        gone
    }

    /// Look for unused pictures, at most every few minutes (`now`: unless
    /// just done), off the request's way.
    fn sweep_assets_soon(&self, now: bool) {
        {
            let mut last = self.notes.swept.lock();
            let wait = if now {
                Duration::from_secs(5)
            } else {
                SWEEP_EVERY
            };
            if last.is_some_and(|t| t.elapsed() < wait) {
                return;
            }
            *last = Some(Instant::now());
        }
        let Some(me) = self.me.upgrade() else {
            return;
        };
        let run = move || {
            let n = me.sweep_note_assets(ASSET_GRACE);
            if n > 0 {
                tracing::info!("removed {n} pictures no note uses any more");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => drop(h.spawn_blocking(run)),
            Err(_) => run(),
        }
    }

    /// Open a note's tile beside the chat `session_id` belongs to.
    pub(super) fn show_note(&self, session_id: &str, id: &str) -> Result<(), String> {
        self.note(id).map_err(|e| e.message)?;
        self.ensure_tile(
            &self.root_session(session_id),
            MosaicTile {
                id: new_id(),
                session_id: None,
                board_id: None,
                note_id: Some(id.to_string()),
            },
        );
        Ok(())
    }
}

impl Inner {
    /// Whether a bot's conversation: its chats and DMs, channels it is in
    /// (and their subagents). Never a private chat or a helper session.
    pub(super) fn bot_sees_session(&self, bot: &Bot, session: &str) -> bool {
        matches!(
            self.store.get_session(&self.root_session(session)),
            Ok(Some((info, _))) if !info.private && !info.internal
                && (info.bot_id.as_deref() == Some(bot.id.as_str())
                    || (info.kind == SessionKind::Channel && info.members.contains(&bot.id)))
        )
    }

    /// Whether the agent in `session_id` may read `other`'s history: any
    /// non-private conversation, but a bot with its own memory only its own.
    pub(super) fn may_recall(&self, session_id: &str, other: &str) -> bool {
        self.recall_refusal(session_id, other).is_none()
    }

    /// Why the agent in `session_id` may not read `other`, in plain words
    /// (which rule applied, and the way out), or `None` if it may. The same
    /// "You can't read that conversation." for every rule left agents
    /// unable to tell the user what to do.
    pub(super) fn recall_refusal(&self, session_id: &str, other: &str) -> Option<String> {
        if other == session_id {
            return None;
        }
        match self.store.get_session(other) {
            Ok(Some((info, _))) if info.private => {
                return Some("That conversation is private: it stays on the user's devices and no agent can read it. Ask the user to paste what you need.".into());
            }
            Ok(Some((info, _))) if info.internal => {
                return Some("That is one of Tessera's own background sessions (such as the memory review); agents can't read those.".into());
            }
            Ok(Some(_)) => {}
            _ => return Some(format!("There is no conversation `{other}`.")),
        }
        match self.acting_bot(session_id) {
            Some(b) if b.own_memory && !self.bot_sees_session(&b, other) => Some(format!(
                "You act as {}, a bot with its own memory: it reads only its own chats and the channels it is in, not the user's other conversations. Ask the user to share what you need, or to ask from one of their own chats.",
                b.name
            )),
            _ => None,
        }
    }

    /// Whether the agent in `session_id` may see a note: every agent and bot
    /// sees every note, except one the user keeps from agents, which only a
    /// chat the user brought it into sees (and a bot that has it as
    /// knowledge).
    pub(super) fn note_visible(&self, session_id: &str, note: &Note) -> bool {
        if !note.keep_from_agents {
            return true;
        }
        self.note_granted(session_id, &note.id)
            || self
                .acting_bot(session_id)
                .is_some_and(|b| b.knowledge.contains(&note.id))
    }

    /// The user brought this note into the chat `session_id` belongs to.
    fn note_granted(&self, session_id: &str, id: &str) -> bool {
        self.rt(&self.root_session(session_id))
            .is_ok_and(|rt| rt.st.lock().internal.note_grants.iter().any(|g| g == id))
    }

    /// Notes the user brings into a chat in a message of theirs become
    /// readable there, also one kept from agents: a `[[Title]]` (or `[[id]]`)
    /// link, or a note's id (what "Ask about this" puts in the message box).
    /// Only the user's own messages do this (`Request::Send`): what agents
    /// write never widens what an agent may read.
    pub(super) fn grant_mentioned_notes(&self, session_id: &str, content: &[ContentPart]) {
        let text = content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let links: Vec<&str> = wikilinks(&text).collect();
        let named: Vec<&str> = note_ids(&text).collect();
        if links.is_empty() && named.is_empty() {
            return;
        }
        let root = self.root_session(session_id);
        let Ok(rt) = self.rt(&root) else { return };
        // Only notes kept from agents need it (the others are readable anywhere).
        let kept = |n: &Note| n.keep_from_agents;
        let mut ids: Vec<String> = named
            .into_iter()
            .filter(|id| matches!(self.store.note(id), Ok(Some(n)) if kept(&n)))
            .map(str::to_string)
            .collect();
        if !links.is_empty() {
            // The note a link means, as the app opens it: by id, else the newest of that title.
            let all = self.store.notes().unwrap_or_default();
            for target in links {
                let t = target.to_lowercase();
                if let Some(n) = all
                    .iter()
                    .find(|n| n.id == target)
                    .or_else(|| all.iter().find(|n| n.title.trim().to_lowercase() == t))
                    .filter(|n| kept(n))
                {
                    ids.push(n.id.clone());
                }
            }
        }
        let added = {
            let mut st = rt.st.lock();
            let grants = &mut st.internal.note_grants;
            let before = grants.len();
            for id in ids {
                if !grants.contains(&id) {
                    grants.push(id);
                }
            }
            grants.len() > before
        };
        if added {
            self.save(&rt);
        }
    }

    /// The bot acting in a session (a channel's current speaker).
    fn acting_bot(&self, session_id: &str) -> Option<Bot> {
        let rt = self.rt(session_id).ok()?;
        self.speaker_bot(&rt.st.lock())
    }
}

/// A note for lists: a one-line preview instead of the content.
pub(crate) fn note_info(n: &Note) -> NoteInfo {
    let text: Vec<&str> = n
        .content
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '>', '-', '*', ' ']))
        .filter(|l| !l.is_empty() && *l != n.title)
        .collect();
    NoteInfo {
        id: n.id.clone(),
        title: n.title.clone(),
        preview: one_line(&text.join(" "), 140),
        version: n.version,
        created_at: n.created_at,
        updated_at: n.updated_at,
        session_id: n.session_id.clone(),
        by_user: n.by_user,
        all_bots: !n.keep_from_agents,
        keep_from_agents: n.keep_from_agents,
    }
}

fn too_big() -> ApiError {
    bad(format!(
        "pictures in notes are at most {} MB",
        MAX_ASSET >> 20
    ))
}

/// A picture's type and file extension, from its first bytes.
pub(crate) fn picture_kind(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("image/jpeg", "jpg"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

/// `<16 hex>.<png|jpg|webp|gif>`: a name `put_note_asset` gives, never a path.
pub(crate) fn asset_name_ok(name: &str) -> bool {
    let Some((hash, ext)) = name.split_once('.') else {
        return false;
    };
    hash.len() == 16
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && matches!(ext, "png" | "jpg" | "webp" | "gif")
}

/// The pictures a note's text uses (`assets/<name>`, in an image or a link).
pub(crate) fn asset_refs(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices("assets/").filter_map(|(i, _)| {
        let rest = &text[i + 7..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
            .unwrap_or(rest.len());
        let name = rest[..end].trim_end_matches('.');
        asset_name_ok(name).then_some(name)
    })
}

/// The targets of a text's wikilinks: `[[target]]`, `[[target|shown as]]`,
/// `[[target#heading]]`.
pub(crate) fn wikilinks(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices("[[").filter_map(|(i, _)| {
        let rest = &text[i + 2..];
        let inner = &rest[..rest.find("]]")?];
        if inner.contains(['\n', '[']) {
            return None;
        }
        let target = inner.split(['|', '#']).next()?.trim();
        (!target.is_empty()).then_some(target)
    })
}

/// Note ids in a text (`nt_` and 12 letters or digits, as `save_note` makes them).
pub(crate) fn note_ids(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices("nt_").filter_map(|(i, _)| {
        let id = text.get(i..i + 15)?;
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let before = text[..i].chars().next_back().is_some_and(word);
        let after = text[i + 15..].chars().next().is_some_and(word);
        (!before
            && !after
            && id[3..]
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()))
        .then_some(id)
    })
}

/// Whether `text` links to `note`: by its title (any case) or its id.
fn links_to(text: &str, note: &Note) -> bool {
    let title = note.title.trim().to_lowercase();
    wikilinks(text).any(|t| t == note.id || t.to_lowercase() == title)
}

/// The first heading or line of the content, else "Untitled".
fn default_note_title(content: &str) -> String {
    content
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .map(|l| one_line(l, 80))
        .unwrap_or_else(|| "Untitled".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// A PNG's first bytes (its signature and size): enough to be taken as one.
    fn png(w: u32, h: u32, salt: u8) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0, salt]);
        b
    }

    async fn engine() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        (e, dir)
    }

    async fn put(e: &Engine, bytes: &[u8]) -> Result<NoteAsset, ApiError> {
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let v = e
            .handle(
                Request::PutNoteAsset {
                    mime: "image/png".into(),
                    data,
                },
                None,
            )
            .await?;
        Ok(serde_json::from_value(v).unwrap())
    }

    async fn read(e: &Engine, name: &str, offset: u64, len: u32) -> Result<FileChunk, ApiError> {
        let v = e
            .handle(
                Request::ReadNoteAsset {
                    name: name.into(),
                    offset,
                    len,
                },
                None,
            )
            .await?;
        Ok(serde_json::from_value(v).unwrap())
    }

    async fn save(e: &Engine, id: Option<&str>, content: &str) -> Note {
        let v = e
            .handle(
                Request::SaveNote {
                    id: id.map(str::to_string),
                    title: "Album".into(),
                    content: content.into(),
                    base_version: None,
                    session_id: None,
                },
                None,
            )
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    #[tokio::test]
    async fn pictures_are_kept_by_their_content_and_read_in_pieces() {
        let (e, dir) = engine().await;
        let bytes = png(640, 480, 1);
        let a = put(&e, &bytes).await.unwrap();
        assert!(
            asset_name_ok(&a.name) && a.name.ends_with(".png"),
            "{}",
            a.name
        );
        assert_eq!(
            (a.mime.as_str(), a.width, a.height),
            ("image/png", 640, 480)
        );
        // The same picture again is the same one; another is another.
        assert_eq!(put(&e, &bytes).await.unwrap().name, a.name);
        assert_ne!(put(&e, &png(640, 480, 2)).await.unwrap().name, a.name);
        let file = dir.path().join("note-assets").join(&a.name);
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only the server's user reads it: {mode:o}");
        }
        // In pieces, as the apps fetch it.
        let mut got = Vec::new();
        let mut offset = 0;
        loop {
            let c = read(&e, &a.name, offset, 7).await.unwrap();
            assert_eq!(c.size, bytes.len() as u64);
            let part = base64::engine::general_purpose::STANDARD
                .decode(&c.data)
                .unwrap();
            offset += part.len() as u64;
            got.extend(part);
            if c.eof {
                break;
            }
        }
        assert_eq!(got, bytes);
        assert!(read(&e, &a.name, 1 << 30, 10).await.is_err());
    }

    #[tokio::test]
    async fn only_pictures_of_a_sensible_size_are_kept() {
        let (e, _dir) = engine().await;
        let not = put(&e, b"<svg onload=alert(1)>").await.unwrap_err();
        assert!(not.message.contains("isn't a PNG"), "{}", not.message);
        let mut big = png(9000, 9000, 0);
        big.resize(MAX_ASSET + 1, 0);
        let too_big = put(&e, &big).await.unwrap_err();
        assert!(
            too_big.message.contains("at most 8 MB"),
            "{}",
            too_big.message
        );
        // A JPEG, a GIF and a WebP are fine (by their bytes, whatever they claim).
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0];
        jpg.extend([0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x58, 0x03, 0x20, 0, 0]);
        let j = put(&e, &jpg).await.unwrap();
        assert!(
            j.name.ends_with(".jpg") && j.mime == "image/jpeg" && (j.width, j.height) == (800, 600)
        );
        assert!(
            put(&e, b"GIF89a\x40\x01\xf0\x00\0\0")
                .await
                .unwrap()
                .name
                .ends_with(".gif")
        );
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend([0; 4]);
        webp.extend([0, 0, 0, 0, 0x7F, 0x02, 0x00, 0xDF, 0x01, 0x00]);
        let w = put(&e, &webp).await.unwrap();
        assert!(
            w.name.ends_with(".webp") && (w.width, w.height) == (640, 480),
            "{w:?}"
        );
    }

    #[tokio::test]
    async fn names_that_are_not_pictures_of_notes_are_never_read() {
        let (e, dir) = engine().await;
        std::fs::write(dir.path().join("config.toml"), "secret = 1").unwrap();
        let a = put(&e, &png(10, 10, 0)).await.unwrap();
        // A link put where a picture would be is not followed.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            dir.path().join("config.toml"),
            dir.path().join("note-assets").join("0123456789abcdef.png"),
        )
        .unwrap();
        for name in [
            "../config.toml",
            "..%2Fconfig.toml",
            "0123456789ABCDEF.png",
            "0123456789abcdef.svg",
            "0123456789abcdef.png/../../config.toml",
            "0123456789abcdef.png",
            "",
        ] {
            let err = read(&e, name, 0, 0).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::NotFound, "{name}");
        }
        assert!(read(&e, &a.name, 0, 0).await.is_ok());
    }

    #[tokio::test]
    async fn pictures_no_note_or_version_uses_are_removed_after_a_while() {
        let (e, dir) = engine().await;
        let a = put(&e, &png(1, 1, 1)).await.unwrap();
        let b = put(&e, &png(1, 1, 2)).await.unwrap();
        let c = put(&e, &png(1, 1, 3)).await.unwrap();
        let n = save(&e, None, &format!("Day one\n\n![beach](assets/{})", a.name)).await;
        // Changed to another picture: the version from before keeps the first.
        save(
            &e,
            Some(&n.id),
            &format!("Day one\n\n![sea|240](assets/{})\n", b.name),
        )
        .await;
        let dir = dir.path().join("note-assets");
        let there = |name: &str| dir.join(name).exists();
        // Still new: nothing goes.
        assert_eq!(e.0.sweep_note_assets(ASSET_GRACE), 0);
        assert!(there(&a.name) && there(&b.name) && there(&c.name));
        // Unused and old enough: only `c`.
        assert_eq!(e.0.sweep_note_assets(Duration::ZERO), 1);
        assert!(there(&a.name) && there(&b.name) && !there(&c.name));
        // The note goes (and its versions with it): its pictures can go too.
        e.handle(Request::DeleteNote { id: n.id.clone() }, None)
            .await
            .unwrap();
        assert_eq!(e.0.sweep_note_assets(Duration::ZERO), 2);
        assert!(!there(&a.name) && !there(&b.name));
        // Files that aren't pictures of notes are left alone.
        std::fs::write(dir.join("README"), "mine").unwrap();
        assert_eq!(e.0.sweep_note_assets(Duration::ZERO), 0);
        assert!(dir.join("README").exists());
    }

    #[test]
    fn pictures_and_links_are_found_in_a_note() {
        let text = "![a](assets/0123456789abcdef.png) and [file](assets/fedcba9876543210.jpg). \
                    ![x](assets/notahexname0000.png) ![y](https://example.com/assets/0123456789abcdef.gif)\n\
                    See [[Packing list]], [[Trip|the trip]], [[Notes#Today]] and [[ bad\nlink]].";
        let refs: Vec<&str> = asset_refs(text).collect();
        assert_eq!(
            refs,
            vec![
                "0123456789abcdef.png",
                "fedcba9876543210.jpg",
                "0123456789abcdef.gif"
            ]
        );
        let links: Vec<&str> = wikilinks(text).collect();
        assert_eq!(links, vec!["Packing list", "Trip", "Notes"]);
    }

    #[test]
    fn note_ids_are_found_in_a_message() {
        let text = "About the “Trip” note (nt_0123456789ab): and nt_abcdefghjkmn, \
                    not xnt_0123456789ab, nt_0123456789abc, nt_ABCDEFGHJKMN or nt_short.";
        let ids: Vec<&str> = note_ids(text).collect();
        assert_eq!(ids, vec!["nt_0123456789ab", "nt_abcdefghjkmn"]);
        assert_eq!(note_ids("ends with nt_0123456789ab").count(), 1);
        assert_eq!(note_ids("nt_0123456789a€").count(), 0);
    }

    #[tokio::test]
    async fn notes_that_link_to_a_note_are_its_backlinks() {
        let (e, _dir) = engine().await;
        let target = save(&e, None, "# Packing list\n\n- passport").await;
        let set_title = |id: &str, title: &str, content: &str| Request::SaveNote {
            id: Some(id.to_string()),
            title: title.into(),
            content: content.into(),
            base_version: None,
            session_id: None,
        };
        e.handle(set_title(&target.id, "Packing list", "- passport"), None)
            .await
            .unwrap();
        let a = save(&e, None, "Bring the [[packing list]] items.").await;
        let b = save(&e, None, &format!("By id: [[{}|that list]]", target.id)).await;
        let _c = save(&e, None, "Packing list, but no link.").await;
        let v = e
            .handle(
                Request::NoteBacklinks {
                    id: target.id.clone(),
                },
                None,
            )
            .await
            .unwrap();
        let list: Vec<NoteInfo> = serde_json::from_value(v).unwrap();
        let mut ids: Vec<&str> = list.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        let mut want = vec![a.id.as_str(), b.id.as_str()];
        want.sort();
        assert_eq!(ids, want);
    }
}
