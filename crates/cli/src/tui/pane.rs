//! One chat on screen (the whole width, or one of the chats side by side):
//! its transcript, stream, running tools, approvals, queue, scroll
//! position, draft and completion state. Backend events are routed to every
//! pane showing their session.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::text::Line;
use serde_json::Value;
use tessera_proto::*;

use super::editor::Editor;
use super::transcript::{RunningTool, Transcript};

/// Which pane (stable while it is open).
pub type PaneId = u64;

/// The model response currently streaming.
pub struct Live {
    pub content: Vec<AssistantPart>,
    pub ended: bool,
    pub version: u64,
}

#[derive(Default)]
pub struct LiveCache {
    pub key: u64,
    pub lines: Vec<Line<'static>>,
    /// (part index, split offset, width, lines) of the stable Markdown prefix.
    pub prefix: Option<(usize, usize, usize, Vec<Line<'static>>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompKind {
    Command,
    Arg,
    File,
}

pub struct CompItem {
    pub label: String,
    pub detail: String,
    pub value: String,
    /// Directory entries keep completing after acceptance.
    pub dir: bool,
}

pub struct Completion {
    pub kind: CompKind,
    pub items: Vec<CompItem>,
    pub selected: usize,
    pub range: std::ops::Range<usize>,
}

/// What an event means for the app beyond the pane's own state.
#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    Toast(String),
    /// The turn ended (busy → idle).
    TurnEnded,
    /// The session was deleted elsewhere.
    Removed,
}

/// How much of the first approval's text was shown: one that doesn't fit
/// scrolls (↑/↓, space a page), and is allowed only once every line of it
/// was on screen.
#[derive(Default)]
pub struct ApprovalView {
    /// The approval this is about (another one starts at its top).
    pub id: String,
    pub scroll: usize,
    /// The width it was wrapped at: another one starts reading again (the
    /// lines are others).
    pub width: usize,
    /// Lines of its text, and how many the card has room for right now.
    pub total: usize,
    pub rows: usize,
    /// Lines below the room right now.
    pub below: usize,
    /// Lines shown so far, from the top without a gap (jumping to the end
    /// doesn't count what was skipped).
    pub seen: usize,
    /// All of it was on screen.
    pub read: bool,
}

pub struct Pane {
    pub id: PaneId,
    pub session: Option<SessionInfo>,
    /// Bumped whenever the pane switches sessions; stale replies are dropped.
    pub epoch: u64,
    pub transcript: Transcript,
    pub live: Option<Live>,
    pub live_cache: LiveCache,
    /// Bot id of the speaker currently streaming.
    pub live_author: Option<String>,
    pub running: HashMap<String, RunningTool>,
    pub approvals: Vec<ApprovalRequest>,
    /// When the first waiting approval appeared: keys typed just before
    /// (a message in progress) don't answer it.
    pub approval_at: Option<Instant>,
    /// The first approval's card when its text is longer than the room.
    pub approval_view: ApprovalView,
    pub queued: Vec<QueuedMessage>,
    pub children: Vec<SessionInfo>,
    pub turn_started: Option<Instant>,
    /// When the session's status last changed (how long it has compacted).
    pub status_since: Option<Instant>,
    /// Sent once the pane's session exists.
    pub pending_prompt: Option<String>,
    /// Your last message as typed, until the server's copy arrives (it may
    /// come back with a secret taken out).
    pub sent: Option<String>,
    /// Messages of yours the server rewrote (typed, stored): the app drops
    /// the typed text from its own ↑ history too.
    pub rewritten: Vec<(String, String)>,
    pub creating: bool,
    /// Session id being fetched.
    pub loading: Option<String>,
    pub editor: Editor,
    pub completion: Option<Completion>,
    pub comp_dismissed: Option<String>,
    pub scroll: usize,
    pub last_total: usize,
    pub last_width: usize,
    pub page: usize,
    pub input_width: usize,
    /// Where the pane was last drawn (zero when hidden) — mouse hit tests.
    pub rect: Rect,
    pub trans_rect: Rect,
    pub input_rect: Rect,
    /// The selected queued message (↑ on an empty composer).
    pub queue_sel: Option<usize>,
    /// The composer holds this queued message for editing.
    pub editing_queued: Option<usize>,
    /// How long each assistant entry thought (ms), measured live, by seq.
    pub think_ms: HashMap<u64, u64>,
    /// The streaming message started thinking at…
    pub think_started: Option<Instant>,
    /// …and thought this long before it moved on.
    pub think_live_ms: Option<u64>,
    /// Selected option of a question.
    pub ask_sel: usize,
    /// A question put aside with esc (the apps still show it).
    pub ask_hidden: Option<String>,
}

impl Pane {
    pub fn new(id: PaneId) -> Self {
        Self {
            id,
            session: None,
            epoch: 0,
            transcript: Transcript::default(),
            live: None,
            live_cache: LiveCache::default(),
            live_author: None,
            running: HashMap::new(),
            approvals: Vec::new(),
            approval_at: None,
            approval_view: ApprovalView::default(),
            queued: Vec::new(),
            children: Vec::new(),
            turn_started: None,
            status_since: None,
            pending_prompt: None,
            sent: None,
            rewritten: Vec::new(),
            creating: false,
            loading: None,
            editor: Editor::default(),
            completion: None,
            comp_dismissed: None,
            scroll: 0,
            last_total: 0,
            last_width: 0,
            page: 10,
            input_width: 40,
            rect: Rect::default(),
            trans_rect: Rect::default(),
            input_rect: Rect::default(),
            queue_sel: None,
            editing_queued: None,
            think_ms: HashMap::new(),
            think_started: None,
            think_live_ms: None,
            ask_sel: 0,
            ask_hidden: None,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.id.as_str())
    }

    pub fn shows(&self, id: &str) -> bool {
        self.session_id() == Some(id) || self.loading.as_deref() == Some(id)
    }

    pub fn busy(&self) -> bool {
        self.session.as_ref().is_some_and(|s| is_busy(s.status))
    }

    pub fn animating(&self) -> bool {
        self.busy()
            || self.live.is_some()
            || !self.running.is_empty()
            || self.creating
            || self.loading.is_some()
    }

    /// Bot this chat is with (bot chats and private bot DMs).
    pub fn bot_id(&self) -> Option<&str> {
        self.session.as_ref().and_then(|s| s.bot_id.as_deref())
    }

    pub fn is_channel(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.kind == SessionKind::Channel)
    }

    /// A private bot-to-bot conversation: read-only for the user.
    pub fn is_dm(&self) -> bool {
        self.is_channel() && self.bot_id().is_some()
    }

    /// Forget everything about the current session.
    pub fn reset(&mut self) {
        self.epoch += 1;
        self.transcript.clear();
        self.live = None;
        self.live_cache = LiveCache::default();
        self.live_author = None;
        self.running.clear();
        self.approvals.clear();
        self.queued.clear();
        self.children.clear();
        self.turn_started = None;
        self.scroll = 0;
        self.last_total = 0;
        self.completion = None;
        self.queue_sel = None;
        self.editing_queued = None;
        self.think_ms.clear();
        self.think_started = None;
        self.think_live_ms = None;
        self.ask_sel = 0;
        self.ask_hidden = None;
    }

    /// Returns true when a turn just ended.
    pub fn apply_info(&mut self, info: SessionInfo) -> bool {
        let was_busy = self.busy();
        let busy_now = is_busy(info.status);
        if self.session.as_ref().map(|s| s.status) != Some(info.status) {
            self.status_since = Some(Instant::now());
        }
        if busy_now && !was_busy {
            self.turn_started = Some(Instant::now());
        }
        if !busy_now {
            self.turn_started = None;
            self.live = None;
            self.live_author = None;
            self.running.clear();
            self.approvals.clear();
        }
        self.session = Some(info);
        was_busy && !busy_now
    }

    pub fn apply_snapshot(
        &mut self,
        snap: SessionSnapshot,
        full: bool,
        proc_history: &[String],
        version: &mut u64,
    ) {
        if full {
            self.transcript.clear();
            self.scroll = 0;
        }
        for e in snap.entries {
            self.transcript.add_entry(e);
        }
        match snap.streaming {
            Some(s) => {
                *version += 1;
                self.live_author = s.author.clone();
                self.live = (!s.content.is_empty()).then_some(Live {
                    content: s.content,
                    ended: false,
                    version: *version,
                });
                let now = Instant::now();
                let old = std::mem::take(&mut self.running);
                for t in s.running_tools {
                    let started = old.get(&t.call_id).map(|o| o.started).unwrap_or(now);
                    self.running.insert(
                        t.call_id,
                        RunningTool {
                            name: t.name,
                            output: t.output,
                            started,
                        },
                    );
                }
            }
            None => {
                self.live = None;
                self.running.clear();
            }
        }
        if self.approvals.is_empty() && !snap.approvals.is_empty() {
            self.approval_at = Some(Instant::now());
        }
        self.approvals = snap.approvals;
        self.queued = snap.queued;
        if full {
            let mut hist = self.transcript.user_texts();
            for h in proc_history {
                if !hist.contains(h) {
                    hist.push(h.clone());
                }
            }
            self.editor.set_history(hist);
        }
        self.apply_info(snap.info);
    }

    /// Apply a session-scoped event. The caller routes only events for this
    /// pane's session (plus `SessionUpdated` for its subagents).
    pub fn on_event(&mut self, e: &Event, version: &mut u64) -> Effect {
        let sid = self.session_id().map(str::to_string);
        let mine = |id: &str| sid.as_deref() == Some(id);
        match e {
            Event::SessionUpdated { session } => {
                if mine(&session.id) {
                    if self.apply_info(session.clone()) {
                        return Effect::TurnEnded;
                    }
                } else if session.kind == SessionKind::Subagent
                    && session.parent_id.is_some()
                    && session.parent_id == sid
                    // Harness helpers (the memory review) aren't subagents
                    // you started: their outcome shows as a notice.
                    && !session.internal
                {
                    match self.children.iter_mut().find(|c| c.id == session.id) {
                        Some(c) => *c = session.clone(),
                        None => self.children.push(session.clone()),
                    }
                }
            }
            Event::SessionRemoved { session_id } => {
                self.children.retain(|c| &c.id != session_id);
                if mine(session_id) {
                    return Effect::Removed;
                }
            }
            Event::EntryAdded { session_id, entry } if mine(session_id) => {
                match &entry.body {
                    EntryBody::Assistant { .. } => {
                        self.live = None;
                        // The thinking time measured while it streamed.
                        if let Some(start) = self.think_started.take() {
                            let ms = self
                                .think_live_ms
                                .take()
                                .unwrap_or_else(|| start.elapsed().as_millis() as u64);
                            self.think_ms.insert(entry.seq, ms);
                        }
                        self.think_live_ms = None;
                    }
                    EntryBody::ToolResult { call_id, .. } => {
                        self.running.remove(call_id);
                    }
                    EntryBody::User {
                        content,
                        author: None,
                    } => {
                        let t = plain_text(content);
                        match self.sent.take() {
                            // Stored with a secret taken out: ↑ recalls what
                            // was stored, never what was typed.
                            Some(typed)
                                if typed != t && self.editor.history().last() == Some(&typed) =>
                            {
                                let mut history = self.editor.history().to_vec();
                                if let Some(last) = history.last_mut() {
                                    *last = t.clone();
                                }
                                self.editor.set_history(history);
                                self.rewritten.push((typed, t));
                            }
                            _ => {
                                if self.editor.history().last() != Some(&t) {
                                    self.editor.push_history(&t);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                self.transcript.add_entry(entry.clone());
            }
            Event::EntriesChanged {
                session_id,
                entries,
            } if mine(session_id) => {
                for entry in entries {
                    let Some(old) = self.transcript.replace_entry(entry.clone()) else {
                        continue;
                    };
                    // Your own message: ↑ recalls the rewritten text.
                    if let (
                        EntryBody::User {
                            content: was,
                            author: None,
                        },
                        EntryBody::User { content: now, .. },
                    ) = (&old.body, &entry.body)
                    {
                        let (was, now) = (plain_text(was), plain_text(now));
                        let history = self
                            .editor
                            .history()
                            .iter()
                            .map(|h| if *h == was { now.clone() } else { h.clone() })
                            .collect();
                        self.editor.set_history(history);
                        self.rewritten.push((was, now));
                    }
                }
            }
            Event::StreamStarted {
                session_id, author, ..
            } if mine(session_id) => {
                self.live_cache = LiveCache::default();
                self.think_started = None;
                self.think_live_ms = None;
                *version += 1;
                self.live = Some(Live {
                    content: Vec::new(),
                    ended: false,
                    version: *version,
                });
                self.live_author = author.clone();
            }
            Event::StreamDelta { session_id, delta } if mine(session_id) => {
                *version += 1;
                match delta {
                    StreamDelta::Thinking { .. } => {
                        if self.think_started.is_none() {
                            self.think_started = Some(Instant::now());
                        }
                    }
                    _ => {
                        if let Some(start) = self.think_started
                            && self.think_live_ms.is_none()
                        {
                            self.think_live_ms = Some(start.elapsed().as_millis() as u64);
                        }
                    }
                }
                let live = self.live.get_or_insert_with(|| Live {
                    content: Vec::new(),
                    ended: false,
                    version: 0,
                });
                live.version = *version;
                apply_delta(&mut live.content, delta.clone());
            }
            Event::StreamEnded { session_id } if mine(session_id) => {
                if let Some(l) = self.live.as_mut() {
                    l.ended = true;
                    if l.content.is_empty() {
                        self.live = None;
                    }
                }
            }
            Event::ToolStarted {
                session_id,
                call_id,
                name,
            } if mine(session_id) => {
                self.running.insert(
                    call_id.clone(),
                    RunningTool {
                        name: name.clone(),
                        output: String::new(),
                        started: Instant::now(),
                    },
                );
            }
            Event::ToolOutput {
                session_id,
                call_id,
                chunk,
            } if mine(session_id) => {
                if let Some(t) = self.running.get_mut(call_id) {
                    t.output.push_str(chunk);
                    if t.output.len() > 64 * 1024 {
                        let cut =
                            tessera_core::util::ceil_char(&t.output, t.output.len() - 48 * 1024);
                        t.output.drain(..cut);
                    }
                }
            }
            Event::ApprovalRequested {
                session_id,
                request,
            } if mine(session_id) => {
                if !self.approvals.iter().any(|a| a.id == request.id) {
                    if self.approvals.is_empty() {
                        self.approval_at = Some(Instant::now());
                    }
                    self.approvals.push(request.clone());
                }
                self.completion = None;
            }
            Event::ApprovalResolved {
                session_id,
                request_id,
                ..
            } if mine(session_id) => {
                self.approvals.retain(|a| &a.id != request_id);
            }
            Event::QueueChanged { session_id, queued } if mine(session_id) => {
                self.queued = queued.clone();
                if self.queued.is_empty() {
                    self.queue_sel = None;
                } else if let Some(s) = self.queue_sel {
                    self.queue_sel = Some(s.min(self.queued.len() - 1));
                }
            }
            Event::Notice {
                session_id: Some(id),
                level,
                message,
            } if mine(id) => match level {
                NoticeLevel::Info => return Effect::Toast(message.clone()),
                _ => self.transcript.add_local(*level, message.clone()),
            },
            _ => {}
        }
        Effect::None
    }
}

pub fn is_busy(s: SessionStatus) -> bool {
    matches!(
        s,
        SessionStatus::Running | SessionStatus::AwaitingApproval | SessionStatus::Compacting
    )
}

pub fn apply_delta(content: &mut Vec<AssistantPart>, delta: StreamDelta) {
    let (index, make): (usize, AssistantPart) = match &delta {
        StreamDelta::Text { index, .. } => (
            *index as usize,
            AssistantPart::Text {
                text: String::new(),
            },
        ),
        StreamDelta::Thinking { index, .. } => (
            *index as usize,
            AssistantPart::Thinking {
                text: String::new(),
                opaque: None,
            },
        ),
        StreamDelta::ToolCall {
            index, id, name, ..
        } => (
            *index as usize,
            AssistantPart::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: Value::String(String::new()),
            },
        ),
    };
    if index > 10_000 {
        return;
    }
    while content.len() <= index {
        content.push(AssistantPart::Text {
            text: String::new(),
        });
    }
    let same = matches!(
        (&content[index], &make),
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
    if !same {
        content[index] = make;
    }
    match (delta, &mut content[index]) {
        (StreamDelta::Text { text, .. }, AssistantPart::Text { text: t }) => t.push_str(&text),
        (StreamDelta::Thinking { text, .. }, AssistantPart::Thinking { text: t, .. }) => {
            t.push_str(&text)
        }
        (
            StreamDelta::ToolCall {
                arguments,
                id,
                name,
                ..
            },
            AssistantPart::ToolCall {
                arguments: a,
                id: pid,
                name: pname,
            },
        ) => {
            if pid.is_empty() && !id.is_empty() {
                *pid = id;
            }
            if pname.is_empty() && !name.is_empty() {
                *pname = name;
            }
            if let Value::String(acc) = a {
                acc.push_str(&arguments);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn info(id: &str, status: SessionStatus) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            revision: 0,
            title: format!("chat {id}"),
            kind: SessionKind::Chat,
            cwd: "/tmp".into(),
            model: ModelRef {
                provider: "local".into(),
                model: "m".into(),
            },
            thinking: ThinkingLevel::Medium,
            permission: PermissionMode::Default,
            status,
            parent_id: None,
            bot_id: None,
            members: Vec::new(),
            created_at: 0,
            updated_at: 0,
            last_seq: 0,
            usage: Usage::default(),
            context: ContextStats {
                tokens: 0,
                window: 0,
                compactions: 0,
                source: None,
            },
            pinned: false,
            archived: false,
            preview: String::new(),
            task: None,
            private: false,
            steps: 0,
            internal: false,
            fast: false,
            daybreak: false,
            subagents: true,
            helper_limits: None,
            helper_queued: false,
            parent_call: None,
            helper_owner: None,
            role: None,
            lean: false,
            working_for: None,
        }
    }

    #[test]
    fn deltas_accumulate() {
        let mut c = Vec::new();
        apply_delta(
            &mut c,
            StreamDelta::Thinking {
                index: 0,
                text: "hm".into(),
            },
        );
        apply_delta(
            &mut c,
            StreamDelta::Text {
                index: 1,
                text: "Hel".into(),
            },
        );
        apply_delta(
            &mut c,
            StreamDelta::Text {
                index: 1,
                text: "lo".into(),
            },
        );
        apply_delta(
            &mut c,
            StreamDelta::ToolCall {
                index: 2,
                id: "c".into(),
                name: "bash".into(),
                arguments: "{\"com".into(),
            },
        );
        apply_delta(
            &mut c,
            StreamDelta::ToolCall {
                index: 2,
                id: String::new(),
                name: String::new(),
                arguments: "mand\":1}".into(),
            },
        );
        assert_eq!(c.len(), 3);
        assert!(matches!(&c[1], AssistantPart::Text { text } if text == "Hello"));
        assert!(
            matches!(&c[2], AssistantPart::ToolCall { arguments: Value::String(a), name, .. } if a == "{\"command\":1}" && name == "bash")
        );
    }

    #[test]
    fn pane_tracks_its_own_session_only() {
        let mut v = 0;
        let mut p = Pane::new(1);
        p.apply_info(info("a", SessionStatus::Idle));
        let other = Event::StreamStarted {
            session_id: "b".into(),
            model: ModelRef {
                provider: "p".into(),
                model: "m".into(),
            },
            author: None,
        };
        assert_eq!(p.on_event(&other, &mut v), Effect::None);
        assert!(p.live.is_none());
        let start = Event::StreamStarted {
            session_id: "a".into(),
            model: ModelRef {
                provider: "p".into(),
                model: "m".into(),
            },
            author: Some("bot1".into()),
        };
        p.on_event(&start, &mut v);
        assert_eq!(p.live_author.as_deref(), Some("bot1"));
        p.on_event(
            &Event::StreamDelta {
                session_id: "a".into(),
                delta: StreamDelta::Text {
                    index: 0,
                    text: "hi".into(),
                },
            },
            &mut v,
        );
        assert!(
            matches!(&p.live.as_ref().unwrap().content[0], AssistantPart::Text { text } if text == "hi")
        );
        // Busy → idle ends the turn and clears the stream.
        p.on_event(
            &Event::SessionUpdated {
                session: info("a", SessionStatus::Running),
            },
            &mut v,
        );
        assert!(p.busy() && p.turn_started.is_some());
        assert_eq!(
            p.on_event(
                &Event::SessionUpdated {
                    session: info("a", SessionStatus::Idle)
                },
                &mut v
            ),
            Effect::TurnEnded
        );
        assert!(p.live.is_none() && !p.busy());
        // Subagent updates land in `children`; forks and "(rewound)"
        // copies also name a parent but aren't subagents.
        let mut child = info("c", SessionStatus::Running);
        child.parent_id = Some("a".into());
        child.kind = SessionKind::Subagent;
        p.on_event(&Event::SessionUpdated { session: child }, &mut v);
        assert_eq!(p.children.len(), 1);
        let mut fork = info("f", SessionStatus::Idle);
        fork.parent_id = Some("a".into());
        p.on_event(&Event::SessionUpdated { session: fork }, &mut v);
        assert_eq!(p.children.len(), 1, "a fork is not a subagent");
        let mut review = info("r", SessionStatus::Running);
        review.parent_id = Some("a".into());
        review.kind = SessionKind::Subagent;
        review.internal = true;
        p.on_event(&Event::SessionUpdated { session: review }, &mut v);
        assert_eq!(p.children.len(), 1, "the memory review is never listed");
        assert_eq!(
            p.on_event(
                &Event::SessionRemoved {
                    session_id: "a".into()
                },
                &mut v
            ),
            Effect::Removed
        );
        assert_eq!(
            p.on_event(
                &Event::Notice {
                    session_id: Some("a".into()),
                    level: NoticeLevel::Info,
                    message: "x".into()
                },
                &mut v
            ),
            Effect::Toast("x".into())
        );
    }
}
