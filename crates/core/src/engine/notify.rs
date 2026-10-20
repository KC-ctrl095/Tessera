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
/// Agents' notifications per chat per hour, and scheduled ones in all.
const MAX_PER_HOUR: usize = 12;
const MAX_SCHEDULED: usize = 100;
/// The notification center lists this many sent ones at most.
const INBOX_MAX: u32 = 500;

/// Ignore cosmetic case, whitespace and clock/date stamps, but preserve
/// measurements and counts. A changed number can be genuine news.
fn scheduled_text(title: &str, body: &str) -> String {
    format!("{title} {body}")
        .split_whitespace()
        .filter(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let clock = word.contains(':')
                && word
                    .chars()
                    .all(|c| c.is_ascii_digit() || ":.Zz+-Tt".contains(c));
            let date = word.len() >= 10
                && word.as_bytes().get(4) == Some(&b'-')
                && word.as_bytes().get(7) == Some(&b'-')
                && word
                    .chars()
                    .all(|c| c.is_ascii_digit() || "-:TtZz+.".contains(c));
            !clock && !date
        })
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
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
                for s in session_ids {
                    self.notifications_seen(&[], Some(&s));
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

    /// Presence is short-lived and scoped to this engine. Never let an
    /// abandoned tab silence the user's phone indefinitely.
    fn chat_in_view(&self, session: &str) -> bool {
        self.notification_presence
            .lock()
            .values()
            .any(|(at, chats)| at.elapsed().as_secs() < 45 && chats.iter().any(|s| s == session))
    }

    pub(super) fn notification_delivery(&self, n: &UserNotification, device: Option<&str>) -> bool {
        if !self.notification_visible(n, device) || n.quiet || n.seen {
            return false;
        }
        if !n.sent {
            return true;
        }
        if let Ok(Some(owner)) = self.store.notification_owner(n.id)
            && device != Some(owner.as_str())
        {
            return false;
        }
        let presence = self.notification_presence.lock();
        let active = presence
            .iter()
            .filter(|(_, (at, _))| at.elapsed().as_secs() < 45)
            .max_by_key(|(_, (at, _))| *at);
        if let Some((owner, (_, chats))) = active {
            // The foreground device owns in-app delivery. All other
            // devices, including Android background polls, stay quiet.
            return device == Some(owner.as_str())
                && n.session_id.as_ref().is_none_or(|s| !chats.contains(s));
        }
        true
    }

    fn bind_notification_owner(&self, n: &UserNotification) {
        if !n.sent || n.quiet {
            return;
        }
        let owner = self
            .notification_presence
            .lock()
            .iter()
            .filter(|(_, (at, _))| at.elapsed().as_secs() < 45)
            .max_by_key(|(_, (at, _))| *at)
            .map(|(device, _)| device.clone());
        if let Some(owner) = owner {
            let _ = self.store.set_notification_owner(n.id, &owner);
        }
    }

    pub(super) fn job_notification_source(&self, session: &str) -> String {
        self.store
            .job_run(session)
            .ok()
            .flatten()
            .map(|r| format!("job_{}", r.job_id))
            .unwrap_or_default()
    }

    /// Repeated scheduled checks are not news. The durable timestamp is
    /// independent of inbox reads/replacement and survives server restarts.
    fn scheduled_policy(&self, n: &mut NewNotification, except: u64) -> Result<(), String> {
        if n.source_key.is_empty() || n.quiet || n.due_at > now_ms() {
            return Ok(());
        }
        let now = now_ms();
        let previous = self
            .store
            .notification_source(&n.source_key, except)
            .map_err(|e| e.to_string())?;
        use sha2::Digest;
        let fingerprint = format!(
            "{:x}",
            sha2::Sha256::digest(scheduled_text(&n.title, &n.body).as_bytes())
        );
        let fingerprint_key = format!("notify_text:{}", n.source_key);
        let previous_fingerprint = self
            .store
            .kv_get(&fingerprint_key)
            .map_err(|e| e.to_string())?;
        let repeated = previous_fingerprint.as_deref() == Some(&fingerprint)
            || previous.as_ref().is_some_and(|p| {
                scheduled_text(&p.title, &p.body) == scheduled_text(&n.title, &n.body)
            });
        let key = format!("notify_last:{}", n.source_key);
        let last = self
            .store
            .kv_get(&key)
            .map_err(|e| e.to_string())?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        n.quiet = (repeated && (!n.urgent || now < last.saturating_add(900_000)))
            || (!n.urgent && now < last.saturating_add(3_600_000));
        self.store
            .kv_set(&fingerprint_key, &fingerprint)
            .map_err(|e| e.to_string())?;
        if !n.quiet {
            self.store
                .kv_set(&key, &now.to_string())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
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
        self.scheduled_policy(&mut n, 0)?;
        // A reply already visible on any device must never replay as an
        // unseen completion on a phone later.
        if n.due_at <= now_ms()
            && n.session_id
                .as_deref()
                .is_some_and(|s| self.chat_in_view(s))
        {
            n.quiet = true;
        }
        let Some((stored, recorded)) = self
            .store
            .record_notification_once(&n, window)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        if !matches!(recorded, crate::store::Recorded::Same) {
            self.bind_notification_owner(&stored);
        }
        let stored = self.notification_payload(stored);
        if recorded == crate::store::Recorded::Same {
            return Ok(Some(stored));
        }
        if let crate::store::Recorded::Changed { previous } = recorded {
            let _ = self.events.send(Event::NotificationsSeen {
                ids: vec![previous],
            });
        }
        // A quiet one is never a system notification: devices only learn
        // that their notification center changed (below).
        if stored.sent
            && (!stored.quiet || !stored.source_key.is_empty())
            && let Ok(ids) = self.store.supersede_notifications(&stored)
            && !ids.is_empty()
        {
            let _ = self.events.send(Event::NotificationsSeen { ids });
        }
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

    /// `notify` from an agent: rate-limited, from the chat the user sees.
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
        {
            let mut recent = recent_sends().lock();
            recent.retain(|_, v| {
                v.retain(|(t, _)| now < t + 3_600_000);
                !v.is_empty()
            });
            let mine = recent.entry(session.clone()).or_default();
            if n.at.is_none() && mine.iter().any(|(t, k)| *k == key && now < t + 600_000) {
                return Err("That notification was already sent a moment ago.".into());
            }
            if mine.len() >= MAX_PER_HOUR {
                return Err(format!(
                    "Already {MAX_PER_HOUR} notifications from this chat in the last hour; that's enough. Put the rest in the chat."
                ));
            }
            mine.push((now, key));
        }
        let sent = self.send_notification(NewNotification {
            title: n.title,
            body: n.body,
            from: self.notification_sender(&session),
            session_id: Some(session.clone()),
            urgent: n.urgent,
            due_at: n.at.unwrap_or(now),
            source_key: self.job_notification_source(&session),
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
                    source_key: notification.source_key.clone(),
                    urgent: notification.urgent,
                    quiet: notification.quiet,
                    due_at: now,
                    ..Default::default()
                };
                if let Err(error) = self.scheduled_policy(&mut policy, notification.id) {
                    tracing::warn!(%error, "due notification policy failed");
                    continue;
                }
                notification.quiet = policy.quiet
                    || notification
                        .session_id
                        .as_deref()
                        .is_some_and(|s| self.chat_in_view(s));
                if notification.quiet {
                    let _ = self.store.quiet_notification(notification.id);
                }
                if let Ok(ids) = self.store.supersede_notifications(&notification)
                    && !ids.is_empty()
                {
                    let _ = self.events.send(Event::NotificationsSeen { ids });
                }
                self.bind_notification_owner(&notification);
                let notification = self.notification_payload(notification);
                if !notification.quiet {
                    let _ = self.events.send(Event::Notification { notification });
                }
            }
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
                if on {
                    self.store.unshown_notifications(d, now).map_err(internal)?
                } else {
                    Vec::new()
                },
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
        waiting.retain(|w| mine(Some(&w.session_id)) && !self.chat_in_view(&w.session_id));
        if self
            .notification_presence
            .lock()
            .values()
            .any(|(at, _)| at.elapsed().as_secs() < 45)
        {
            waiting.clear();
        }
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
    }

    #[tokio::test]
    async fn notification_policy_presence_owns_delivery_and_expires() {
        let (e, _dir, rt) = super::super::test_support::engine().await;
        let session = rt.st.lock().info.id.clone();
        let n =
            e.0.send_notification(NewNotification {
                title: "Asked for this".into(),
                session_id: Some(session.clone()),
                ..Default::default()
            })
            .unwrap();
        e.0.notification_presence
            .lock()
            .insert("laptop".into(), (std::time::Instant::now(), vec![]));
        assert!(e.0.notification_delivery(&n, Some("laptop")));
        assert!(!e.0.notification_delivery(&n, Some("phone")));
        e.0.notification_presence.lock().insert(
            "laptop".into(),
            (std::time::Instant::now(), vec![session.clone()]),
        );
        assert!(!e.0.notification_delivery(&n, Some("laptop")));
        let viewed =
            e.0.send_notification(NewNotification {
                title: "Reply".into(),
                session_id: Some(session.clone()),
                ..Default::default()
            })
            .unwrap();
        assert!(viewed.quiet, "a viewed reply must not replay later");
        let future =
            e.0.send_notification(NewNotification {
                title: "Tomorrow's reminder".into(),
                session_id: Some(session.clone()),
                due_at: now_ms() + 60_000,
                ..Default::default()
            })
            .unwrap();
        assert!(
            !future.sent && !future.quiet,
            "looking now must not cancel a future reminder"
        );
        e.0.notification_presence.lock().insert(
            "laptop".into(),
            (
                std::time::Instant::now() - Duration::from_secs(46),
                vec![session],
            ),
        );
        assert!(
            e.0.notification_delivery(&n, Some("phone")),
            "abandoned foreground leases expire"
        );
        e.0.notification_presence
            .lock()
            .insert("laptop".into(), (std::time::Instant::now(), vec![]));
        let owned =
            e.0.send_notification(NewNotification {
                title: "Explicit news elsewhere".into(),
                ..Default::default()
            })
            .unwrap();
        e.0.notification_presence.lock().clear();
        assert!(e.0.notification_delivery(&owned, Some("laptop")));
        assert!(
            !e.0.notification_delivery(&owned, Some("phone")),
            "foreground event ownership prevents a delayed duplicate after lease expiry"
        );
    }

    #[tokio::test]
    async fn notification_policy_one_live_chat_revision_and_silent_helpers() {
        let (e, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        let a =
            e.0.send_notification(NewNotification {
                title: "First".into(),
                session_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        let b =
            e.0.send_notification(NewNotification {
                title: "Second".into(),
                session_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert!(e.store().notification(a.id).unwrap().unwrap().seen);
        assert!(!e.store().notification(b.id).unwrap().unwrap().seen);
        e.0.keep_activity(
            &id,
            "helper",
            "Helper finished",
            "Report".into(),
            crate::store::Replace::Unread,
        );
        let helper = e
            .store()
            .unread_notification(&id, "helper")
            .unwrap()
            .unwrap();
        assert!(helper.quiet);
        assert!(!e.0.notification_delivery(&helper, Some("phone")));
    }

    fn job_alert(body: &str, urgent: bool) -> NewNotification {
        NewNotification {
            title: "Monitor".into(),
            body: body.into(),
            source_key: "job_test".into(),
            urgent,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn notification_policy_scheduled_burst_repeat_and_urgent_failure() {
        let (e, _dir, _rt) = super::super::test_support::engine().await;
        assert!(
            !e.0.send_notification(job_alert("Healthy at 18:15", false))
                .unwrap()
                .quiet
        );
        for at in [
            "18:30", "18:40", "18:50", "18:55", "19:00", "19:05", "19:15", "19:25",
        ] {
            assert!(
                e.0.send_notification(job_alert(&format!("HEALTHY  at  {at}"), false))
                    .unwrap()
                    .quiet
            );
        }
        assert_eq!(
            e.store().notifications(500).unwrap().len(),
            1,
            "one item across run chats"
        );
        assert!(
            e.0.send_notification(job_alert("Queue has 3 items", false))
                .unwrap()
                .quiet,
            "changed nonurgent burst waits"
        );
        assert!(
            !e.0.send_notification(job_alert("Failure needs your attention", true))
                .unwrap()
                .quiet,
            "new urgent failure bypasses hourly throttle"
        );
        assert!(
            e.0.send_notification(job_alert("Failure needs your attention", true))
                .unwrap()
                .quiet
        );
        assert!(
            !e.0.send_notification(job_alert("A different failure needs your attention", true))
                .unwrap()
                .quiet
        );
        e.store()
            .kv_set(
                "notify_last:job_test",
                &now_ms().saturating_sub(901_000).to_string(),
            )
            .unwrap();
        assert!(
            !e.0.send_notification(job_alert("A different failure needs your attention", true))
                .unwrap()
                .quiet,
            "unresolved urgent failure can remind after fifteen minutes"
        );
    }

    #[tokio::test]
    async fn notification_policy_repeat_memory_survives_inbox_clear_and_reopen() {
        let (e, dir, rt) = super::super::test_support::engine().await;
        let first =
            e.0.send_notification(job_alert("Nothing changed", false))
                .unwrap();
        e.store().clear_notifications(&[first.id]).unwrap();
        e.store()
            .kv_set(
                "notify_last:job_test",
                &now_ms().saturating_sub(7_200_000).to_string(),
            )
            .unwrap();
        drop(rt);
        drop(e);
        let e = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        assert!(
            e.0.send_notification(job_alert("Nothing changed", false))
                .unwrap()
                .quiet,
            "read/clear do not reset duplicate state"
        );
        assert!(
            !e.0.send_notification(job_alert("There are 4 new results", false))
                .unwrap()
                .quiet
        );
        assert_ne!(
            scheduled_text("Monitor", "4 new results"),
            scheduled_text("Monitor", "5 new results")
        );
    }

    #[tokio::test]
    async fn notification_policy_scenario_counts() {
        let (e, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        let mut alerts = 0;
        for _ in 0..5 {
            e.0.keep_activity(
                &id,
                "helper",
                "A helper reported",
                "Report".into(),
                crate::store::Replace::Unread,
            );
        }
        for _ in 0..3 {
            let n =
                e.0.send_notification(NewNotification {
                    title: "Needs your approval".into(),
                    kind: "approval".into(),
                    session_id: Some(id.clone()),
                    ..Default::default()
                })
                .unwrap();
            alerts += usize::from(!n.quiet);
        }
        let reply =
            e.0.send_notification(NewNotification {
                title: "Finished".into(),
                kind: "finished".into(),
                session_id: Some(id),
                ..Default::default()
            })
            .unwrap();
        alerts += usize::from(!reply.quiet);
        for _ in 0..8 {
            alerts += usize::from(
                !e.0.send_notification(job_alert("Healthy, nothing changed", false))
                    .unwrap()
                    .quiet,
            );
        }
        assert_eq!(
            (5 + 3 + 1 + 8, alerts),
            (17, 5),
            "two-hour scenario, device away"
        );
        let mut daily_alerts = 0;
        for _ in 0..53 {
            let mut n = job_alert("The check is unchanged", false);
            n.source_key = "job_day".into();
            daily_alerts += usize::from(!e.0.send_notification(n).unwrap().quiet);
        }
        daily_alerts += usize::from(
            !e.0.send_notification(NewNotification {
                title: "The one explicitly requested interactive alert".into(),
                ..Default::default()
            })
            .unwrap()
            .quiet,
        );
        assert_eq!(
            (53 + 1, daily_alerts),
            (54, 2),
            "conditional estimate if the 53 observed job messages all repeat one source"
        );
    }

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
