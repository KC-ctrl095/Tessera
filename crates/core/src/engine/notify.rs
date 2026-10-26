//! Notifications for the user's devices (the `notify` tool, a test from
//! Settings) and the files they can download from a chat (`share_file`,
//! web view downloads, their own uploads).
//!
//! Notifications are stored, sent when due (the scheduler checks every few
//! seconds) and shown once per device: connected apps get them as events,
//! a phone in the background asks `pending_notifications` when it wakes up.
//! Only devices that take notifications get either.

use super::*;
use crate::store::NewNotification;

use crate::tools::share::MAX_SHARE;

/// How much of Tessera's own copies is kept (the oldest go first).
const SHARED_QUOTA: u64 = 5 << 30;
/// Largest piece of a file per `read_shared_file`.
const MAX_CHUNK: u32 = 1 << 20;
pub(crate) const TITLE_MAX: usize = 80;
pub(crate) const BODY_MAX: usize = 600;
/// A scheduled notification can wait this long at most.
pub(crate) const MAX_DELAY_MS: u64 = 366 * 86_400_000;
/// How many alerts an agent's notifications may page from one source in an
/// hour (more go in the digest, saying why: a runaway or misled agent can't
/// ring every device all night), and scheduled ones in all.
const MAX_PER_HOUR: usize = 12;
const MAX_SCHEDULED: usize = 100;
/// The notification center lists this many sent ones at most.
const INBOX_MAX: u32 = 500;

use tessera_proto::{
    NotificationDelivery, NotificationLevel, NotificationPolicy, NotificationSourceRule,
};

fn default_level(n: &NewNotification) -> (NotificationLevel, String) {
    use NotificationLevel::*;
    let (level, reason) = if n.urgent {
        (
            Page,
            format!(
                "{} marked this urgent",
                if n.from.is_empty() {
                    "Tessera"
                } else {
                    &n.from
                }
            ),
        )
    } else if matches!(n.kind.as_str(), "approval" | "secret") {
        (Page, "an approval is waiting".into())
    } else if n.kind == "error" {
        (Page, "work stopped and needs you".into())
    } else if n.kind == "reminder" {
        (Page, "a reminder you asked for".into())
    } else if n.kind == "helper" || (n.quiet && n.kind != "job") {
        (Silent, "routine activity, kept without an alert".into())
    } else if n.source == "bot"
        || n.source == "job"
        || matches!(n.kind.as_str(), "job" | "message" | "progress")
    {
        (
            Digest,
            if n.source == "job" || n.kind == "job" {
                "a scheduled check finished"
            } else {
                "agents' updates go in the digest"
            }
            .into(),
        )
    } else if n.kind == "finished" {
        (Page, "a reply you asked for is ready".into())
    } else if n.kind.is_empty() || n.kind == "test" {
        (Page, "you asked for this alert".into())
    } else {
        (Silent, "routine status, kept without an alert".into())
    };
    (level, reason)
}

fn quiet_at(policy: &NotificationPolicy, minute: u16) -> bool {
    policy.quiet
        && if policy.quiet_from < policy.quiet_to {
            policy.quiet_from <= minute && minute < policy.quiet_to
        } else if policy.quiet_from > policy.quiet_to {
            minute >= policy.quiet_from || minute < policy.quiet_to
        } else {
            false
        }
}

fn cap_level(level: NotificationLevel, cap: NotificationLevel) -> NotificationLevel {
    use NotificationLevel::*;
    match (level, cap) {
        (Silent, _) | (_, Silent) => Silent,
        (Digest, _) | (_, Digest) => Digest,
        _ => Page,
    }
}

fn policy_level(
    n: &NewNotification,
    policy: &NotificationPolicy,
    minute: u16,
) -> (NotificationLevel, String) {
    let (mut level, mut reason) = default_level(n);
    if n.kind != "test" {
        if let Some(rule) = policy.sources.iter().find(|r| r.key == n.source_key) {
            let capped = cap_level(level, rule.level);
            if capped != level {
                reason = if capped == NotificationLevel::Silent {
                    "you muted this source"
                } else {
                    "you chose less from this source"
                }
                .into();
            }
            level = capped;
        }
        if level == NotificationLevel::Page && !n.urgent && quiet_at(policy, minute) {
            level = NotificationLevel::Digest;
            reason = "quiet hours, saved for the digest".into();
        }
    }
    (level, reason)
}

/// Long work's notifications by their title. A self-recovering usage
/// wait is inbox-only, not an action-needed alert.
fn kind_of_long_work(title: &str) -> &'static str {
    if title.starts_with("Stopped") {
        "error"
    } else if title.starts_with("Finished") {
        "finished"
    } else {
        ""
    }
}

/// What each chat's agent sent lately: (when, title and body), by chat.
type RecentSends = parking_lot::Mutex<HashMap<String, Vec<(u64, String)>>>;

fn recent_sends() -> &'static RecentSends {
    static RECENT: std::sync::OnceLock<RecentSends> = std::sync::OnceLock::new();
    RECENT.get_or_init(Default::default)
}

/// The agent of `session` (a root chat) sent the user a notification since
/// `since_ms`.
pub(super) fn sent_since(session: &str, since_ms: u64) -> bool {
    recent_sends()
        .lock()
        .get(session)
        .is_some_and(|v| v.iter().any(|(t, _)| *t >= since_ms))
}

impl Engine {
    /// Whether `device` gets `notification` events (the server sends them
    /// only to devices that take notifications).
    pub fn takes_notifications(&self, device: &str) -> bool {
        self.0.store.device_notify(device).unwrap_or(true)
    }
}

/// One line of plain text: control and bidi-override characters dropped,
/// whitespace runs collapsed, at most `max` characters.
pub(crate) fn plain_line(s: &str, max: usize) -> String {
    let words: Vec<String> = s
        .split(|c: char| c.is_whitespace() || c.is_control())
        .map(|w| {
            w.chars()
                .filter(|c| !is_format_char(*c))
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    clip(&words.join(" "), max)
}

/// At most `max` characters, `…` included.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// Plain text that keeps its line breaks (at most a blank line between
/// paragraphs).
pub(crate) fn plain_text(s: &str, max: usize) -> String {
    let lines: Vec<String> = s.lines().map(|l| plain_line(l, max)).collect();
    let mut out = String::new();
    let mut blank = false;
    for l in lines {
        if l.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank { "\n\n" } else { "\n" });
        }
        blank = false;
        out.push_str(&l);
    }
    clip(&out, max)
}

fn is_format_char(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// A download name: the last path component, no control characters, not
/// empty or a dot name.
pub(crate) fn file_name(raw: &str) -> String {
    let name: String = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .take(120)
        .collect();
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        "file".into()
    } else {
        name.to_string()
    }
}

/// A MIME type from the extension (downloads only need a sensible one).
pub(crate) fn mime_of(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "epub" => "application/epub+zip",
        "apk" => "application/vnd.android.package-archive",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        _ => "application/octet-stream",
    }
}

impl Inner {
    pub(super) fn notify_request(&self, req: Request, device: Option<&str>) -> ApiResult {
        // Checked here too: a request queued by a device that was signed
        // out meanwhile reads or changes nothing (a machine's login never
        // has a notification center).
        if let Some(d) = device
            && !self
                .store
                .devices(None)
                .map_err(internal)?
                .iter()
                .any(|x| x.id == d && x.client != DeviceClient::Machine)
        {
            return Err(ApiError::new(
                ErrorCode::Unauthorized,
                "this device is signed out",
            ));
        }
        match req {
            Request::GetNotificationPolicy => to_value(self.notification_policy().map_err(bad)?),
            Request::SetNotificationPolicy { mut policy } => {
                if policy.daily_at.len() > 4
                    || policy.daily_at.iter().any(|m| *m >= 1440)
                    || policy.quiet_from >= 1440
                    || policy.quiet_to >= 1440
                {
                    return Err(bad("choose valid times, at most four a day"));
                }
                let _gate = self.notification_gate.lock();
                policy.sources = self.notification_policy().map_err(bad)?.sources;
                self.store
                    .kv_set(
                        "notification_policy",
                        &serde_json::to_string(&policy).map_err(internal)?,
                    )
                    .map_err(internal)?;
                self.store
                    .kv_set("notification_digest_next", "")
                    .map_err(internal)?;
                self.sched.notify_one();
                to_value(policy)
            }
            Request::SetNotificationSource { key, level } => {
                let _gate = self.notification_gate.lock();
                let mut policy = self.notification_policy().map_err(bad)?;
                let item = self
                    .visible_notifications(
                        self.store.notifications(INBOX_MAX).map_err(internal)?,
                        device,
                    )
                    .into_iter()
                    .find(|n| !n.private && n.source_key == key);
                let name = item
                    .map(|n| if n.from.is_empty() { n.source } else { n.from })
                    .or_else(|| {
                        policy
                            .sources
                            .iter()
                            .find(|r| r.key == key)
                            .map(|r| r.name.clone())
                    })
                    .ok_or_else(|| bad("no such notification source"))?;
                policy.sources.retain(|r| r.key != key);
                if let Some(level) = level {
                    if policy.sources.len() >= 500 {
                        return Err(bad("too many source choices"));
                    }
                    policy.sources.push(NotificationSourceRule {
                        key: key.clone(),
                        name,
                        level,
                    });
                    for mut n in self
                        .store
                        .notifications(INBOX_MAX)
                        .map_err(internal)?
                        .into_iter()
                        .filter(|n| !n.private && n.sent && n.source_key == key)
                    {
                        n.level = cap_level(n.level, level);
                        n.quiet = n.level != NotificationLevel::Page;
                        n.reason = if level == NotificationLevel::Silent {
                            "you muted this source"
                        } else {
                            "you chose less from this source"
                        }
                        .into();
                        self.store
                            .update_notification_policy(&n)
                            .map_err(internal)?;
                    }
                }
                self.store
                    .kv_set(
                        "notification_policy",
                        &serde_json::to_string(&policy).map_err(internal)?,
                    )
                    .map_err(internal)?;
                self.inbox_changed();
                to_value(policy)
            }
            Request::TestNotification { delay_secs } => {
                if delay_secs > 3600 {
                    return Err(bad("a test can wait up to an hour"));
                }
                // One waiting test at a time: tests must never crowd out reminders.
                if self
                    .store
                    .notifications(0)
                    .map_err(internal)?
                    .iter()
                    .any(|n| n.kind == "test" && !n.sent)
                {
                    return Err(bad("a test alert is already on its way"));
                }
                let n = self.send_notification(NewNotification { title: "Test alert".into(), body: "An urgent test from your server. Open Settings to check device reports.".into(), kind: "test".into(), source: "system".into(), source_key: "system_test".into(), from: "Tessera".into(), urgent: true, due_at: now_ms() + delay_secs * 1000, ..Default::default() }).map_err(bad)?;
                to_value(n)
            }
            Request::NotificationDeliveries { id } => {
                self.check_notification_ids(&[id], device)?;
                let list = self
                    .store
                    .devices(device)
                    .map_err(internal)?
                    .into_iter()
                    .filter(|d| d.client != DeviceClient::Machine)
                    .map(|d| NotificationDelivery {
                        delivered: self
                            .store
                            .notification_delivered(id, &d.id)
                            .unwrap_or(false),
                        device_id: d.id,
                        name: d.name,
                    })
                    .collect::<Vec<_>>();
                to_value(list)
            }
            Request::ReportNotificationDelivery { ids } => {
                self.check_notification_ids(&ids, device)?;
                let d = device.ok_or_else(|| bad("a device login is required"))?;
                self.store
                    .report_notification_delivery(d, &ids)
                    .map_err(internal)?;
                Ok(Value::Null)
            }
            Request::SetDeviceNotify { id, notify } => {
                if !self
                    .store
                    .set_device_notify(&id, notify)
                    .map_err(internal)?
                {
                    return Err(ApiError::new(ErrorCode::NotFound, "no such device"));
                }
                to_value(self.device_list(device)?)
            }
            Request::ListNotifications | Request::ListNotificationInbox => {
                let limit = if matches!(req, Request::ListNotificationInbox) {
                    INBOX_MAX
                } else {
                    50
                };
                let list = self.store.notifications(limit).map_err(internal)?;
                to_value(self.visible_notifications(list, device))
            }
            Request::SendNotification {
                title,
                body,
                session_id,
                delay_secs,
            } => {
                if let Some(s) = &session_id {
                    self.rt(s)?;
                }
                let delay = delay_secs.saturating_mul(1000);
                if delay > MAX_DELAY_MS {
                    return Err(bad("a notification can be scheduled up to a year ahead"));
                }
                let session_id = session_id.map(|s| self.root_session(&s));
                let source_key = session_id
                    .as_deref()
                    .map(|s| self.job_notification_source(s))
                    .unwrap_or_default();
                let from = session_id
                    .as_deref()
                    .map(|s| self.notification_sender(s))
                    .unwrap_or_else(|| "Tessera".into());
                let n = self
                    .send_notification(NewNotification {
                        title,
                        body,
                        session_id: session_id.clone(),
                        source_key,
                        from,
                        due_at: now_ms() + delay,
                        ..Default::default()
                    })
                    .map_err(bad)?;
                if let Some(session) = session_id {
                    let mut recent = recent_sends().lock();
                    let sends = recent.entry(session).or_default();
                    sends.retain(|(at, _)| now_ms() < at.saturating_add(3_600_000));
                    sends.push((now_ms(), format!("{}\u{0}{}", n.title, n.body)));
                    if sends.len() > MAX_PER_HOUR {
                        sends.remove(0);
                    }
                }
                to_value(n)
            }
            Request::NegotiateNotifications => to_value(true),
            Request::NotificationPresence {
                active,
                session_ids,
            } => {
                let d = device.ok_or_else(|| bad("a device login is required"))?;
                if session_ids.len() > 32 {
                    return Err(bad("too many visible chats"));
                }
                for s in &session_ids {
                    if !self.notification_chat_visible(s, device) {
                        return Err(bad("no such chat"));
                    }
                }
                {
                    let mut presence = self.notification_presence.lock();
                    presence.retain(|_, (at, _)| at.elapsed().as_secs() < 45);
                    if active {
                        presence.insert(d.into(), (std::time::Instant::now(), session_ids.clone()));
                    } else {
                        presence.remove(d);
                    }
                }
                if active {
                    let viewed = self
                        .store
                        .unshown_notifications(d, now_ms())
                        .map_err(internal)?
                        .into_iter()
                        .filter(|n| {
                            !n.urgent
                                && n.session_id
                                    .as_ref()
                                    .is_some_and(|s| session_ids.contains(s))
                        })
                        .map(|n| n.id)
                        .collect::<Vec<_>>();
                    self.store
                        .mark_notifications_shown(d, &viewed)
                        .map_err(internal)?;
                }
                Ok(Value::Null)
            }
            Request::PendingNotifications | Request::PendingNotificationsV2 => {
                let mut pending = self.pending_notifications(device)?;
                if matches!(req, Request::PendingNotifications) {
                    pending.new.retain(|n| n.kind.is_empty());
                }
                to_value(pending)
            }
            Request::AckNotifications { ids } => {
                self.check_notification_ids(&ids, device)?;
                if let Some(d) = device {
                    self.store
                        .mark_notifications_shown(d, &ids)
                        .map_err(internal)?;
                    let key = format!("notify_tags:{d}");
                    let mut tags: Vec<(u64, String)> = self
                        .store
                        .kv_get(&key)
                        .map_err(internal)?
                        .and_then(|v| serde_json::from_str(&v).ok())
                        .unwrap_or_default();
                    for id in ids {
                        if let Some(n) = self.store.notification(id).map_err(internal)?
                            && !n.private
                            && !n
                                .session_id
                                .as_deref()
                                .is_some_and(|s| self.store.is_private(s))
                            && let Some(tag) = if n.source_key.is_empty() {
                                n.session_id
                            } else {
                                Some(n.source_key)
                            }
                        {
                            tags.retain(|(old, _)| *old != id);
                            tags.push((id, tag));
                        }
                    }
                    let skip = tags.len().saturating_sub(INBOX_MAX as usize);
                    tags.drain(..skip);
                    self.store
                        .kv_set(&key, &serde_json::to_string(&tags).map_err(internal)?)
                        .map_err(internal)?;
                }
                Ok(Value::Null)
            }
            Request::SeenNotifications { ids, session_id } => {
                self.check_notification_ids(&ids, device)?;
                if let Some(s) = &session_id
                    && !self.device_sees(s, device)
                {
                    return Err(ApiError::new(ErrorCode::NotFound, "no such chat"));
                }
                self.notifications_seen(&ids, session_id.as_deref());
                Ok(Value::Null)
            }
            Request::CancelNotification { id } => {
                self.check_notification_ids(&[id], device)?;
                if !self.store.delete_notification(id).map_err(internal)? {
                    return Err(ApiError::new(ErrorCode::NotFound, "no such notification"));
                }
                // Devices that show (or would wake up for) it let it go.
                let _ = self.events.send(Event::NotificationsSeen { ids: vec![id] });
                Ok(Value::Null)
            }
            Request::ClearNotifications { ids } => {
                self.check_notification_ids(&ids, device)?;
                let gone = self.store.clear_notifications(&ids).map_err(internal)?;
                if !gone.is_empty() {
                    let _ = self.events.send(Event::NotificationsSeen { ids: gone });
                }
                Ok(Value::Null)
            }
            Request::ListSharedFiles {
                session_id,
                board_id,
            } => {
                let files = match (session_id, board_id) {
                    (Some(s), _) => self.store.shared_files(Some(&[s]), None),
                    (None, Some(b)) => self.store.shared_files(None, Some(&b)),
                    (None, None) => return Err(bad("give a session_id or a board_id")),
                }
                .map_err(internal)?;
                to_value(
                    files
                        .into_iter()
                        .map(|(f, _)| f)
                        .filter(|f| Path::new(&f.path).is_file())
                        .collect::<Vec<_>>(),
                )
            }
            Request::ReadSharedFile { id, offset, len } => {
                to_value(self.read_shared_file(&id, offset, len)?)
            }
            _ => Err(bad("not a notifications request")),
        }
    }

    /// What `device` may see of `list`, in order: nothing about another
    /// device's private chat or a chat that is gone (a private chat's go
    /// with a restart), no protocol messages, at most `INBOX_MAX` sent.
    fn visible_notifications(
        &self,
        list: Vec<UserNotification>,
        device: Option<&str>,
    ) -> Vec<UserNotification> {
        let mut known: HashMap<String, bool> = HashMap::new();
        let mut sent = 0;
        list.into_iter()
            .filter(|n| {
                let ok = !tessera_proto::is_protocol_notification(n)
                    && n.session_id.as_deref().is_none_or(|s| {
                        *known
                            .entry(s.to_string())
                            .or_insert_with(|| self.notification_chat_visible(s, device))
                    });
                if ok && n.sent {
                    sent += 1;
                }
                ok && (!n.sent || sent <= INBOX_MAX)
            })
            .map(|n| self.notification_payload(n))
            .collect()
    }

    /// The chat a notification is about is there and `device` may see it.
    fn notification_chat_visible(&self, session: &str, device: Option<&str>) -> bool {
        self.device_sees(session, device) && self.store.session_exists(session)
    }

    pub(super) fn notification_visible(&self, n: &UserNotification, device: Option<&str>) -> bool {
        n.session_id
            .as_deref()
            .is_none_or(|s| self.notification_chat_visible(s, device))
    }

    /// Presence only affects the device that sent it. A desktop never owns
    /// the phone's alert, and merely viewing a tile does not mark a new alert read.
    fn chat_in_view(&self, session: &str, device: Option<&str>) -> bool {
        device
            .and_then(|d| self.notification_presence.lock().get(d).cloned())
            .is_some_and(|(at, chats)| {
                at.elapsed().as_secs() < 45 && chats.iter().any(|s| s == session)
            })
    }

    /// Some device shows this chat, focused, right now (fresh presence).
    fn chat_watched_anywhere(&self, session: &str) -> bool {
        self.notification_presence
            .lock()
            .values()
            .any(|(at, chats)| at.elapsed().as_secs() < 45 && chats.iter().any(|s| s == session))
    }

    pub(super) fn notification_delivery(&self, n: &UserNotification, device: Option<&str>) -> bool {
        if !self.notification_visible(n, device) || n.quiet || n.seen {
            return false;
        }
        if n.kind == "test" || n.urgent {
            return true;
        }
        !n.session_id
            .as_deref()
            .is_some_and(|s| self.chat_in_view(s, device))
    }

    pub(super) fn job_notification_source(&self, session: &str) -> String {
        self.store
            .job_run(session)
            .ok()
            .flatten()
            .map(|r| format!("job_{}", r.job_id))
            .unwrap_or_default()
    }

    fn notification_policy(&self) -> Result<NotificationPolicy, String> {
        self.store
            .kv_get("notification_policy")
            .map_err(|e| e.to_string())
            .map(|v| {
                v.and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| NotificationPolicy {
                        daily_at: vec![18 * 60],
                        quiet_from: 22 * 60,
                        quiet_to: 7 * 60,
                        ..Default::default()
                    })
            })
    }

    fn apply_notification_policy(&self, n: &mut NewNotification) -> Result<(), String> {
        if n.source.is_empty() {
            if n.kind == "helper" {
                n.source = "helper".into();
            } else if let Some(session) = &n.session_id {
                let job = self.job_notification_source(session);
                if !job.is_empty() {
                    n.source = "job".into();
                    n.source_key = job;
                } else {
                    let bot = if matches!(n.kind.as_str(), "" | "message" | "progress") {
                        self.rt(session)
                            .ok()
                            .and_then(|rt| self.speaker_id(&rt.st.lock()))
                    } else {
                        None
                    };
                    if let Some(bot) = bot {
                        n.source = "bot".into();
                        n.source_key = format!("bot_{bot}");
                    } else {
                        n.source = "chat".into();
                    }
                }
            } else if n.source_key.starts_with("job_") {
                n.source = "job".into();
            } else {
                n.source = "system".into();
            }
        }
        if n.source_key.is_empty() {
            n.source_key = n
                .session_id
                .as_ref()
                .map(|s| {
                    if n.source == "chat" {
                        s.clone()
                    } else {
                        format!("{}_{s}", n.source)
                    }
                })
                .unwrap_or_else(|| "system".into());
        }
        let time = jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC));
        (n.level, n.reason) = policy_level(
            n,
            &self.notification_policy()?,
            time.hour() as u16 * 60 + time.minute() as u16,
        );
        // An agent's notifications (urgent ones too) page at most
        // MAX_PER_HOUR times an hour per source; the rest go in the digest.
        // Never rejected, never silent. Approvals, secrets and errors come
        // from Tessera itself and aren't counted. Scheduled ones count when due.
        if n.level == NotificationLevel::Page
            && matches!(n.kind.as_str(), "" | "message" | "reminder")
            && n.due_at <= now_ms()
        {
            let now = now_ms();
            let mut pages = self.notification_pages.lock();
            pages.retain(|_, times| {
                times.retain(|t| now < t.saturating_add(3_600_000));
                !times.is_empty()
            });
            let times = pages.entry(n.source_key.clone()).or_default();
            if times.len() >= MAX_PER_HOUR {
                n.level = NotificationLevel::Digest;
                n.reason = format!(
                    "more than {MAX_PER_HOUR} alerts from here in an hour; the rest go in the digest"
                );
            } else {
                times.push(now);
            }
        }
        // A reply the user is watching on some device (focused there in the
        // last 45 s) needs no alert on another: they have it already. Only
        // replies: approvals and urgent news stay each device's own.
        if n.level == NotificationLevel::Page
            && n.kind == "finished"
            && !n.urgent
            && n.session_id
                .as_deref()
                .is_some_and(|s| self.chat_watched_anywhere(s))
        {
            n.level = NotificationLevel::Silent;
            n.reason = "you were watching this chat".into();
        }
        n.quiet = n.level != NotificationLevel::Page;
        Ok(())
    }

    fn deliver_digest(&self, now: u64) -> Result<(), String> {
        let policy = self.notification_policy()?;
        let next = self
            .store
            .kv_get("notification_digest_next")
            .map_err(|e| e.to_string())?
            .and_then(|v| v.parse::<u64>().ok());
        if next.is_some_and(|at| at <= now) {
            if let Some(notification) = self
                .store
                .assemble_notification_digest(now)
                .map_err(|e| e.to_string())?
            {
                let _ = self.events.send(Event::Notification { notification });
                self.inbox_changed();
            }
        } else if next.is_some() {
            return Ok(());
        }
        let next = if policy.hourly {
            now / 3_600_000 * 3_600_000 + 3_600_000
        } else {
            let times = if policy.daily_at.is_empty() {
                vec![18 * 60]
            } else {
                policy.daily_at
            };
            times
                .into_iter()
                .filter_map(|minute| {
                    crate::jobs::next_run(&format!("{} {} * * *", minute % 60, minute / 60), now)
                        .ok()
                })
                .min()
                .ok_or("could not schedule the digest in the server time zone")?
        };
        self.store
            .kv_set("notification_digest_next", &next.to_string())
            .map_err(|e| e.to_string())
    }
    /// Redact at the wire boundary too, including explicit messages to old apps.
    fn notification_payload(&self, mut n: UserNotification) -> UserNotification {
        n.private |= n
            .session_id
            .as_deref()
            .is_some_and(|s| self.store.is_private(s));
        if n.private {
            n.title = "Tessera".into();
            n.body.clear();
            n.from.clear();
            n.reason = "private chat activity".into();
            n.source_key.clear();
        }
        n
    }

    /// Ids a device acts on must be ones it may see (unknown ids are
    /// harmless: they change nothing).
    fn check_notification_ids(&self, ids: &[u64], device: Option<&str>) -> Result<(), ApiError> {
        if ids.len() > 600 {
            return Err(bad("too many notifications at once"));
        }
        for id in ids {
            if let Some(n) = self.store.notification(*id).map_err(internal)?
                && !self.notification_visible(&n, device)
            {
                return Err(ApiError::new(ErrorCode::NotFound, "no such notification"));
            }
        }
        Ok(())
    }

    /// How a notification names its sender: the bot, or the chat.
    pub(super) fn notification_sender(&self, session_id: &str) -> String {
        let Ok(rt) = self.rt(session_id) else {
            return "Tessera".into();
        };
        // In a channel, the bot speaking now (not the channel).
        let (bot, title, private) = {
            let st = rt.st.lock();
            (self.speaker_id(&st), st.info.title.clone(), st.info.private)
        };
        if private {
            return "Private chat".into();
        }
        if let Some(b) = bot.and_then(|b| self.find_bot(&b)) {
            return plain_line(&b.name, 40);
        }
        let t = plain_line(&title, 60);
        if t.is_empty() { "Tessera".into() } else { t }
    }

    /// Store a notification and send it (or tell devices when it is due).
    pub(super) fn send_notification(&self, n: NewNotification) -> Result<UserNotification, String> {
        Ok(self
            .send_notification_once(n, None)?
            .expect("no dedup window"))
    }

    fn send_notification_once(
        &self,
        mut n: NewNotification,
        window: Option<(&str, u64)>,
    ) -> Result<Option<UserNotification>, String> {
        let _policy = self.notification_gate.lock();
        if crate::tools::meta::is_check_ack(&n.title) || crate::tools::meta::is_check_ack(&n.body) {
            return Err("Completion acknowledgements are internal, not notifications.".into());
        }
        n.title = plain_line(&n.title, TITLE_MAX);
        n.body = plain_text(&n.body, BODY_MAX);
        n.from = plain_line(&n.from, 60);
        if n.title.is_empty() && n.body.is_empty() {
            return Err("a notification needs a title or a message".into());
        }
        if n.title.is_empty() {
            n.title = if n.from.is_empty() {
                "Tessera".into()
            } else {
                n.from.clone()
            };
        }
        if n.due_at > now_ms() + MAX_DELAY_MS {
            return Err("a notification can be scheduled up to a year ahead".into());
        }
        if !n.quiet
            && n.due_at > now_ms()
            && self
                .store
                .notifications(0)
                .map_err(|e| e.to_string())?
                .len()
                >= MAX_SCHEDULED
        {
            return Err(format!(
                "{MAX_SCHEDULED} notifications are already scheduled; cancel some first."
            ));
        }
        self.apply_notification_policy(&mut n)?;
        let Some((stored, recorded)) = self
            .store
            .record_notification_once(&n, window)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        if stored.sent && !stored.urgent && !stored.quiet {
            let viewers = self
                .notification_presence
                .lock()
                .iter()
                .filter(|(_, (at, chats))| {
                    at.elapsed().as_secs() < 45
                        && stored
                            .session_id
                            .as_ref()
                            .is_some_and(|s| chats.contains(s))
                })
                .map(|(device, _)| device.clone())
                .collect::<Vec<_>>();
            for device in viewers {
                self.store
                    .mark_notifications_shown(&device, &[stored.id])
                    .map_err(|e| e.to_string())?;
            }
        }
        let stored = self.notification_payload(stored);
        if recorded == crate::store::Recorded::Same {
            self.inbox_changed();
            return Ok(Some(stored));
        }
        if let crate::store::Recorded::Changed { previous } = recorded {
            let _ = self.events.send(Event::NotificationsSeen {
                ids: vec![previous],
            });
        }
        // Distinct news stays in the inbox. Only an exact repeat is grouped.
        if !stored.quiet {
            let _ = self.events.send(Event::Notification {
                notification: stored.clone(),
            });
        }
        self.inbox_changed();
        // On time, not at the scheduler's next round (it still catches any
        // this misses, e.g. after a restart).
        let wait = stored.due_at.saturating_sub(now_ms());
        if !stored.sent
            && wait < 86_400_000
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            let me = self.me.clone();
            rt.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                if let Some(me) = me.upgrade() {
                    me.deliver_due_notifications();
                }
            });
        }
        Ok(Some(stored))
    }

    /// Long work ended, or started waiting for a usage limit: tell the
    /// user's devices, as the chat (the user has likely walked away). Not
    /// for subagents, jobs or rooms, and not when the agent already sent a
    /// notification since `since_ms`. A private chat's says nothing of it.
    pub(super) fn notify_long_work(&self, rt: &Rt, since_ms: u64, title: &str, body: String) {
        self.notify_long_work_window(rt, since_ms, title, body, None);
    }

    /// True only after this window was recorded (now or by an earlier run).
    pub(super) fn notify_long_work_window(
        &self,
        rt: &Rt,
        since_ms: u64,
        title: &str,
        body: String,
        window: Option<(&str, u64)>,
    ) -> bool {
        let (id, kind, internal, private, depth) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                st.info.kind,
                st.info.internal || st.info.working_for.is_some(),
                st.info.private,
                st.depth,
            )
        };
        if kind != SessionKind::Chat || internal || depth > 0 {
            return false;
        }
        if sent_since(&id, since_ms) || self.notification_action_pending(&id) {
            return false;
        }
        let quiet = kind_of_long_work(title).is_empty();
        let quiet_identity = window
            .filter(|_| quiet)
            .map(|(identity, _)| format!("quiet_{identity}"));
        let window =
            window.map(|(identity, reset)| (quiet_identity.as_deref().unwrap_or(identity), reset));
        let recorded = self.send_notification_once(
            NewNotification {
                title: title.into(),
                body: if private { String::new() } else { body },
                from: self.notification_sender(&id),
                session_id: Some(id),
                due_at: now_ms(),
                kind: kind_of_long_work(title).into(),
                quiet,
                // Identical words in a later turn are still a new awaited
                // reply. The delivery policy withdraws the older revision.
                replace: if kind_of_long_work(title) == "finished" {
                    crate::store::Replace::New
                } else {
                    crate::store::Replace::Unread
                },
                ..Default::default()
            },
            window,
        );
        // A self-recovering wait must not consume the one terminal alert
        // for this reset window or suppress the actual awaited reply.
        recorded.is_ok() && !quiet
    }

    /// Record merged chat activity. Only action-needed and final-reply
    /// kinds are eligible for the central device-delivery policy.
    pub(super) fn keep_activity(
        &self,
        session: &str,
        kind: &str,
        title: &str,
        body: String,
        replace: crate::store::Replace,
    ) -> Option<u64> {
        if kind == "finished" && self.notification_action_pending(session) {
            return None;
        }
        let private = self.store.is_private(session);
        let n = NewNotification {
            title: title.into(),
            body: if private { String::new() } else { body },
            from: self.notification_sender(session),
            session_id: Some(session.to_string()),
            due_at: now_ms(),
            kind: kind.into(),
            quiet: !matches!(kind, "approval" | "finished" | "error"),
            replace,
            ..Default::default()
        };
        match self.send_notification(n) {
            Ok(n) => Some(n.id),
            Err(e) => {
                tracing::warn!("couldn't keep a {kind} notification: {e}");
                None
            }
        }
    }

    /// The chat's unread `kind` notification no longer needs the user (an
    /// approval was answered): read now (gone if it was only a moment).
    pub(super) fn activity_resolved(&self, session: &str, kind: &str) {
        if let Ok(Some(n)) = self.store.unread_notification(session, kind) {
            self.notifications_seen(&[n.id], None);
        }
    }

    /// Devices refresh their notification center. An empty
    /// `notifications_seen` marks nothing read and withdraws nothing: older
    /// apps ignore it, and it reaches devices that take no notifications.
    pub(super) fn inbox_changed(&self) {
        let _ = self
            .events
            .send(Event::NotificationsSeen { ids: Vec::new() });
    }

    /// The first line of the agent's last words in `session` after `after_seq`.
    pub(super) fn last_words(&self, session: &str, after_seq: u64) -> String {
        let entries = self.store.entries(session, after_seq).unwrap_or_default();
        entries
            .iter()
            .rev()
            .filter(|e| !crate::tools::meta::is_protocol_entry(e))
            .find_map(|e| match &e.body {
                EntryBody::Assistant { content, .. } => {
                    content.iter().rev().find_map(|p| match p {
                        AssistantPart::Text { text }
                            if !text.trim().is_empty()
                                && !crate::tools::meta::is_check_ack(text) =>
                        {
                            text.lines()
                                .find(|l| !l.trim().is_empty())
                                .map(|l| plain_line(l, 200))
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// `notify` from an agent, from the chat the user sees (its pages are
    /// capped per source and hour in `apply_notification_policy`).
    pub(super) fn tool_notify(
        &self,
        session_id: &str,
        n: crate::tools::notify::Outgoing,
    ) -> Result<crate::tools::notify::Sent, String> {
        let now = now_ms();
        let session = self.root_session(session_id);
        let key = format!(
            "{}\u{0}{}",
            plain_line(&n.title, TITLE_MAX),
            plain_text(&n.body, BODY_MAX)
        );
        // No per-source time throttle. New text and urgent items must never
        // be rejected because another result arrived earlier in the hour.
        let mut recent = recent_sends().lock();
        let mine = recent.entry(session.clone()).or_default();
        mine.retain(|(t, _)| now < t + 3_600_000);
        mine.push((now, key));
        if mine.len() > MAX_PER_HOUR {
            mine.remove(0);
        }
        drop(recent);
        let sent = self.send_notification(NewNotification {
            title: n.title,
            body: n.body,
            from: self.notification_sender(&session),
            session_id: Some(session.clone()),
            urgent: n.urgent,
            due_at: n.at.unwrap_or(now),
            source_key: self.job_notification_source(&session),
            // A reminder (`at`, later) is something the user asked for: it
            // alerts like urgent news; other updates go in the digest.
            kind: if n.urgent {
                ""
            } else if n.at.is_some_and(|at| at > now.saturating_add(30_000)) {
                "reminder"
            } else {
                "message"
            }
            .into(),
            ..Default::default()
        })?;
        let devices = self
            .store
            .notified_devices()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        Ok(crate::tools::notify::Sent {
            id: sent.id,
            due_at: sent.due_at,
            scheduled: !sent.sent,
            quiet: sent.quiet,
            level: sent.level,
            reason: sent.reason,
            grouped_into: sent.grouped_into,
            devices,
        })
    }

    /// `cancel` from an agent: only a reminder of its own chat.
    pub(super) fn cancel_scheduled_notification(
        &self,
        session: &str,
        id: u64,
    ) -> Result<(), String> {
        match self.store.notification(id).map_err(|e| e.to_string())? {
            Some(n) if n.session_id.as_deref() != Some(self.root_session(session).as_str()) => {
                Err(format!("no scheduled notification {id} in this chat"))
            }
            Some(n) if !n.sent => {
                let _ = self.store.delete_notification(id);
                let _ = self.events.send(Event::NotificationsSeen { ids: vec![id] });
                Ok(())
            }
            Some(_) => Err(format!("notification {id} was already sent")),
            None => Err(format!("no scheduled notification {id}")),
        }
    }

    /// Send the scheduled notifications whose time has come (late ones too:
    /// the server may have been off).
    pub(super) fn deliver_due_notifications(&self) {
        let _policy = self.notification_gate.lock();
        let now = now_ms();
        let Ok(due) = self.store.take_due_notifications(now) else {
            return;
        };
        let any = !due.is_empty();
        for mut notification in due {
            if !tessera_proto::is_protocol_notification(&notification) {
                let mut policy = NewNotification {
                    title: notification.title.clone(),
                    body: notification.body.clone(),
                    session_id: notification.session_id.clone(),
                    from: notification.from.clone(),
                    source: notification.source.clone(),
                    source_key: notification.source_key.clone(),
                    kind: notification.kind.clone(),
                    urgent: notification.urgent,
                    due_at: now,
                    ..Default::default()
                };
                if let Err(error) = self.apply_notification_policy(&mut policy) {
                    tracing::warn!(%error, "due notification policy failed");
                    continue;
                }
                notification.level = policy.level;
                notification.reason = policy.reason;
                notification.quiet = policy.quiet;
                let _ = self.store.update_notification_policy(&notification);
                if !notification.quiet {
                    let _ = self.events.send(Event::Notification {
                        notification: self.notification_payload(notification),
                    });
                }
            }
        }
        if let Err(error) = self.deliver_digest(now) {
            tracing::warn!(%error, "digest failed");
        }
        if any {
            self.inbox_changed();
        }
    }

    fn pending_notifications(
        &self,
        device: Option<&str>,
    ) -> Result<PendingNotifications, ApiError> {
        let now = now_ms();
        let on = device.is_none_or(|d| self.store.device_notify(d).unwrap_or(true));
        let (new, seen) = match device {
            Some(d) => (
                self.store
                    .unshown_notifications(d, now)
                    .map_err(internal)?
                    .into_iter()
                    .filter(|n| on || n.kind == "test")
                    .collect(),
                self.store.seen_since_shown(d, now).map_err(internal)?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        // Never another device's private chat's (nor about a chat that is gone).
        let mine = |s: Option<&str>| s.is_none_or(|s| self.device_sees(s, device));
        let mut new = self.visible_notifications(new, device);
        new.retain(|n| self.notification_delivery(n, device));
        let seen: Vec<u64> = seen
            .into_iter()
            .filter(|id| {
                self.store
                    .notification(*id)
                    .ok()
                    .flatten()
                    .is_none_or(|n| self.notification_visible(&n, device))
            })
            .collect();
        let mut waiting = self.waiting_chats();
        waiting.retain(|w| mine(Some(&w.session_id)) && !self.chat_in_view(&w.session_id, device));
        let all = self.visible_notifications(
            self.store.notifications(INBOX_MAX).map_err(internal)?,
            device,
        );
        let key = |n: &UserNotification| {
            if !n.source_key.is_empty() {
                Some(n.source_key.clone())
            } else {
                n.session_id.clone()
            }
        };
        let unread_keys: Vec<String> = all
            .iter()
            .filter(|n| n.sent && !n.seen && !n.quiet)
            .filter_map(key)
            .collect();
        let mut withdraw: Vec<String> = all
            .iter()
            .filter(|n| seen.contains(&n.id))
            .filter_map(key)
            .filter(|k| !unread_keys.contains(k))
            .collect();
        let tags: Vec<(u64, String)> = device
            .map(|d| self.store.kv_get(&format!("notify_tags:{d}")))
            .transpose()
            .map_err(internal)?
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        withdraw.extend(
            tags.into_iter()
                .map(|(_, tag)| tag)
                .filter(|tag| !unread_keys.contains(tag)),
        );
        withdraw.sort();
        withdraw.dedup();
        Ok(PendingNotifications {
            new,
            seen,
            withdraw,
            next_due: if on {
                self.store
                    .notifications(0)
                    .map_err(internal)?
                    .into_iter()
                    .filter(|n| !n.sent && self.notification_visible(n, device))
                    .map(|n| n.due_at)
                    .min()
            } else {
                None
            },
            waiting,
        })
    }

    /// Chats blocked on the user (an approval or an answer), by the chat
    /// the user sees (a subagent's parent).
    fn waiting_chats(&self) -> Vec<WaitingChat> {
        let waiting: Vec<(SessionInfo, bool)> = self
            .live_sessions()
            .into_iter()
            .filter_map(|rt| {
                let st = rt.st.lock();
                (st.info.status == SessionStatus::AwaitingApproval && !st.info.internal)
                    .then(|| (st.info.clone(), st.running))
            })
            .collect();
        let mut out: Vec<WaitingChat> = Vec::new();
        for (info, _) in waiting {
            let root = if info.kind == SessionKind::Subagent {
                self.root_session(&info.id)
            } else {
                info.id.clone()
            };
            if out.iter().any(|w| w.session_id == root) {
                continue;
            }
            let (title, private) = match self.rt(&root) {
                Ok(rt) => {
                    let st = rt.st.lock();
                    (st.info.title.clone(), st.info.private)
                }
                Err(_) => (info.title.clone(), info.private),
            };
            out.push(WaitingChat {
                session_id: root,
                title: if private {
                    String::new()
                } else {
                    plain_line(&title, 60)
                },
                private,
            });
        }
        out
    }

    /// Mark seen and let every device remove them.
    pub(super) fn notifications_seen(&self, ids: &[u64], session: Option<&str>) {
        let sessions: Vec<String> = session.map(|s| s.to_string()).into_iter().collect();
        if let Ok(seen) = self.store.mark_notifications_seen(ids, &sessions)
            && !seen.is_empty()
        {
            let _ = self.events.send(Event::NotificationsSeen { ids: seen });
        }
    }

    // ---- shared files -----------------------------------------------------

    fn shared_dir(&self) -> PathBuf {
        self.paths.data_dir.join("shared")
    }

    /// Copy `src` into Tessera's store as a download in `session_id`'s chat,
    /// refusing a file that holds a secret. The tool checked the path.
    pub(super) fn share_file(
        &self,
        session_id: &str,
        src: &Path,
        input: &std::fs::File,
        name: &str,
        held_ok: bool,
    ) -> Result<SharedFile, crate::tools::share::Refusal> {
        if self.store.is_private(session_id) {
            return Err("files can't be shared from a private chat".into());
        }
        let meta = input
            .metadata()
            .map_err(|e| format!("{}: {e}", src.display()))?;
        if !meta.is_file() {
            return Err(format!("{} isn't a file", src.display()).into());
        }
        if meta.len() > MAX_SHARE {
            return Err(format!(
                "{} is {}; files up to {} can be shared. Split or compress it first.",
                src.display(),
                crate::util::human_bytes(meta.len()),
                crate::util::human_bytes(MAX_SHARE)
            )
            .into());
        }
        let name = file_name(if name.trim().is_empty() {
            src.file_name().and_then(|n| n.to_str()).unwrap_or("file")
        } else {
            name
        });
        let id = new_id();
        let dir = self.shared_dir().join(&id);
        make_private_dir(&dir).map_err(|e| e.to_string())?;
        let dst = dir.join(&name);
        let mut patterns = self.auth.share_patterns();
        if let Ok(text) = std::fs::read_to_string(self.paths.data_dir.join("identity.key"))
            && let Some(secret) = text.lines().nth(1).map(str::trim).filter(|s| s.len() >= 16)
        {
            patterns.push(("the server's private key".into(), secret.to_string(), true));
        }
        let size =
            match crate::tools::share::copy_checked_file(input, src, &dst, &patterns, held_ok) {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(e);
                }
            };
        let file = SharedFile {
            id,
            mime: mime_of(&name).into(),
            name,
            size,
            created_at: now_ms(),
            // A subagent's file belongs with the chat the user sees.
            session_id: Some(self.root_session(session_id)),
            board_id: None,
            source: "agent".into(),
            path: dst.display().to_string(),
        };
        if let Err(e) = self.store.add_shared_file(&file, true) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.to_string().into());
        }
        self.prune_shared();
        let _ = self.events.send(Event::FileShared { file: file.clone() });
        Ok(file)
    }

    /// A file already on the server (an upload, a web view download) made
    /// downloadable as it is.
    pub(super) fn offer_file(
        &self,
        path: &Path,
        session_id: Option<&str>,
        board_id: Option<&str>,
        source: &str,
    ) -> Option<SharedFile> {
        let size = std::fs::metadata(path).ok().filter(|m| m.is_file())?.len();
        let name = file_name(path.file_name()?.to_str()?);
        let file = SharedFile {
            id: new_id(),
            mime: mime_of(&name).into(),
            name,
            size,
            created_at: now_ms(),
            session_id: session_id.map(str::to_string),
            board_id: board_id.map(str::to_string),
            source: source.into(),
            path: path.display().to_string(),
        };
        self.store.add_shared_file(&file, false).ok()?;
        let _ = self.events.send(Event::FileShared { file: file.clone() });
        Some(file)
    }

    fn read_shared_file(&self, id: &str, offset: u64, len: u32) -> Result<FileChunk, ApiError> {
        use base64::Engine as _;
        use std::io::{Read, Seek, SeekFrom};
        let gone = || ApiError::new(ErrorCode::NotFound, "this file is no longer on the server");
        let (file, owned) = self
            .store
            .shared_file(id)
            .map_err(internal)?
            .ok_or_else(gone)?;
        // Still a plain file where it was offered: never one a command has
        // since swapped for a link to something else.
        let home = if owned {
            self.shared_dir()
        } else if file.source == "upload" {
            self.paths.uploads_dir()
        } else {
            self.browser.server().downloads_dir().to_path_buf()
        };
        let path = Path::new(&file.path);
        let plain = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file());
        let inside = match (
            crate::platform::canonicalize(path),
            crate::platform::canonicalize(&home),
        ) {
            (Ok(p), Ok(h)) => p.starts_with(h),
            _ => false,
        };
        if !plain || !inside {
            return Err(gone());
        }
        let opened = crate::tools::file_object::Opened::open_in(path, &home).map_err(|_| gone())?;
        let mut f = &opened.file;
        let size = f.metadata().map_err(|_| gone())?.len();
        if offset > size {
            return Err(bad("offset past the end of the file"));
        }
        f.seek(SeekFrom::Start(offset)).map_err(internal)?;
        let want = len.clamp(1, MAX_CHUNK) as u64;
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

    /// Tessera's own copies stay under the quota; the oldest go first.
    fn prune_shared(&self) {
        let Ok(files) = self.store.shared_files(None, None) else {
            return;
        };
        let mut total = 0u64;
        for (f, owned) in files {
            if !owned {
                continue;
            }
            total += f.size;
            if total > SHARED_QUOTA {
                self.remove_shared(&f);
            }
        }
    }

    fn remove_shared(&self, f: &SharedFile) {
        let _ = self.store.delete_shared_file(&f.id);
        // Only ever inside our own folder.
        let dir = self.shared_dir().join(&f.id);
        if !f.id.is_empty() && f.id.chars().all(|c| c.is_ascii_alphanumeric()) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// A deleted chat: its files go (Tessera's copies), and its
    /// notifications.
    pub(super) fn forget_session_files(&self, session_id: &str) {
        if let Ok(files) = self
            .store
            .shared_files(Some(&[session_id.to_string()]), None)
        {
            for (f, owned) in files {
                if owned {
                    self.remove_shared(&f);
                } else {
                    let _ = self.store.delete_shared_file(&f.id);
                }
            }
        }
        if self.store.forget_notification_session(session_id).is_ok() {
            self.inbox_changed();
        }
    }

    /// The server's browser finished a download: offer it in the web view's
    /// tile and chat.
    pub(super) fn browser_downloaded(&self, d: &crate::browser::Downloaded) -> bool {
        let session = d.session.as_deref().map(|s| self.root_session(s));
        self.offer_file(&d.path, session.as_deref(), d.board.as_deref(), "web_view")
            .is_some()
    }
}

pub(super) fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = dir.parent() {
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn notification_policy_secret_request_is_generic_and_resolves_separately() {
        let (e, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        let entry = Entry {
            seq: 1,
            ts: now_ms(),
            body: EntryBody::ToolResult {
                call_id: "fixture-secret".into(),
                tool: "save_secret".into(),
                content: vec![],
                is_error: false,
                artifact: None,
                details: Some(
                    json!({"secret_entry": {"name": "FIXTURE_ONLY", "title": "Private service", "saved": false}}),
                ),
            },
        };
        e.0.secret_entry_needed(&id, &entry);
        let n = e
            .store()
            .unread_notification(&id, "secret")
            .unwrap()
            .unwrap();
        assert!(!n.quiet && e.0.notification_action_pending(&id));
        assert!(!format!("{} {}", n.title, n.body).contains("Private service"));
        e.0.activity_resolved(&id, "secret");
        assert!(!e.0.notification_action_pending(&id));
        // Asked again after the first was answered: a fresh, unread alert.
        e.0.secret_entry_needed(&id, &entry);
        let again = e
            .store()
            .unread_notification(&id, "secret")
            .unwrap()
            .expect("the second request is listed again");
        assert!(!again.quiet && !again.seen && again.id != n.id);
        assert!(e.0.notification_action_pending(&id));
        assert!(e.0.notification_delivery(&again, Some("phone")));
    }

    /// A runaway or misled agent can't ring every device all night: its
    /// pages are capped per source and hour, the rest kept in the digest.
    #[tokio::test]
    async fn an_agents_urgent_alerts_page_at_most_twelve_times_an_hour() {
        let (e, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        let mut levels = Vec::new();
        for i in 0..14 {
            let sent =
                e.0.tool_notify(
                    &id,
                    crate::tools::notify::Outgoing {
                        title: format!("Alert {i}"),
                        body: "Different text each time.".into(),
                        urgent: true,
                        at: None,
                    },
                )
                .unwrap();
            levels.push(sent.level);
        }
        assert!(
            levels[..MAX_PER_HOUR]
                .iter()
                .all(|l| *l == NotificationLevel::Page)
        );
        assert!(
            levels[MAX_PER_HOUR..]
                .iter()
                .all(|l| *l == NotificationLevel::Digest),
            "{levels:?}"
        );
        // Tessera's own approvals still page.
        let approval =
            e.0.send_notification(NewNotification {
                title: "Waiting for your approval".into(),
                kind: "approval".into(),
                session_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(approval.level, NotificationLevel::Page);
    }

    #[tokio::test]
    async fn one_test_alert_waits_at_a_time() {
        let (e, _dir, _rt) = super::super::test_support::engine().await;
        let first = e
            .handle(Request::TestNotification { delay_secs: 600 }, None)
            .await;
        assert!(first.is_ok(), "{first:?}");
        let second = e
            .handle(Request::TestNotification { delay_secs: 600 }, None)
            .await;
        assert!(second.is_err_and(|err| err.message.contains("already on its way")));
    }

    include!("notify_policy_tests.rs");

    #[test]
    fn text_is_plain_and_bounded() {
        assert_eq!(plain_line("  a\u{202e}b\tc\n d ", 80), "ab c d");
        assert_eq!(
            plain_text("one\n\n\n two \nthree\u{0007}", 80),
            "one\n\ntwo\nthree"
        );
        assert_eq!(plain_line(&"x".repeat(200), 80).chars().count(), 80);
        assert_eq!(file_name("../../etc/passwd"), "passwd");
        assert_eq!(file_name("a\\b\\..\\"), "file");
        assert_eq!(file_name("re\u{0}port.pdf"), "report.pdf");
        assert_eq!(mime_of("Report.PDF"), "application/pdf");
        assert_eq!(mime_of("noext"), "application/octet-stream");
    }
}
