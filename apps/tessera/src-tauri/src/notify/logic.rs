use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tessera_client::Client;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tessera_proto::{
    ContentPart, Entry, EntryBody, Event, Job, NoticeLevel, PendingNotifications, SessionInfo,
    SessionKind, SessionStatus, UserNotification,
};

/// Shorter turns aren't worth a "finished" notification.
const LONG_TURN: Duration = Duration::from_secs(20);
/// A turn of several steps, or one resumed by a subagent report, often
/// starts again right after it stops: wait this long before calling it done.
const SETTLE: Duration = Duration::from_secs(2);
/// At most one notification per session this often.
const PER_SESSION: Duration = Duration::from_secs(10);
/// `while_busy`: keep the connection this long after the work is done.
const LINGER: Duration = Duration::from_secs(120);
/// A tap older than this is no longer the session to open at startup.
const OPEN_TTL: Duration = Duration::from_secs(60);
const TITLE_MAX: usize = 60;
const PREVIEW_MAX: usize = 140;
// Agents' message text, only used when current previews policy permits.
const MESSAGE_MAX: usize = 600;
/// Notifications that belong to no session.
const APP_KEY: &str = "tessera";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeepAlive {
    /// While something runs, and a couple of minutes after.
    #[default]
    WhileBusy,
    Always,
    Off,
}

/// What the user chose. Previews are off by default: message and tool text
/// only reaches the notification shade if they turn them on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub approvals: bool,
    pub finished: bool,
    pub subagents: bool,
    pub jobs: bool,
    pub errors: bool,
    pub previews: bool,
    pub keep_alive: KeepAlive,
    /// Quiet hours: notifications arrive silently between `quiet_from`
    /// and `quiet_to` ("22:00", local time).
    pub quiet: bool,
    pub quiet_from: String,
    pub quiet_to: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // The OS permission is still only asked on first need.
            enabled: true,
            approvals: true,
            finished: true,
            subagents: false,
            jobs: true,
            errors: true,
            previews: false,
            keep_alive: KeepAlive::WhileBusy,
            quiet: false,
            quiet_from: "22:00".into(),
            quiet_to: "07:00".into(),
        }
    }
}

/// Minutes after midnight of "HH:MM".
fn minutes(hhmm: &str) -> Option<u16> {
    let (h, m) = hhmm.trim().split_once(':')?;
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl Settings {
    pub(crate) fn wants(&self, kind: Kind) -> bool {
        self.enabled
            && match kind {
                Kind::Approval => self.approvals,
                Kind::Finished => self.finished,
                Kind::Subagent => false,
                Kind::Job => self.jobs,
                Kind::Error => self.errors,
                // Asked for: whether this device gets them at all is the
                // server's per-device setting.
                Kind::Agent => true,
            }
    }

    /// Whether `now` (minutes after local midnight) is in quiet hours.
    pub(crate) fn quiet_at(&self, now: u16) -> bool {
        let (Some(from), Some(to)) = (minutes(&self.quiet_from), minutes(&self.quiet_to)) else {
            return false;
        };
        self.quiet
            && match from.cmp(&to) {
                std::cmp::Ordering::Less => from <= now && now < to,
                // Across midnight (22:00–07:00).
                std::cmp::Ordering::Greater => now >= from || now < to,
                std::cmp::Ordering::Equal => false,
            }
    }

    pub(crate) fn quiet_now(&self) -> bool {
        self.quiet && local_minutes().is_some_and(|m| self.quiet_at(m))
    }
}

/// Minutes after local midnight, now.
pub(crate) fn local_minutes() -> Option<u16> {
    #[cfg(unix)]
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as libc::time_t;
        // SAFETY: localtime_r only writes the struct we pass.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
        ok.then(|| (tm.tm_hour * 60 + tm.tm_min) as u16)
    }
    #[cfg(windows)]
    {
        let mut t = windows_sys::Win32::Foundation::SYSTEMTIME::default();
        // SAFETY: a correctly sized SYSTEMTIME output.
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        Some(t.wHour * 60 + t.wMinute)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// `notifications.json`, next to `servers.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    #[serde(flatten)]
    settings: Settings,
    /// The system prompt was shown once on our own initiative; don't nag.
    #[serde(default)]
    permission_asked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Permission {
    Granted,
    Denied,
    /// Not decided yet: the system prompt can still be shown.
    Prompt,
}

impl Permission {
    #[cfg_attr(not(mobile), allow(dead_code))]
    fn parse(s: &str) -> Self {
        match s {
            "granted" => Self::Granted,
            "denied" => Self::Denied,
            _ => Self::Prompt,
        }
    }
}

/// Ordered by importance: a queued notification is replaced by a more
/// important one for the same session, never by a less important one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Subagent,
    Job,
    Finished,
    Error,
    /// A message an agent sent with `notify`.
    Agent,
    Approval,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Note {
    /// One notification per key: a session id, or `APP_KEY`.
    pub key: String,
    pub generation: u64,
    pub transport_epoch: u64,
    pub client_epoch: Option<u64>,
    pub client_instance: Option<u64>,
    pub server_scope: String,
    pub notification_id: Option<u64>,
    pub private: bool,
    /// Opened when the notification is tapped.
    pub session: Option<String>,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    /// Subagent reports folded into this one.
    count: u32,
    /// Who sent an agent's message (shown small).
    pub from: Option<String>,
    /// When it happened (unix ms), if not now.
    pub when: Option<u64>,
    /// Quiet hours: no sound or pop-up.
    pub silent: bool,
    /// An agent asked for this one to alert like an approval.
    urgent_agent: bool,
    server_policy: bool,
    test: bool,
}

impl Note {
    /// Re-run with current settings immediately before the platform call.
    fn payload(&self, settings: &Settings) -> Self {
        let mut note = self.clone();
        if self.private || !settings.previews {
            note.generic();
        }
        note
    }

    fn sample() -> Self {
        Self::new(
            "test",
            None,
            Kind::Finished,
            "Notifications are on",
            "This is how Tessera lets you know when it needs you.".into(),
        )
    }
    fn new(key: &str, session: Option<&str>, kind: Kind, title: &str, body: String) -> Self {
        Self {
            key: key.to_string(),
            generation: 0,
            transport_epoch: 0,
            client_epoch: None,
            client_instance: None,
            server_scope: String::new(),
            notification_id: None,
            private: false,
            session: session.map(str::to_string),
            kind,
            title: title.to_string(),
            body,
            count: 1,
            from: None,
            when: None,
            silent: false,
            urgent_agent: false,
            server_policy: false,
            test: false,
        }
    }

    // Used by live desktop/Android events AND Android background pull.
    fn local_limit_body(text: &str) -> String {
        limit_body_with(text, |at| {
            at.with_timezone(&chrono::Local)
                .format("%b %-d, %-I:%M %p %:z")
                .to_string()
        })
    }
    /// A notification from the server: an agent's message (`notify`;
    /// urgent ones alert like an approval), or one the server made about a
    /// chat or a scheduled job (its `kind`, under that kind's setting).
    pub fn agent(n: &UserNotification) -> Self {
        let session = n.session_id.as_deref().filter(|s| valid_id(s));
        let title = clip(&n.title, TITLE_MAX + 20);
        let local = Self::local_limit_body(&n.body);
        let body = local
            .lines()
            .map(|l| clip(l, MESSAGE_MAX))
            .collect::<Vec<_>>()
            .join("\n");
        let key = if valid_id(&n.source_key) { n.source_key.clone() }
            else if let Some(session) = session { session.into() }
            else { agent_key(n.id) };
        let mut note = Self {
            notification_id: Some(n.id),
            private: n.private,
            from: Some(clip(&n.from, TITLE_MAX)).filter(|f| !f.is_empty()),
            when: Some(n.due_at),
            urgent_agent: n.urgent,
            server_policy: !n.reason.is_empty(),
            test: n.kind == "test",
            silent: n.level == tessera_proto::NotificationLevel::Digest,
            ..Self::new(
                &key,
                session,
                server_kind(&n.kind),
                &title,
                clip_chars(&body, MESSAGE_MAX),
            )
        };
        if n.private {
            note.generic();
        }
        note
    }

    fn generic(&mut self) {
        self.title = "Tessera".into();
        self.body = "New activity. Open Tessera for details.".into();
        self.from = None;
    }

    #[cfg_attr(not(any(mobile, test)), allow(dead_code))]
    pub fn urgent(&self) -> bool {
        self.kind == Kind::Approval || self.urgent_agent
    }

    /// What a locked screen shows instead: nothing about the content.
    #[cfg_attr(not(mobile), allow(dead_code))]
    pub fn public_body(&self) -> &'static str {
        match self.kind {
            Kind::Approval => "Needs your approval",
            Kind::Agent => "A message for you",
            _ => "New activity",
        }
    }

    fn absorb(&mut self, newer: Note) {
        if newer.kind > self.kind {
            *self = newer;
        } else if newer.kind == self.kind {
            let count = self.count + newer.count;
            *self = newer;
            self.count = count;
            if self.kind == Kind::Subagent {
                self.title = subagent_title(count);
            }
        }
    }
}

fn limit_body_with(
    text: &str,
    render: impl FnOnce(chrono::DateTime<chrono::FixedOffset>) -> String,
) -> String {
    if !text.contains("Claude plan's") {
        return text.into();
    }
    let marker = " It resets at ";
    let Some(start) = text.find(marker).map(|i| i + marker.len()) else {
        return text.into();
    };
    let token = text[start..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(token) else {
        return text.into();
    };
    let mut local = text.to_string();
    local.replace_range(start..start + token.len(), &render(at));
    local
}
/// A server notification as this device shows it, if at all: under its
/// kind's setting, and what the server says about a chat or a job only with
/// previews on; the final OS boundary reapplies the current policy.
pub(crate) fn server_note(settings: &Settings, n: &UserNotification) -> Option<Note> {
    if n.quiet || (n.kind == "helper" && !n.urgent) { return None; }
    let mut note = Note::agent(n);
    if n.urgent { note.kind = Kind::Agent; }
    if n.source_key.is_empty() && note.session.is_none() {
        note.key = agent_key(n.id);
    }
    if n.kind != "test" && !settings.wants(note.kind) {
        return None;
    }
    if n.private || !settings.previews {
        note.generic();
    }
    Some(note)
}

/// What a server notification's `kind` is here (unknown ones: a message).
fn server_kind(kind: &str) -> Kind {
    match kind {
        "job" => Kind::Job,
        "finished" => Kind::Finished,
        "error" => Kind::Error,
        "approval" | "secret" => Kind::Approval,
        "helper" => Kind::Subagent,
        _ => Kind::Agent,
    }
}

/// The notification key of an agent's message.
pub(crate) fn agent_key(id: u64) -> String {
    format!("n{id}")
}

/// At most `max` characters, line breaks kept.
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

fn subagent_title(count: u32) -> String {
    if count > 1 {
        format!("{count} subagents reported back")
    } else {
        "A subagent reported back".into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Service {
    pub run: bool,
    /// Start now, even with the app on screen (`always`). Otherwise Android
    /// starts it when the app leaves the screen.
    pub eager: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ack {
    ids: Vec<u64>,
    generation: u64,
    epoch: u64,
    client_epoch: Option<u64>,
    client_instance: Option<u64>,
    delivered: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Action {
    Post(Note),
    /// User-requested, local generic sample: never opens a server session.
    Sample,
    Withdraw(String),
    Service(Service),
    /// Tell the server these agents' messages were handled here.
    Ack(Ack),
    /// A scheduled message is due then: a phone wakes up to fetch it.
    WakeAt(u64),
}

/// Session ids come from the server. Only plain ids ever reach a
/// notification tag, an intent extra or the UI.
pub(crate) fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One line of plain text, at most `max` characters. Control and bidi
/// override characters go (they could disguise the text), runs of
/// whitespace become one space.
pub(crate) fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for word in s.split(|c: char| c.is_whitespace() || c.is_control()) {
        let word: String = word.chars().filter(|&c| !is_format_char(c)).collect();
        if word.is_empty() {
            continue;
        }
        for c in (n > 0).then_some(' ').into_iter().chain(word.chars()) {
            if n == max {
                out.truncate(out.trim_end().len());
                out.push('…');
                return out;
            }
            out.push(c);
            n += 1;
        }
    }
    out
}

fn is_format_char(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

fn is_busy(s: SessionStatus) -> bool {
    matches!(
        s,
        SessionStatus::Running | SessionStatus::AwaitingApproval | SessionStatus::Compacting
    )
}

/// Subagents and scheduled jobs report through their own notifications.
fn user_facing(kind: SessionKind) -> bool {
    matches!(kind, SessionKind::Chat | SessionKind::Channel)
}

fn joined(place: &str, detail: &str) -> String {
    let detail = clip(detail, PREVIEW_MAX);
    if detail.is_empty() {
        place.to_string()
    } else {
        format!("{place}: {detail}")
    }
}

/// A report's text without its `[Report from subagent …]` header.
fn report_text(content: &[ContentPart]) -> String {
    let text: String = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    match text.strip_prefix("[Report from subagent") {
        Some(rest) => rest.split_once("\n\n").map_or("", |(_, b)| b).to_string(),
        None => text,
    }
}

#[derive(Debug, Default)]
struct Sess {
    title: String,
    private: bool,
    kind: Option<SessionKind>,
    status: Option<SessionStatus>,
    preview: String,
    /// When the current piece of work was first seen running.
    started: Option<Instant>,
    /// When it stopped; whether it finished is decided `SETTLE` later.
    ended: Option<Instant>,
    /// It stopped while the app was in front: the user saw it.
    ended_in_view: bool,
    /// Subagent reports not notified yet.
    reports: u32,
    report_at: Option<Instant>,
    report_text: String,
    /// The latest error notice, for a failed turn's preview.
    error: String,
}

enum Settled {
    Failed,
    Finished,
    Reports(u32),
}

/// A pending approval.
#[derive(Debug)]
struct Asked {
    session: String,
    summary: String,
    /// Arrived (or was left pending) with the app in the background.
    notified: bool,
}

/// Async answers belong to one transport and one hydration attempt, not
/// merely the saved Client (which reconnects in place).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SyncToken {
    generation: u64,
    epoch: u64,
    revision: u64,
}

struct Inner {
    stored: Stored,
    permission: Permission,
    foreground: bool,
    /// Which connection the events belong to.
    generation: u64,
    transport_epoch: u64,
    sync_revision: u64,
    attached: bool,
    link: bool,
    /// "Disconnect" was chosen on the keep-alive notification: connect
    /// again when the app comes back.
    paused: bool,
    /// Android: the activity is gone; the Kotlin side can't be reached.
    activity_gone: bool,
    sessions: HashMap<String, Sess>,
    /// The harness's own helper sessions (the memory review): never
    /// counted, reported or kept alive for.
    internal: HashSet<String>,
    automatic_notifications: Option<bool>,
    jobs: HashMap<String, Option<String>>,
    buffered_jobs: std::collections::VecDeque<Job>,
    live_jobs: HashSet<String>,
    disconnected_at: Option<u64>,
    /// By approval id.
    approvals: HashMap<String, Asked>,
    /// Questions agents put on whiteboards: ask id by board id (the asks
    /// themselves live in `approvals`, like approvals).
    board_asks: HashMap<String, String>,
    last_posted: HashMap<String, Instant>,
    shown: HashMap<String, Kind>,
    queued: HashMap<String, (Instant, Note)>,
    busy_until: Option<Instant>,
    service: Option<Service>,
    missed_approval: bool,
    opened: Option<(String, Instant)>,
    /// Agents' messages shown here, by id: the chat they open.
    agent_shown: HashMap<u64, (Option<String>, String)>,
    accepted_receipts: HashMap<String, std::collections::VecDeque<u64>>,
    server_scope: String,
    client_instance: Option<u64>,
}

impl Inner {
    fn new(stored: Stored) -> Self {
        Self {
            stored,
            // Desktop needs no permission; Android reports the real state.
            permission: if cfg!(mobile) {
                Permission::Prompt
            } else {
                Permission::Granted
            },
            foreground: true,
            generation: 0,
            transport_epoch: 0,
            sync_revision: 0,
            attached: false,
            link: false,
            paused: false,
            activity_gone: false,
            sessions: HashMap::new(),
            internal: HashSet::new(),
            automatic_notifications: None,
            jobs: HashMap::new(),
            buffered_jobs: std::collections::VecDeque::new(),
            live_jobs: HashSet::new(),
            disconnected_at: None,
            approvals: HashMap::new(),
            board_asks: HashMap::new(),
            last_posted: HashMap::new(),
            shown: HashMap::new(),
            queued: HashMap::new(),
            busy_until: None,
            service: None,
            missed_approval: false,
            opened: None,
            agent_shown: HashMap::new(),
            accepted_receipts: HashMap::new(),
            server_scope: String::new(),
            client_instance: None,
        }
    }

    fn settings(&self) -> &Settings {
        &self.stored.settings
    }

    fn wants(&self, kind: Kind) -> bool {
        self.settings().wants(kind)
    }

    /// How a session is named: never the title of a private chat, nothing at
    /// all for one we know nothing about.
    fn place(&self, session: &str) -> Option<String> {
        let s = self.sessions.get(session)?;
        s.kind?;
        Some(if s.private {
            "Private chat".into()
        } else if s.title.trim().is_empty() {
            "Untitled chat".into()
        } else {
            clip(&s.title, TITLE_MAX)
        })
    }

    /// Content only with previews on, and never from a private chat (or one
    /// that might be private).
    fn previews_for(&self, session: &str) -> bool {
        self.settings().previews
            && self
                .sessions
                .get(session)
                .is_some_and(|s| s.kind.is_some() && !s.private)
    }

    fn busy(&self) -> bool {
        self.link
            && self
                .sessions
                .values()
                .any(|s| s.status.is_some_and(is_busy))
    }

    /// The linger counts from the moment the work stopped: note it before
    /// an event (or a lost link) can change that.
    fn mark_busy(&mut self, now: Instant) {
        if self.busy() {
            self.busy_until = Some(now + LINGER);
        }
    }

    fn event(&mut self, now: Instant, e: &Event) -> Vec<Action> {
        self.mark_busy(now);
        let mut out = Vec::new();
        match e {
            Event::SessionUpdated { session } => self.session(now, session),
            Event::SessionRemoved { session_id } => {
                self.sessions.remove(session_id);
                self.internal.remove(session_id);
                self.approvals.retain(|_, a| &a.session != session_id);
                out.extend(self.seen(session_id));
            }
            Event::EntryAdded { session_id, entry } => self.entry(now, session_id, entry),
            Event::StreamStarted { session_id, .. } => {
                if let Some(s) = self.sessions.get_mut(session_id) {
                    s.ended = None;
                    s.started.get_or_insert(now);
                }
            }
            Event::ApprovalRequested {
                session_id,
                request,
            } => {
                self.approvals.insert(
                    request.id.clone(),
                    Asked {
                        session: session_id.clone(),
                        summary: request.summary.clone(),
                        notified: !self.foreground,
                    },
                );
                if self.automatic_notifications != Some(true) {
                    out.extend(self.approval(now, session_id, &request.summary));
                }
            }
            Event::ApprovalResolved {
                session_id,
                request_id,
                ..
            } => {
                self.approvals.remove(request_id);
                out.extend(self.resolved(session_id));
            }
            Event::BoardUpdated { board } => match &board.ask {
                Some(ask) if !self.approvals.contains_key(&ask.id) => {
                    if let Some(old) = self.board_asks.insert(board.id.clone(), ask.id.clone()) {
                        self.approvals.remove(&old);
                    }
                    self.approvals.insert(
                        ask.id.clone(),
                        Asked {
                            session: board.session_id.clone(),
                            summary: board.title.clone(),
                            notified: !self.foreground,
                        },
                    );
                    if self.automatic_notifications != Some(true) {
                        out.extend(self.input_needed(now, &board.session_id, &board.title));
                    }
                }
                Some(_) => {}
                None => out.extend(self.board_answered(&board.id, &board.session_id)),
            },
            Event::BoardRemoved { id, session_id } => {
                out.extend(self.board_answered(id, session_id))
            }
            Event::JobUpdated { job } => {
                self.live_jobs.insert(job.id.clone());
                match self.automatic_notifications {
                    Some(false) => out.extend(self.job(now, job)),
                    None => {
                        if self.buffered_jobs.len() == 256 {
                            self.buffered_jobs.pop_front();
                        }
                        self.buffered_jobs.push_back(job.clone());
                    }
                    Some(true) => {}
                }
            }
            Event::JobRemoved { id } => {
                self.jobs.remove(id);
                self.live_jobs.insert(id.clone());
                self.buffered_jobs.retain(|job| &job.id != id);
            }
            // A scheduled run's outcome comes as a server notification
            // (`kind: "job"`), also to a phone that wasn't connected.
            Event::Notice {
                session_id,
                level: NoticeLevel::Error,
                message,
            } if self.automatic_notifications != Some(true) => out.extend(self.notice(now, session_id.as_deref(), message)),
            Event::Notification { notification } => {
                out.extend(self.agent_message(now, notification))
            }
            Event::NotificationsSeen { ids } => out.extend(self.agent_seen(ids)),
            _ => {}
        }
        out.extend(self.tick(now));
        out
    }

    fn acknowledge(&self, ids: Vec<u64>) -> Action {
        Action::Ack(Ack {
            ids,
            generation: self.generation,
            epoch: self.transport_epoch,
            client_epoch: None,
            client_instance: self.client_instance,
            delivered: false,
        })
    }

    fn accepted(&mut self, note: &Note, accepted: bool) -> Option<Action> {
        let id = note.notification_id?;
        if accepted {
            let receipts = self
                .accepted_receipts
                .entry(note.server_scope.clone())
                .or_default();
            if !receipts.contains(&id) {
                if receipts.len() == 512 {
                    receipts.pop_front();
                }
                receipts.push_back(id);
            }
        }
        if !self.accepts_post(note) {
            return None;
        }
        if !accepted {
            self.agent_shown.remove(&id);
            self.shown.remove(&note.key);
            self.last_posted.remove(&note.key);
            self.queued.remove(&note.key);
            return None;
        }
        Some(Action::Ack(Ack {
            ids: vec![id],
            generation: note.generation,
            epoch: note.transport_epoch,
            client_epoch: note.client_epoch,
            client_instance: note.client_instance,
            delivered: true,
        }))
    }

    /// An agent's message: acknowledge deliberate suppression, otherwise
    /// only the platform's accepted-post result may acknowledge it.
    fn agent_message(&mut self, now: Instant, n: &UserNotification) -> Vec<Action> {
        if tessera_proto::is_protocol_notification(n) {
            return vec![self.acknowledge(vec![n.id])];
        }
        if !n.sent {
            return vec![Action::WakeAt(n.due_at)];
        }
        if self
            .accepted_receipts
            .get(&self.server_scope)
            .is_some_and(|ids| ids.contains(&n.id))
        {
            return vec![self.acknowledge(vec![n.id])];
        }
        if n.seen || self.agent_shown.contains_key(&n.id) {
            return Vec::new();
        }
        let Some(note) = server_note(self.settings(), n) else {
            return vec![self.acknowledge(vec![n.id])];
        };
        if (self.foreground && !note.server_policy && !note.test) || (!note.test && !self.wants(note.kind)) {
            return vec![self.acknowledge(vec![n.id])];
        }
        let mut out = Vec::new();
        if let Some(post) = self.post(now, note) {
            out.push(post);
        }
        out
    }

    /// Seen on some device: remove them here too.
    fn agent_seen(&mut self, ids: &[u64]) -> Vec<Action> {
        let mut out = Vec::new();
        for id in ids {
            self.queued.retain(|_, (_, note)| note.notification_id != Some(*id));
            let key = self.agent_shown.remove(id).map(|(_, key)| key).unwrap_or_else(|| agent_key(*id));
            if !self.agent_shown.values().any(|(_, current)| current == &key) {
                self.shown.remove(&key);
                out.push(Action::Withdraw(key));
            }
        }
        out
    }

    /// What this device missed while it was away.
    fn pending(&mut self, now: Instant, p: PendingNotifications) -> Vec<Action> {
        let mut out = self.agent_seen(&p.seen);
        out.extend(p.withdraw.into_iter().filter(|key| valid_id(key)).map(Action::Withdraw));
        for n in &p.new {
            out.extend(self.agent_message(now, n));
        }
        out.extend(p.next_due.map(Action::WakeAt));
        out
    }

    fn session(&mut self, now: Instant, info: &SessionInfo) {
        // Not a real id: nothing about it may reach a notification.
        if !valid_id(&info.id) {
            return;
        }
        // A helper of the harness (the memory review), not the user's work:
        // what it learned shows in the chat, nothing about it is notified.
        if info.internal {
            self.sessions.remove(&info.id);
            self.internal.insert(info.id.clone());
            return;
        }
        let in_view = self.foreground;
        let s = self.sessions.entry(info.id.clone()).or_default();
        s.title.clone_from(&info.title);
        s.private = info.private;
        s.kind = Some(info.kind);
        if !tessera_proto::is_check_ack(&info.preview) {
            s.preview.clone_from(&info.preview);
        }
        s.status = Some(info.status);
        if is_busy(info.status) {
            // Running again before it settled: the same piece of work.
            s.ended = None;
            s.started.get_or_insert(now);
        } else if s.started.is_some() && s.ended.is_none() {
            s.ended = Some(now);
            s.ended_in_view = in_view;
        }
    }

    fn entry(&mut self, now: Instant, session_id: &str, entry: &Entry) {
        let Some(s) = self.sessions.get_mut(session_id) else {
            return;
        };
        match &entry.body {
            EntryBody::User {
                author: Some(author),
                content,
            } if author.starts_with("subagent:")
                && s.kind.is_some_and(user_facing)
                && !self.internal.contains(&author["subagent:".len()..]) =>
            {
                s.reports += 1;
                s.report_at.get_or_insert(now + SETTLE);
                s.report_text = report_text(content);
            }
            EntryBody::Notice {
                level: NoticeLevel::Error,
                text,
                ..
            } => s.error.clone_from(text),
            _ => {}
        }
    }

    fn approval(&mut self, now: Instant, session: &str, summary: &str) -> Option<Action> {
        if !valid_id(session) {
            return None;
        }
        let body = match self.place(session) {
            Some(place) if self.previews_for(session) => joined(&place, summary),
            Some(place) => place,
            None => "Open Tessera to review it.".into(),
        };
        if !self.foreground && self.wants(Kind::Approval) && self.permission != Permission::Granted
        {
            self.missed_approval = true;
        }
        let note = Note::new(
            session,
            Some(session),
            Kind::Approval,
            "Tessera needs your approval",
            body,
        );
        self.post(now, note)
    }

    /// An agent is waiting for the user's input on a whiteboard (a signature,
    /// a choice…): as urgent as an approval, and under the same setting.
    fn input_needed(&mut self, now: Instant, session: &str, title: &str) -> Option<Action> {
        if !valid_id(session) {
            return None;
        }
        let body = match self.place(session) {
            Some(place) if self.previews_for(session) => joined(&place, title),
            Some(place) => place,
            None => "Open Tessera to answer.".into(),
        };
        let note = Note::new(
            session,
            Some(session),
            Kind::Approval,
            "Tessera needs your input",
            body,
        );
        self.post(now, note)
    }

    fn board_answered(&mut self, board: &str, session: &str) -> Option<Action> {
        let ask = self.board_asks.remove(board)?;
        self.approvals.remove(&ask);
        self.resolved(session)
    }

    /// Answered (maybe on another device): the notification is stale.
    fn resolved(&mut self, session: &str) -> Option<Action> {
        if self.approvals.values().any(|a| a.session == session) {
            return None;
        }
        if self
            .queued
            .get(session)
            .is_some_and(|(_, n)| n.kind == Kind::Approval)
        {
            self.queued.remove(session);
        }
        if self.shown.get(session) == Some(&Kind::Approval) {
            self.shown.remove(session);
            return Some(Action::Withdraw(session.to_string()));
        }
        None
    }

    fn job(&mut self, now: Instant, job: &Job) -> Option<Action> {
        let before = self
            .jobs
            .insert(job.id.clone(), job.last_status.clone())
            .flatten();
        let status = job.last_status.as_deref();
        // Only a run seen going from "running" to done: saving a job also
        // sends its old status.
        let just_finished = before.is_none()
            && self
                .disconnected_at
                .is_some_and(|away| job.last_end.is_some_and(|end| end > away));
        if (before.as_deref() != Some("running") && !just_finished)
            || !matches!(status, Some("ok" | "error"))
        {
            return None;
        }
        let session = job.last_session.as_deref().filter(|s| valid_id(s));
        let key = session.unwrap_or(&job.id);
        if !valid_id(key) {
            return None;
        }
        let title = if status == Some("error") {
            "A scheduled job failed"
        } else {
            "A scheduled job finished"
        };
        let name = clip(&job.name, TITLE_MAX);
        let body = if !session.is_some_and(|s| self.previews_for(s)) || name.is_empty() {
            "Scheduled job".into()
        } else {
            name
        };
        self.post(now, Note::new(key, session, Kind::Job, title, body))
    }

    fn notice(&mut self, now: Instant, session: Option<&str>, message: &str) -> Option<Action> {
        let session = session.filter(|s| valid_id(s));
        let generic = || "Open Tessera for details.".to_string();
        let body = match session {
            Some(id) => match self.place(id) {
                Some(place) if self.previews_for(id) => joined(&place, message),
                Some(place) => place,
                None => generic(),
            },
            None if self.settings().previews => clip(message, PREVIEW_MAX),
            None => generic(),
        };
        let key = session.unwrap_or(APP_KEY);
        self.post(
            now,
            Note::new(key, session, Kind::Error, "Something went wrong", body),
        )
    }

    /// Deadlines: settled turns, subagent reports, rate-limited notes, and
    /// the keep-alive.
    fn tick(&mut self, now: Instant) -> Vec<Action> {
        self.last_posted.retain(|_, t| now < *t + PER_SESSION);
        let mut out = Vec::new();
        let due: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.ended.is_some_and(|t| now >= t + SETTLE) || s.report_at.is_some_and(|t| now >= t)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if let Some(note) = self.settle(now, &id) {
                out.extend(self.post(now, note));
            }
        }
        let due: Vec<String> = self
            .queued
            .iter()
            .filter(|(_, (t, _))| now >= *t)
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            if let Some((_, note)) = self.queued.remove(&key) {
                out.extend(self.post(now, note));
            }
        }
        out.extend(self.sync_service(now));
        out
    }

    fn settle(&mut self, now: Instant, id: &str) -> Option<Note> {
        let wants_finished = self.wants(Kind::Finished);
        let server_owned = self.automatic_notifications == Some(true);
        let s = self.sessions.get_mut(id)?;
        let facing = s.kind.is_some_and(user_facing);
        // Reports that arrive while it works (or is settling) are summed up
        // when the work ends.
        s.report_at = None;
        let settled = match s.ended {
            Some(ended) if now >= ended + SETTLE => {
                let took = ended.saturating_duration_since(s.started.unwrap_or(ended));
                let reports = std::mem::take(&mut s.reports);
                s.started = None;
                s.ended = None;
                if std::mem::take(&mut s.ended_in_view) {
                    return None;
                }
                if s.status == Some(SessionStatus::Error) {
                    Settled::Failed
                } else if took >= LONG_TURN && wants_finished {
                    Settled::Finished
                } else {
                    Settled::Reports(reports)
                }
            }
            Some(_) => return None,
            None if s.status.is_some_and(is_busy) => return None,
            None => Settled::Reports(std::mem::take(&mut s.reports)),
        };
        if !facing || server_owned {
            return None;
        }
        let (error, preview, report) = (s.error.clone(), s.preview.clone(), s.report_text.clone());
        let place = self.place(id)?;
        let previews = self.previews_for(id);
        let detail = |d: &str| {
            if previews {
                joined(&place, d)
            } else {
                place.clone()
            }
        };
        Some(match settled {
            Settled::Failed => {
                Note::new(id, Some(id), Kind::Error, "A task failed", detail(&error))
            }
            Settled::Finished => Note::new(
                id,
                Some(id),
                Kind::Finished,
                "A task finished",
                detail(&preview),
            ),
            Settled::Reports(0) => return None,
            Settled::Reports(n) => Note {
                count: n,
                ..Note::new(
                    id,
                    Some(id),
                    Kind::Subagent,
                    &subagent_title(n),
                    detail(&report),
                )
            },
        })
    }

    fn post(&mut self, now: Instant, mut note: Note) -> Option<Action> {
        if note.private
            || !self.settings().previews
            || note
                .session
                .as_ref()
                .is_some_and(|s| {
                    // Typed server messages carry their privacy bit. Local
                    // fallback events need hydration before showing previews.
                    self.sessions.get(s).map_or(note.notification_id.is_none(), |s| s.private)
                })
        {
            note.generic();
        }
        if !note.test && ((self.foreground && !note.server_policy) || !self.wants(note.kind)) {
            return None;
        }
        if let Some(last) = self.last_posted.get(&note.key).copied()
            && !note.server_policy && !note.urgent_agent
            && now < last + PER_SESSION
        {
            match self.queued.get_mut(&note.key) {
                Some((_, queued)) => queued.absorb(note),
                None => {
                    self.queued
                        .insert(note.key.clone(), (last + PER_SESSION, note));
                }
            }
            return None;
        }
        if let Some((_, mut queued)) = self.queued.remove(&note.key) {
            queued.absorb(note);
            note = queued;
        }
        if note.private
            || !self.settings().previews
            || note
                .session
                .as_ref()
                .is_some_and(|s| {
                    // Typed server messages carry their privacy bit. Local
                    // fallback events need hydration before showing previews.
                    self.sessions.get(s).map_or(note.notification_id.is_none(), |s| s.private)
                })
        {
            note.generic();
        }
        if let Some(id) = note.notification_id {
            self.agent_shown.insert(id, (note.session.clone(), note.key.clone()));
        }
        self.last_posted.insert(note.key.clone(), now);
        self.shown.insert(note.key.clone(), note.kind);
        note.silent |= !note.server_policy && self.settings().quiet_now() && !note.urgent_agent;
        Some(Action::Post(note))
    }

    fn seen(&mut self, session: &str) -> Vec<Action> {
        self.queued.remove(session);
        if let Some(s) = self.sessions.get_mut(session) {
            s.reports = 0;
            s.report_at = None;
        }
        let agents: Vec<u64> = self
            .agent_shown
            .iter()
            .filter(|(_, (s, _))| s.as_deref() == Some(session))
            .map(|(id, _)| *id)
            .collect();
        let mut out = self.agent_seen(&agents);
        out.extend(
            self.shown
                .remove(session)
                .map(|_| Action::Withdraw(session.to_string())),
        );
        out
    }

    fn set_foreground(&mut self, now: Instant, foreground: bool) -> Vec<Action> {
        self.foreground = foreground;
        let mut out = Vec::new();
        if foreground {
            // Back in the app: whatever was waiting is visible there.
            self.queued.clear();
        } else if self.automatic_notifications != Some(true) {
            // Left with an approval still unanswered: the agent is blocked
            // on it, so say so once.
            let mut waiting = Vec::new();
            for a in self.approvals.values_mut() {
                if !std::mem::replace(&mut a.notified, true) {
                    waiting.push((a.session.clone(), a.summary.clone()));
                }
            }
            for (session, summary) in waiting {
                out.extend(self.approval(now, &session, &summary));
            }
        }
        out.extend(self.tick(now));
        out
    }

    fn service_want(&mut self, now: Instant) -> Service {
        let off = Service {
            run: false,
            eager: false,
        };
        if !self.attached || self.paused {
            return off;
        }
        self.mark_busy(now);
        let busy = self.busy();
        match self.settings().keep_alive {
            KeepAlive::Off => off,
            KeepAlive::Always => Service {
                run: true,
                eager: true,
            },
            KeepAlive::WhileBusy => Service {
                run: busy || self.busy_until.is_some_and(|t| now < t),
                eager: false,
            },
        }
    }

    fn sync_service(&mut self, now: Instant) -> Option<Action> {
        let want = self.service_want(now);
        if self.service == Some(want) {
            return None;
        }
        self.service = Some(want);
        Some(Action::Service(want))
    }

    fn next_deadline(&self) -> Option<Instant> {
        let sessions = self
            .sessions
            .values()
            .flat_map(|s| [s.ended.map(|t| t + SETTLE), s.report_at]);
        let queued = self.queued.values().map(|(t, _)| Some(*t));
        let linger = self
            .service
            .filter(|s| s.run && !s.eager)
            .and(self.busy_until);
        sessions
            .chain(queued)
            .chain(std::iter::once(linger))
            .flatten()
            .min()
    }

    fn invalidate(&mut self) -> Vec<Action> {
        self.opened = None;
        self.last_posted.clear();
        self.agent_shown.clear();
        self.shown
            .drain()
            .map(|(key, _)| Action::Withdraw(key))
            .collect()
    }

    fn attach(&mut self) -> u64 {
        self.disconnected_at = None;
        self.generation += 1;
        self.attached = true;
        self.paused = false;
        self.link = false;
        self.transport_epoch += 1;
        self.sessions.clear();
        self.internal.clear();
        self.board_asks.clear();
        self.buffered_jobs.clear();
        self.live_jobs.clear();
        self.jobs.clear();
        self.automatic_notifications = None;
        self.approvals.clear();
        self.queued.clear();
        self.generation
    }

    fn detach(&mut self) {
        self.generation += 1;
        self.attached = false;
        self.link = false;
        self.transport_epoch += 1;
        self.sessions.clear();
        self.internal.clear();
        self.board_asks.clear();
        self.buffered_jobs.clear();
        self.live_jobs.clear();
        self.jobs.clear();
        self.automatic_notifications = None;
        self.approvals.clear();
        self.queued.clear();
        self.busy_until = None;
    }

    fn transport(&mut self, now: Instant, up: bool, observed_ms: u64) -> Vec<Action> {
        self.mark_busy(now);
        let mut actions = Vec::new();
        if !up {
            if self.link {
                self.disconnected_at = Some(observed_ms);
            }
            self.transport_epoch += 1;
            self.automatic_notifications = None;
            self.buffered_jobs.clear();
            self.live_jobs.clear();
            self.jobs.clear();
            self.sessions.clear();
            self.internal.clear();
            self.approvals.clear();
            self.board_asks.clear();
            self.queued.clear();
            actions.extend(self.invalidate());
        }
        self.link = up;
        actions.extend(self.tick(now));
        actions
    }

    fn accepts_ack(&self, ack: &Ack, client: &Client) -> bool {
        self.attached
            && self.link
            && ack.generation == self.generation
            && ack.epoch == self.transport_epoch
            && ack.client_instance == self.client_instance
            && ack.client_instance == Some(client.instance_id())
    }

    fn platform_payload(&self, note: &Note) -> Option<Note> {
        (note.key == "test" || self.accepts_post(note)).then(|| note.payload(self.settings()))
    }

    fn accepts_post(&self, note: &Note) -> bool {
        self.attached
            && self.link
            && note.generation == self.generation
            && note.transport_epoch == self.transport_epoch
            && note.client_instance == self.client_instance
    }

    fn begin_sync(&mut self, generation: u64) -> Option<SyncToken> {
        if generation != self.generation || !self.attached || !self.link {
            return None;
        }
        self.sync_revision += 1;
        Some(SyncToken {
            generation,
            epoch: self.transport_epoch,
            revision: self.sync_revision,
        })
    }

    fn accepts_sync(&self, token: SyncToken) -> bool {
        self.attached
            && self.link
            && token.generation == self.generation
            && token.epoch == self.transport_epoch
            && token.revision == self.sync_revision
    }

    fn negotiated(&mut self, token: SyncToken, automatic: bool) -> bool {
        if !self.accepts_sync(token) {
            return false;
        }
        self.automatic_notifications = Some(automatic);
        true
    }

    fn replay_jobs(&mut self, now: Instant) -> Vec<Action> {
        let jobs = std::mem::take(&mut self.buffered_jobs);
        if self.automatic_notifications == Some(false) {
            jobs.iter().filter_map(|j| self.job(now, j)).collect()
        } else {
            Vec::new()
        }
    }

    fn hydrated(
        &mut self,
        token: SyncToken,
        now: Instant,
        sessions: Option<Vec<SessionInfo>>,
        jobs: Option<Vec<Job>>,
        pending: Option<PendingNotifications>,
    ) -> Vec<Action> {
        if !self.accepts_sync(token) {
            return Vec::new();
        }
        let mut out = self.seed(now, sessions);
        if self.automatic_notifications == Some(false) {
            for job in jobs.iter().flatten() {
                if !self.live_jobs.contains(&job.id) {
                    out.extend(self.job(now, job));
                }
            }
        }
        if let Some(p) = pending {
            out.extend(self.pending(now, p));
        }
        out
    }

    fn seed(&mut self, now: Instant, sessions: Option<Vec<SessionInfo>>) -> Vec<Action> {
        self.mark_busy(now);
        let mut out = Vec::new();
        for info in sessions.iter().flatten() {
            let was = self.sessions.get(&info.id).and_then(|s| s.status);
            self.session(now, info);
            if info.internal {
                continue;
            }
            // Asked while we were away (or before we connected).
            if info.status == SessionStatus::AwaitingApproval
                && was != Some(SessionStatus::AwaitingApproval)
            {
                let shown_on = match info.kind {
                    SessionKind::Subagent => info.parent_id.clone().unwrap_or(info.id.clone()),
                    _ => info.id.clone(),
                };
                out.extend(self.approval(now, &shown_on, ""));
            }
        }
        out.extend(self.tick(now));
        out
    }

    /// After an approval arrived with no permission to show it: ask once,
    /// when the user is back.
    fn ask_after_miss(&mut self) -> bool {
        let ask = std::mem::take(&mut self.missed_approval)
            && self.permission == Permission::Prompt
            && !self.stored.permission_asked
            && self.wants(Kind::Approval);
        if ask {
            self.stored.permission_asked = true;
        }
        ask
    }
}

pub(crate) struct Notifier {
    client: Mutex<Option<Client>>,
    dispatch: tokio::sync::Mutex<()>,
    save_gate: std::sync::Arc<Mutex<()>>,
    file: PathBuf,
    inner: std::sync::Arc<Mutex<Inner>>,
    tx: tokio::sync::mpsc::UnboundedSender<Action>,
    /// Re-arms the timer after a change that may move a deadline.
    wake: tokio::sync::Notify,
}

impl Notifier {
    /// The only connection-bound producer->consumer handoff. The caller
    /// holds inner throughout stamping and enqueueing (including deadlines).
    fn publish(&self, i: &Inner, mut actions: Vec<Action>) {
        for action in &mut actions {
            if let Action::Post(note) = action {
                note.generation = i.generation;
                note.transport_epoch = i.transport_epoch;
                note.client_instance = i.client_instance;
                note.server_scope.clone_from(&i.server_scope);
                note.client_epoch = self
                    .client
                    .lock()
                    .as_ref()
                    .and_then(Client::connected_epoch);
            }
        }
        for action in &mut actions {
            if let Action::Ack(ack) = action
                && ack.client_epoch.is_none()
            {
                ack.client_instance = i.client_instance;
                ack.client_epoch = self
                    .client
                    .lock()
                    .as_ref()
                    .and_then(Client::connected_epoch);
            }
        }
        for action in actions {
            let _ = self.tx.send(action);
        }
    }

    fn with_at(&self, now: Instant, f: impl FnOnce(&mut Inner, Instant) -> Vec<Action>) {
        let mut i = self.inner.lock();
        let actions = f(&mut i, now);
        self.publish(&i, actions);
    }

    fn with(&self, f: impl FnOnce(&mut Inner, Instant) -> Vec<Action>) {
        self.with_at(Instant::now(), f);
        self.wake.notify_one();
    }

    /// No wake: re-arming our own deadline loop would spin.
    fn tick(&self, now: Instant) {
        self.with_at(now, Inner::tick);
    }

    fn sample(&self) {
        self.with(|_, _| vec![Action::Sample]);
    }

    fn settings(&self) -> Settings {
        self.inner.lock().settings().clone()
    }

    async fn apply_settings(&self, settings: Settings) -> Result<(), String> {
        self.apply_settings_with(settings, crate::private_file::write_verified)
            .await
    }

    async fn apply_settings_with(
        &self,
        settings: Settings,
        persist: impl FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()> + Send + 'static,
    ) -> Result<(), String> {
        let _dispatch = self.dispatch.lock().await;
        let gate = self.save_gate.clone();
        let inner = self.inner.clone();
        let file = self.file.clone();
        let pending = settings.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let _save = gate.lock();
            let mut stored = inner.lock().stored.clone();
            stored.settings = pending;
            let data = serde_json::to_vec_pretty(&stored).map_err(std::io::Error::other)?;
            let saved = persist(&file, &data);
            // Don't delete an off-policy file the save successfully installed,
            // including a verified payload with a later durability error.
            if saved.is_err()
                && !stored.settings.previews
                && std::fs::read(&file).ok().as_deref() != Some(data.as_slice())
            {
                let _ = std::fs::remove_file(&file);
            }
            saved
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(|result| result);
        let previews = settings.previews;
        self.with(|i, now| {
            i.stored.settings = settings;
            i.tick(now)
        });
        saved.map_err(|_| {
            if !previews {
                "Previews are off in the app, but this phone might still show previews until it can save. Please try saving again.".to_string()
            } else { "Couldn't save notification settings. Please try again.".to_string() }
        })
    }

    /// A new connection: returns its generation, which tags its events.
    pub async fn attach(&self, client: Option<Client>, scope: String) -> u64 {
        let _dispatch = self.dispatch.lock().await;
        let mut i = self.inner.lock();
        let mut actions = i.invalidate();
        i.client_instance = client.as_ref().map(Client::instance_id);
        *self.client.lock() = client;
        if !i.accepted_receipts.contains_key(&scope) && i.accepted_receipts.len() >= 32 {
            i.accepted_receipts.clear();
        }
        i.server_scope = scope;
        let generation = i.attach();
        actions.extend(i.tick(Instant::now()));
        self.publish(&i, actions);
        generation
    }

    pub async fn detach(&self) {
        let _dispatch = self.dispatch.lock().await;
        self.with(|i, now| {
            let mut actions = i.invalidate();
            i.client_instance = None;
            *self.client.lock() = None;
            i.detach();
            actions.extend(i.tick(now));
            actions
        });
    }

    pub fn event(&self, generation: u64, e: &Event) {
        self.with(|i, now| {
            if i.generation != generation || !i.attached || !i.link {
                return Vec::new();
            }
            i.event(now, e)
        });
    }

    pub async fn link(&self, generation: u64, up: bool) {
        let observed_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        self.link_at(generation, up, observed_ms).await;
    }

    async fn link_at(&self, generation: u64, up: bool, observed_ms: u64) {
        let _dispatch = self.dispatch.lock().await;
        self.with(|i, now| {
            if i.generation != generation {
                return Vec::new();
            }
            i.transport(now, up, observed_ms)
        });
    }

    pub fn foreground(&self, foreground: bool) {
        self.with(|i, now| {
            if foreground {
                i.activity_gone = false;
            }
            i.set_foreground(now, foreground)
        });
    }

    fn set_permission(&self, p: Permission) {
        self.inner.lock().permission = p;
    }

    pub fn pause(&self) {
        self.with(|i, now| {
            i.paused = true;
            i.tick(now)
        });
    }

    /// "Disconnect" was chosen earlier; true once, when it is time to
    /// connect again.
    #[cfg_attr(not(mobile), allow(dead_code))]
    pub fn take_paused(&self) -> bool {
        std::mem::take(&mut self.inner.lock().paused)
    }

    #[cfg_attr(not(mobile), allow(dead_code))]
    pub fn activity_gone(&self) {
        self.inner.lock().activity_gone = true;
    }

    #[cfg_attr(not(mobile), allow(dead_code))]
    fn reachable(&self) -> bool {
        !self.inner.lock().activity_gone
    }
}

/// Loads the settings; the returned receiver feeds `start`.
pub(crate) fn load(file: PathBuf) -> (Notifier, tokio::sync::mpsc::UnboundedReceiver<Action>) {
    let stored: Stored = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let notifier = Notifier {
        client: Mutex::new(None),
        dispatch: tokio::sync::Mutex::new(()),
        save_gate: std::sync::Arc::new(Mutex::new(())),
        file,
        inner: std::sync::Arc::new(Mutex::new(Inner::new(stored))),
        tx,
        wake: tokio::sync::Notify::new(),
    };
    (notifier, rx)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
