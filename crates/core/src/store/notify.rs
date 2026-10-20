//! Notifications for the user's devices (`notify`) and the files they can
//! download from a chat (`share_file`, web view downloads, uploads).
//!
//! A notification is sent once it is due; each device then shows it once
//! (`notification_shown`), and seeing it anywhere marks it seen for all.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tessera_proto::{SharedFile, UserNotification};

use super::Store;
use crate::util::now_ms;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS notifications (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at  INTEGER NOT NULL,
    due_at      INTEGER NOT NULL,
    sent        INTEGER NOT NULL DEFAULT 0,
    seen        INTEGER NOT NULL DEFAULT 0,
    session_id  TEXT,
    data        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS notifications_due ON notifications(sent, due_at);

-- Independent of notification retention/device acknowledgement. A reset is a
-- distinct window; deleting/seeing a notification must not re-alert that window.
CREATE TABLE IF NOT EXISTS notification_windows (
    identity TEXT PRIMARY KEY,
    reset_ms INTEGER NOT NULL
);

-- Which device already showed which notification.
CREATE TABLE IF NOT EXISTS notification_shown (
    notification_id INTEGER NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
    device_id       TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    PRIMARY KEY (notification_id, device_id)
);

-- Per-device choices; no row means the defaults.
CREATE TABLE IF NOT EXISTS device_prefs (
    device_id   TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    notify      INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS shared_files (
    id          TEXT PRIMARY KEY,
    session_id  TEXT,
    board_id    TEXT,
    created_at  INTEGER NOT NULL,
    data        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS shared_files_session ON shared_files(session_id);
CREATE INDEX IF NOT EXISTS shared_files_board ON shared_files(board_id);
"#;

/// Sent notifications kept (newest), and for how long.
const KEEP_SENT: i64 = 500;
const KEEP_SENT_MS: u64 = 30 * 86_400_000;
/// A device that was away longer than this doesn't get older ones.
pub const PENDING_WINDOW_MS: u64 = 86_400_000;

/// `sent`: waiting for `due_at`, delivered (a system notification on the
/// devices that take them), or kept for the notification center only.
/// Older servers only ever select 0 and 1, so a quiet row never becomes an
/// alert after a downgrade.
const SCHEDULED: i64 = 0;
const ALERT: i64 = 1;
const QUIET: i64 = 2;

/// Quiet rows about what a chat is doing (an approval, a helper's report, a
/// turn that ended) that were read within this long were never news: they
/// go instead of staying as read history.
const NOT_NEWS_MS: u64 = 60_000;
const ACTIVITY: &[&str] = &["approval", "helper", "finished", "error"];

/// Private chats' notifications: the same tables, in memory only (nothing
/// about a private chat reaches the database, its journal or free pages).
/// Their ids are reserved in the database so they never collide.
pub(super) const PRIVATE_SCHEMA: &str = r#"
CREATE TABLE notifications (
    id          INTEGER PRIMARY KEY,
    created_at  INTEGER NOT NULL,
    due_at      INTEGER NOT NULL,
    sent        INTEGER NOT NULL DEFAULT 0,
    seen        INTEGER NOT NULL DEFAULT 0,
    session_id  TEXT,
    data        TEXT NOT NULL
);
CREATE TABLE notification_shown (
    notification_id INTEGER NOT NULL,
    device_id       TEXT NOT NULL,
    PRIMARY KEY (notification_id, device_id)
);
CREATE TABLE notification_windows (
    identity TEXT PRIMARY KEY,
    reset_ms INTEGER NOT NULL
);
"#;

pub(super) fn private_notes() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(PRIVATE_SCHEMA)?;
    Ok(conn)
}

/// Which earlier row a new one takes the place of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Replace {
    /// None: always a new one.
    #[default]
    New,
    /// The chat's unread one of the same kind (one "needs your approval"
    /// per chat until it is read).
    Unread,
    /// The chat's one of the same kind, read or not (one per scheduled run:
    /// a continued run updates its item).
    Same,
}

/// A notification to store (and send when `due_at` comes).
#[derive(Debug, Clone, Default)]
pub struct NewNotification {
    pub title: String,
    pub body: String,
    pub session_id: Option<String>,
    pub from: String,
    pub urgent: bool,
    /// Unix ms; now or earlier = send now.
    pub due_at: u64,
    /// See `UserNotification::kind`.
    pub kind: String,
    /// For the notification center only, never a system notification.
    pub quiet: bool,
    pub replace: Replace,
    /// Stable job identity, independent of its per-run chat.
    pub source_key: String,
}

/// What [`Store::record_notification`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    New,
    /// An earlier one now says something else (unread again).
    Changed {
        previous: u64,
    },
    /// It already said exactly this: nothing changed.
    Same,
}

#[derive(Serialize, Deserialize, Default, PartialEq)]
struct NoteData {
    title: String,
    body: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    urgent: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    kind: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    private: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    source_key: String,
}

#[derive(Serialize, Deserialize)]
struct FileData {
    #[serde(flatten)]
    file: SharedFile,
    /// The file is Tessera's copy: delete it with the record.
    owned: bool,
}

fn note_row(r: &rusqlite::Row) -> rusqlite::Result<UserNotification> {
    let data: String = r.get(6)?;
    let d: NoteData = serde_json::from_str(&data).unwrap_or_default();
    let sent: i64 = r.get(3)?;
    Ok(UserNotification {
        id: r.get::<_, i64>(0)? as u64,
        created_at: r.get::<_, i64>(1)? as u64,
        due_at: r.get::<_, i64>(2)? as u64,
        sent: sent != SCHEDULED,
        seen: r.get::<_, i64>(4)? != 0,
        session_id: r.get(5)?,
        title: d.title,
        body: d.body,
        from: d.from,
        urgent: d.urgent,
        kind: d.kind,
        private: d.private,
        source_key: d.source_key,
        quiet: sent == QUIET,
    })
}

const NOTE_COLS: &str = "id, created_at, due_at, sent, seen, session_id, data";

fn row_by_id(conn: &Connection, id: u64) -> Result<Option<UserNotification>> {
    Ok(conn
        .query_row(
            &format!("SELECT {NOTE_COLS} FROM notifications WHERE id = ?1"),
            [id as i64],
            note_row,
        )
        .optional()?)
}

fn query(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<UserNotification>> {
    Ok(conn
        .prepare(&format!("SELECT {NOTE_COLS} FROM notifications {sql}"))?
        .query_map(p, note_row)?
        .collect::<Result<_, _>>()?)
}

fn prune(conn: &Connection, now: u64) -> Result<()> {
    conn.execute(
        "DELETE FROM notifications WHERE sent != 0 AND (due_at < ?1 OR id NOT IN
           (SELECT id FROM notifications WHERE sent != 0 ORDER BY due_at DESC, id DESC LIMIT ?2))",
        params![now.saturating_sub(KEEP_SENT_MS) as i64, KEEP_SENT],
    )?;
    // The private tables have no foreign keys to clean up after them.
    conn.execute(
        "DELETE FROM notification_shown
         WHERE notification_id NOT IN (SELECT id FROM notifications)",
        [],
    )?;
    Ok(())
}

impl Store {
    pub fn add_notification(&self, n: &NewNotification) -> Result<UserNotification> {
        Ok(self.record_notification(n)?.0)
    }

    /// Identity is a hash of chat/account/limit type/reset, not credential data.
    /// The window and notification commit together; private windows stay in memory.
    #[cfg(test)]
    pub(crate) fn add_notification_once(
        &self,
        n: &NewNotification,
        window: Option<(&str, u64)>,
    ) -> Result<Option<UserNotification>> {
        store_call!(self.add_notification_once(n, window));
        Ok(self
            .record_notification_once(n, window)?
            .map(|(note, _)| note))
    }

    /// Store or replace a notification, making an updated one unread again.
    pub fn record_notification(&self, n: &NewNotification) -> Result<(UserNotification, Recorded)> {
        store_call!(self.record_notification(n));
        Ok(self
            .record_notification_once(n, None)?
            .expect("no dedup window"))
    }

    pub(crate) fn record_notification_once(
        &self,
        n: &NewNotification,
        window: Option<(&str, u64)>,
    ) -> Result<Option<(UserNotification, Recorded)>> {
        store_call!(self.record_notification_once(n, window));
        let now = now_ms();
        let due = n.due_at.max(now);
        let state = if n.quiet {
            QUIET
        } else if due <= now {
            ALERT
        } else {
            SCHEDULED
        };
        let private = n.session_id.as_deref().is_some_and(|s| self.is_private(s));
        let data = NoteData {
            title: n.title.clone(),
            body: n.body.clone(),
            from: n.from.clone(),
            urgent: n.urgent,
            kind: n.kind.clone(),
            private,
            source_key: n.source_key.clone(),
        };
        let json = serde_json::to_string(&data)?;
        // A private chat's id is reserved here without anything about it:
        // the row goes in the same transaction.
        let reserved = if private {
            let conn = self.conn.lock();
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO notifications(created_at, due_at, sent, data) VALUES (0, 0, 1, '{}')",
                [],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute("DELETE FROM notifications WHERE id = ?1", [id])?;
            tx.commit()?;
            Some(id)
        } else {
            None
        };
        let (id, recorded) = {
            let conn = if private {
                self.private_notes.lock()
            } else {
                self.conn.lock()
            };
            let tx = conn.unchecked_transaction()?;
            if let Some((identity, _)) = window
                && tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM notification_windows WHERE identity = ?1)",
                    [identity],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Ok(None);
            }
            let earlier = if !n.source_key.is_empty() && due <= now {
                tx.query_row(
                    "SELECT id, data, sent FROM notifications WHERE sent != 0
                       AND coalesce(json_extract(data, '$.source_key'), '') = ?1
                     ORDER BY id DESC LIMIT 1",
                    [&n.source_key],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?
            } else {
                match (&n.session_id, n.replace) {
                    (Some(s), Replace::Unread | Replace::Same) => tx
                        .query_row(
                            &format!(
                                "SELECT id, data, sent FROM notifications
                          WHERE session_id = ?1 AND sent != 0
                            AND coalesce(json_extract(data, '$.kind'), '') = ?2 {}
                          ORDER BY id DESC LIMIT 1",
                                if n.replace == Replace::Unread {
                                    "AND seen = 0"
                                } else {
                                    ""
                                }
                            ),
                            params![s, n.kind],
                            |r| {
                                Ok((
                                    r.get::<_, i64>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, i64>(2)?,
                                ))
                            },
                        )
                        .optional()?,
                    _ => None,
                }
            };
            let out = match earlier {
                Some((id, old, sent))
                    if n.source_key.is_empty()
                        && sent == state
                        && serde_json::from_str::<NoteData>(&old).is_ok_and(|o| o == data) =>
                {
                    (id, Recorded::Same)
                }
                Some((id, _, _)) => {
                    // The id is the revision. An old read/clear/ack must never
                    // consume a replacement, including from an old decoder.
                    tx.execute("DELETE FROM notifications WHERE id = ?1", [id])?;
                    tx.execute(
                        "DELETE FROM notification_shown WHERE notification_id = ?1",
                        [id],
                    )?;
                    tx.execute(
                        "INSERT INTO notifications(id, created_at, due_at, sent, session_id, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![reserved, now as i64, due as i64, state, n.session_id, json],
                    )?;
                    (
                        tx.last_insert_rowid(),
                        Recorded::Changed {
                            previous: id as u64,
                        },
                    )
                }
                None => {
                    tx.execute(
                        "INSERT INTO notifications(id, created_at, due_at, sent, session_id, data)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![reserved, now as i64, due as i64, state, n.session_id, json],
                    )?;
                    (tx.last_insert_rowid(), Recorded::New)
                }
            };
            if let Some((identity, reset_ms)) = window {
                tx.execute(
                    "INSERT INTO notification_windows(identity, reset_ms) VALUES (?1, ?2)",
                    params![identity, reset_ms as i64],
                )?;
                tx.execute(
                    "DELETE FROM notification_windows WHERE reset_ms < ?1",
                    [now.saturating_sub(KEEP_SENT_MS) as i64],
                )?;
            }
            tx.commit()?;
            out
        };
        self.prune_notifications(now)?;
        let stored = self
            .notification(id as u64)?
            .ok_or_else(|| anyhow::anyhow!("notification {id} went at once"))?;
        Ok(Some((stored, recorded)))
    }

    /// Latest per-job item and the last audible send survive quiet updates
    /// and restarts. Keys contain only job ids, never message content.
    pub fn notification_source(
        &self,
        source: &str,
        except: u64,
    ) -> Result<Option<UserNotification>> {
        store_call!(self.notification_source(source, except));
        Ok(query(&self.conn.lock(),
            "WHERE sent != 0 AND id != ?2 AND coalesce(json_extract(data, '$.source_key'), '') = ?1 ORDER BY id DESC LIMIT 1",
            params![source, except as i64])?.pop())
    }

    pub fn quiet_notification(&self, id: u64) -> Result<()> {
        store_call!(self.quiet_notification(id));
        self.on_both(|conn| {
            conn.execute(
                "UPDATE notifications SET sent = 2 WHERE id = ?1",
                [id as i64],
            )?;
            Ok(Vec::<()>::new())
        })?;
        Ok(())
    }

    /// Bind an event created while an app is in front to that device. The
    /// binding is kept with the bounded row, privately in memory if needed.
    pub fn set_notification_owner(&self, id: u64, device: &str) -> Result<()> {
        store_call!(self.set_notification_owner(id, device));
        self.on_both(|conn| {
            conn.execute("UPDATE notifications SET data = json_set(data, '$.delivery_device', ?2) WHERE id = ?1", params![id as i64, device])?;
            Ok(Vec::<()>::new())
        })?;
        Ok(())
    }

    pub fn notification_owner(&self, id: u64) -> Result<Option<String>> {
        store_call!(self.notification_owner(id));
        for conn in [&self.conn, &self.private_notes] {
            let owner = conn.lock().query_row("SELECT json_extract(data, '$.delivery_device') FROM notifications WHERE id = ?1", [id as i64], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
            if owner.is_some() {
                return Ok(owner);
            }
        }
        Ok(None)
    }

    /// Withdraw earlier live alerts, keeping their read history. Quiet
    /// updates must not leave an old phone popup alive.
    pub fn supersede_notifications(&self, n: &UserNotification) -> Result<Vec<u64>> {
        store_call!(self.supersede_notifications(n));
        let prior = self
            .notifications(500)?
            .into_iter()
            .filter(|old| {
                old.id != n.id
                    && old.sent
                    && !old.seen
                    && !old.quiet
                    && if n.source_key.is_empty() {
                        n.session_id.is_some() && old.session_id == n.session_id
                    } else {
                        old.source_key == n.source_key
                    }
            })
            .map(|old| old.id)
            .collect::<Vec<_>>();
        self.mark_notifications_seen(&prior, &[])
    }
    /// The chat's newest unread one of `kind`, if any.
    pub fn unread_notification(
        &self,
        session: &str,
        kind: &str,
    ) -> Result<Option<UserNotification>> {
        store_call!(self.unread_notification(session, kind));
        let conn = if self.is_private(session) {
            self.private_notes.lock()
        } else {
            self.conn.lock()
        };
        Ok(query(
            &conn,
            "WHERE session_id = ?1 AND sent != 0 AND seen = 0
               AND coalesce(json_extract(data, '$.kind'), '') = ?2 ORDER BY id DESC LIMIT 1",
            params![session, kind],
        )?
        .pop())
    }

    /// Enforce age and count on reads and delivery too, even on a quiet server.
    pub fn prune_notifications(&self, now: u64) -> Result<()> {
        store_call!(self.prune_notifications(now));
        prune(&self.conn.lock(), now)?;
        prune(&self.private_notes.lock(), now)
    }

    pub fn notification(&self, id: u64) -> Result<Option<UserNotification>> {
        store_call!(self.notification(id));
        if let Some(n) = row_by_id(&self.conn.lock(), id)? {
            return Ok(Some(n));
        }
        row_by_id(&self.private_notes.lock(), id)
    }

    /// Run `f` on the database, then on the private chats' tables.
    fn on_both<T>(&self, f: impl Fn(&Connection) -> Result<Vec<T>>) -> Result<Vec<T>> {
        let mut out = f(&self.conn.lock())?;
        out.extend(f(&self.private_notes.lock())?);
        Ok(out)
    }

    /// Scheduled ones (soonest first), then the latest `limit` sent or kept
    /// (newest first; an updated one counts from its update).
    pub fn notifications(&self, limit: u32) -> Result<Vec<UserNotification>> {
        store_call!(self.notifications(limit));
        self.prune_notifications(now_ms())?;
        let mut scheduled = self.on_both(|c| query(c, "WHERE sent = 0", []))?;
        scheduled.sort_by_key(|n| (n.due_at, n.id));
        let mut sent = self.on_both(|c| {
            query(
                c,
                "WHERE sent != 0 ORDER BY due_at DESC, id DESC LIMIT ?1",
                [limit as i64],
            )
        })?;
        sent.sort_by(|a, b| b.due_at.cmp(&a.due_at).then(b.id.cmp(&a.id)));
        sent.truncate(limit as usize);
        scheduled.extend(sent);
        Ok(scheduled)
    }

    /// Scheduled notifications whose time has come, now marked sent.
    pub fn take_due_notifications(&self, now: u64) -> Result<Vec<UserNotification>> {
        store_call!(self.take_due_notifications(now));
        self.prune_notifications(now)?;
        let mut due = self.on_both(|conn| {
            let tx = conn.unchecked_transaction()?;
            let due = query(
                &tx,
                "WHERE sent = 0 AND due_at <= ?1 ORDER BY due_at, id",
                [now as i64],
            )?;
            tx.execute(
                "UPDATE notifications SET sent = 1 WHERE sent = 0 AND due_at <= ?1",
                [now as i64],
            )?;
            tx.commit()?;
            Ok(due)
        })?;
        due.sort_by_key(|n| (n.due_at, n.id));
        Ok(due
            .into_iter()
            .map(|mut n| {
                n.sent = true;
                n
            })
            .collect())
    }

    /// When the next scheduled notification is due.
    pub fn next_notification_due(&self) -> Result<Option<u64>> {
        store_call!(self.next_notification_due());
        Ok(self
            .on_both(|c| {
                Ok(c.query_row(
                    "SELECT MIN(due_at) FROM notifications WHERE sent = 0",
                    [],
                    |r| r.get::<_, Option<i64>>(0),
                )?
                .map(|v| v as u64)
                .into_iter()
                .collect())
            })?
            .into_iter()
            .min())
    }

    /// Delivered in the last day, not seen anywhere, not yet shown on
    /// `device` (quiet ones never are: they aren't system notifications).
    pub fn unshown_notifications(&self, device: &str, now: u64) -> Result<Vec<UserNotification>> {
        store_call!(self.unshown_notifications(device, now));
        let mut rows = self.on_both(|c| {
            query(
                c,
                "n WHERE sent = 1 AND seen = 0 AND due_at >= ?2
                   AND NOT EXISTS (SELECT 1 FROM notification_shown s
                                   WHERE s.notification_id = n.id AND s.device_id = ?1)
                 ORDER BY id",
                params![device, now.saturating_sub(PENDING_WINDOW_MS) as i64],
            )
        })?;
        rows.sort_by_key(|n| n.id);
        Ok(rows)
    }

    /// Shown on `device` in the last day and since seen somewhere.
    pub fn seen_since_shown(&self, device: &str, now: u64) -> Result<Vec<u64>> {
        store_call!(self.seen_since_shown(device, now));
        let mut rows = self.on_both(|conn| {
            Ok(conn
                .prepare(
                    "SELECT n.id FROM notifications n JOIN notification_shown s ON s.notification_id = n.id
                     WHERE s.device_id = ?1 AND n.seen = 1 AND n.due_at >= ?2 ORDER BY n.id",
                )?
                .query_map(
                    params![device, now.saturating_sub(PENDING_WINDOW_MS) as i64],
                    |r| r.get::<_, i64>(0),
                )?
                .map(|r| r.map(|v| v as u64))
                .collect::<Result<Vec<_>, _>>()?)
        })?;
        rows.sort_unstable();
        Ok(rows)
    }

    /// `device` showed these (unknown ids are ignored).
    pub fn mark_notifications_shown(&self, device: &str, ids: &[u64]) -> Result<()> {
        store_call!(self.mark_notifications_shown(device, ids));
        let known: bool = self
            .conn
            .lock()
            .query_row("SELECT 1 FROM devices WHERE id = ?1", [device], |_| Ok(()))
            .optional()?
            .is_some();
        if !known {
            return Ok(());
        }
        self.on_both(|conn| {
            let tx = conn.unchecked_transaction()?;
            for id in ids {
                tx.execute(
                    "INSERT OR IGNORE INTO notification_shown(notification_id, device_id)
                     SELECT id, ?2 FROM notifications WHERE id = ?1",
                    params![*id as i64, device],
                )?;
            }
            tx.commit()?;
            Ok(Vec::<()>::new())
        })?;
        Ok(())
    }

    /// Mark seen: `ids`, and every delivered one about `session` (and the
    /// chats below it, given as `sessions`). Returns those that weren't yet
    /// (a quiet one about a chat's activity read within a minute goes
    /// instead: it was never news).
    pub fn mark_notifications_seen(&self, ids: &[u64], sessions: &[String]) -> Result<Vec<u64>> {
        store_call!(self.mark_notifications_seen(ids, sessions));
        let now = now_ms();
        let mut out = self.on_both(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut found: Vec<UserNotification> = Vec::new();
            for id in ids {
                found.extend(query(
                    &tx,
                    "WHERE id = ?1 AND sent != 0 AND seen = 0",
                    [*id as i64],
                )?);
            }
            for s in sessions {
                found.extend(query(
                    &tx,
                    "WHERE session_id = ?1 AND sent != 0 AND seen = 0",
                    [s],
                )?);
            }
            found.sort_by_key(|n| n.id);
            found.dedup_by_key(|n| n.id);
            for n in &found {
                if n.quiet
                    && ACTIVITY.contains(&n.kind.as_str())
                    && now < n.due_at.saturating_add(NOT_NEWS_MS)
                {
                    tx.execute("DELETE FROM notifications WHERE id = ?1", [n.id as i64])?;
                } else {
                    tx.execute(
                        "UPDATE notifications SET seen = 1 WHERE id = ?1",
                        [n.id as i64],
                    )?;
                }
            }
            tx.commit()?;
            Ok(found.into_iter().map(|n| n.id).collect())
        })?;
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    pub fn delete_notification(&self, id: u64) -> Result<bool> {
        store_call!(self.delete_notification(id));
        let gone = self.on_both(|c| {
            Ok(vec![c.execute(
                "DELETE FROM notifications WHERE id = ?1",
                [id as i64],
            )?])
        })?;
        Ok(gone.iter().sum::<usize>() > 0)
    }

    /// Forget these sent ones (scheduled ones stay). Returns those that went.
    pub fn clear_notifications(&self, ids: &[u64]) -> Result<Vec<u64>> {
        store_call!(self.clear_notifications(ids));
        let mut gone = self.on_both(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut out = Vec::new();
            for id in ids {
                if tx.execute(
                    "DELETE FROM notifications WHERE id = ?1 AND sent != 0",
                    [*id as i64],
                )? > 0
                {
                    out.push(*id);
                }
            }
            tx.commit()?;
            Ok(out)
        })?;
        prune(&self.private_notes.lock(), now_ms())?;
        gone.sort_unstable();
        gone.dedup();
        Ok(gone)
    }

    /// A deleted (or signed-out private) chat: its notifications go too,
    /// rather than staying as messages about no chat.
    pub fn forget_notification_session(&self, session: &str) -> Result<()> {
        store_call!(self.forget_notification_session(session));
        self.on_both(|c| {
            c.execute("DELETE FROM notifications WHERE session_id = ?1", [session])?;
            Ok(Vec::<()>::new())
        })?;
        prune(&self.private_notes.lock(), now_ms())
    }

    pub fn device_notify(&self, device: &str) -> Result<bool> {
        store_call!(self.device_notify(device));
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT notify FROM device_prefs WHERE device_id = ?1",
                [device],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_none_or(|v| v != 0))
    }

    /// False when there is no such device.
    pub fn set_device_notify(&self, device: &str, notify: bool) -> Result<bool> {
        store_call!(self.set_device_notify(device, notify));
        let conn = self.conn.lock();
        let known = conn
            .query_row("SELECT 1 FROM devices WHERE id = ?1", [device], |_| Ok(()))
            .optional()?
            .is_some();
        if known {
            conn.execute(
                "INSERT INTO device_prefs(device_id, notify) VALUES (?1, ?2)
                 ON CONFLICT(device_id) DO UPDATE SET notify = excluded.notify",
                params![device, notify as i64],
            )?;
        }
        Ok(known)
    }

    /// The devices that take notifications (a machine's login never does).
    pub fn notified_devices(&self) -> Result<Vec<(String, String)>> {
        store_call!(self.notified_devices());
        let conn = self.conn.lock();
        let rows = conn
            .prepare(
                "SELECT d.id, d.name FROM devices d LEFT JOIN device_prefs p ON p.device_id = d.id
                 WHERE coalesce(p.notify, 1) = 1 AND d.kind = 'device' ORDER BY d.created_at",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ---- shared files ---------------------------------------------------

    /// `owned`: the file is Tessera's own copy, deleted with the record.
    pub fn add_shared_file(&self, f: &SharedFile, owned: bool) -> Result<()> {
        store_call!(self.add_shared_file(f, owned));
        let data = serde_json::to_string(&FileData {
            file: f.clone(),
            owned,
        })?;
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO shared_files(id, session_id, board_id, created_at, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![f.id, f.session_id, f.board_id, f.created_at as i64, data],
        )?;
        Ok(())
    }

    /// The file and whether Tessera owns it.
    pub fn shared_file(&self, id: &str) -> Result<Option<(SharedFile, bool)>> {
        store_call!(self.shared_file(id));
        let data: Option<String> = self
            .conn
            .lock()
            .query_row("SELECT data FROM shared_files WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(data
            .and_then(|d| serde_json::from_str::<FileData>(&d).ok())
            .map(|d| (d.file, d.owned)))
    }

    /// Newest first: of these sessions, of a board, or all (`None`, `None`).
    pub fn shared_files(
        &self,
        sessions: Option<&[String]>,
        board: Option<&str>,
    ) -> Result<Vec<(SharedFile, bool)>> {
        store_call!(self.shared_files(sessions, board));
        let conn = self.conn.lock();
        let mut out = Vec::new();
        let mut push = |data: String| {
            if let Ok(d) = serde_json::from_str::<FileData>(&data) {
                out.push((d.file, d.owned));
            }
        };
        match (sessions, board) {
            (Some(sessions), _) => {
                let mut stmt =
                    conn.prepare("SELECT data FROM shared_files WHERE session_id = ?1")?;
                for s in sessions {
                    for d in stmt.query_map([s], |r| r.get::<_, String>(0))? {
                        push(d?);
                    }
                }
            }
            (None, Some(b)) => {
                let mut stmt = conn.prepare("SELECT data FROM shared_files WHERE board_id = ?1")?;
                for d in stmt.query_map([b], |r| r.get::<_, String>(0))? {
                    push(d?);
                }
            }
            (None, None) => {
                let mut stmt = conn.prepare("SELECT data FROM shared_files")?;
                for d in stmt.query_map([], |r| r.get::<_, String>(0))? {
                    push(d?);
                }
            }
        }
        out.sort_by(|a, b| {
            b.0.created_at
                .cmp(&a.0.created_at)
                .then_with(|| b.0.id.cmp(&a.0.id))
        });
        Ok(out)
    }

    pub fn delete_shared_file(&self, id: &str) -> Result<bool> {
        store_call!(self.delete_shared_file(id));
        Ok(self
            .conn
            .lock()
            .execute("DELETE FROM shared_files WHERE id = ?1", [id])?
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: &str, due_at: u64) -> NewNotification {
        NewNotification {
            title: title.into(),
            body: "body".into(),
            session_id: Some("s1".into()),
            from: "Chat".into(),
            due_at,
            ..Default::default()
        }
    }

    #[test]
    fn notifications_reach_each_device_once_and_seen_reaches_all() {
        let store = Store::in_memory().unwrap();
        let (phone, _) = store.add_device("phone").unwrap();
        let (desk, _) = store.add_device("desk").unwrap();
        let now = now_ms();
        let a = store.add_notification(&note("a", 0)).unwrap();
        assert!(a.sent);
        let later = store
            .add_notification(&note("later", now + 60_000))
            .unwrap();
        assert!(!later.sent);
        assert_eq!(store.next_notification_due().unwrap(), Some(later.due_at));

        let titles = |v: Vec<UserNotification>| v.into_iter().map(|n| n.title).collect::<Vec<_>>();
        assert_eq!(
            titles(store.unshown_notifications(&phone, now).unwrap()),
            ["a"]
        );
        store.mark_notifications_shown(&phone, &[a.id]).unwrap();
        assert!(store.unshown_notifications(&phone, now).unwrap().is_empty());
        // The desk still has it.
        assert_eq!(
            titles(store.unshown_notifications(&desk, now).unwrap()),
            ["a"]
        );

        // Due later: nothing yet, then exactly once.
        assert!(store.take_due_notifications(now).unwrap().is_empty());
        let due = store.take_due_notifications(now + 61_000).unwrap();
        assert_eq!(titles(due), ["later"]);
        assert!(
            store
                .take_due_notifications(now + 62_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.next_notification_due().unwrap(), None);

        // Seen by opening the chat on the desk: the phone removes "a".
        let seen = store.mark_notifications_seen(&[], &["s1".into()]).unwrap();
        assert_eq!(seen, [a.id, later.id]);
        assert_eq!(store.seen_since_shown(&phone, now).unwrap(), [a.id]);
        assert!(store.unshown_notifications(&desk, now).unwrap().is_empty());
        // Seeing again changes nothing.
        assert!(
            store
                .mark_notifications_seen(&[a.id], &[])
                .unwrap()
                .is_empty()
        );

        // A day later a device that was away doesn't get it any more.
        let b = store.add_notification(&note("b", 0)).unwrap();
        assert!(
            store
                .unshown_notifications(&desk, now + PENDING_WINDOW_MS + 120_000)
                .unwrap()
                .iter()
                .all(|n| n.id != b.id)
        );
    }

    #[test]
    fn devices_can_opt_out_and_revoked_devices_leave_no_trace() {
        let store = Store::in_memory().unwrap();
        let (phone, _) = store.add_device("phone").unwrap();
        let (desk, _) = store.add_device("desk").unwrap();
        store.add_device_as("build box", true).unwrap();
        assert!(store.device_notify(&phone).unwrap());
        assert!(store.set_device_notify(&desk, false).unwrap());
        assert!(!store.set_device_notify("nobody", false).unwrap());
        assert!(!store.device_notify(&desk).unwrap());
        let names: Vec<String> = store
            .notified_devices()
            .unwrap()
            .into_iter()
            .map(|d| d.1)
            .collect();
        assert_eq!(names, ["phone"]);
        let listed = store.devices(None).unwrap();
        assert!(listed.iter().any(|d| d.id == desk && !d.notify));
        assert!(listed.iter().any(|d| d.id == phone && d.notify));
        let n = store.add_notification(&note("x", 0)).unwrap();
        store.mark_notifications_shown(&desk, &[n.id]).unwrap();
        store.mark_notifications_shown("unknown", &[n.id]).unwrap();
        assert!(store.revoke_device(&desk).unwrap());
        assert!(store.device_notify(&desk).unwrap(), "prefs went with it");
    }

    #[test]
    fn shared_files_by_session_and_board() {
        let store = Store::in_memory().unwrap();
        let file = |id: &str, s: Option<&str>, b: Option<&str>, t: u64| SharedFile {
            id: id.into(),
            name: format!("{id}.txt"),
            mime: "text/plain".into(),
            size: 3,
            created_at: t,
            session_id: s.map(Into::into),
            board_id: b.map(Into::into),
            source: "agent".into(),
            path: "/tmp/x".into(),
        };
        store
            .add_shared_file(&file("f1", Some("s1"), None, 1), true)
            .unwrap();
        store
            .add_shared_file(&file("f2", Some("s1"), Some("b1"), 2), false)
            .unwrap();
        store
            .add_shared_file(&file("f3", Some("s2"), None, 3), true)
            .unwrap();
        let ids = |v: Vec<(SharedFile, bool)>| v.into_iter().map(|f| f.0.id).collect::<Vec<_>>();
        assert_eq!(
            ids(store.shared_files(Some(&["s1".into()]), None).unwrap()),
            ["f2", "f1"]
        );
        assert_eq!(ids(store.shared_files(None, Some("b1")).unwrap()), ["f2"]);
        assert_eq!(
            ids(store.shared_files(None, None).unwrap()),
            ["f3", "f2", "f1"]
        );
        assert_eq!(store.shared_file("f2").unwrap().map(|f| f.1), Some(false));
        assert!(store.delete_shared_file("f1").unwrap());
        assert!(store.shared_file("f1").unwrap().is_none());
    }
    #[test]
    fn claude_limit_window_is_restart_safe_and_marked_only_after_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notifications.db");
        let reset = now_ms() + 3_600_000;
        let n = note("Claude wait", 0);
        {
            let store = Store::open(&path).unwrap();
            // Failure must roll back BOTH the notification and dedup marker.
            store.conn.lock().execute_batch("CREATE TRIGGER reject_notify BEFORE INSERT ON notifications BEGIN SELECT RAISE(ABORT, 'fixture insert failure'); END;").unwrap();
            assert!(
                store
                    .add_notification_once(&n, Some(("chat/account/five-hour/reset", reset)))
                    .is_err()
            );
            assert!(store.notifications(10).unwrap().is_empty());
            store
                .conn
                .lock()
                .execute_batch("DROP TRIGGER reject_notify")
                .unwrap();
            let first = store
                .add_notification_once(&n, Some(("chat/account/five-hour/reset", reset)))
                .unwrap()
                .unwrap();
            // Reconnect/ack/deleting retained notices must not re-alert.
            store
                .conn
                .lock()
                .execute("DELETE FROM notifications WHERE id = ?1", [first.id])
                .unwrap();
            assert!(
                store
                    .add_notification_once(&n, Some(("chat/account/five-hour/reset", reset)))
                    .unwrap()
                    .is_none()
            );
        }
        let store = Store::open(&path).unwrap();
        for title in ["Claude wait", "Stopped by an error", "Finished"] {
            assert!(
                store
                    .add_notification_once(
                        &note(title, 0),
                        Some(("chat/account/five-hour/reset", reset))
                    )
                    .unwrap()
                    .is_none(),
                "all notification kinds share the persisted window"
            );
        }
        assert!(
            store
                .add_notification_once(&n, Some(("chat/account/five-hour/reset", reset)))
                .unwrap()
                .is_none()
        );
        for identity in ["other-chat", "other-account", "seven-day", "new-reset"] {
            assert!(
                store
                    .add_notification_once(&n, Some((identity, reset + 1)))
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(store.notifications(10).unwrap().len(), 4);
    }

    fn activity(kind: &str, session: &str, title: &str, replace: Replace) -> NewNotification {
        NewNotification {
            title: title.into(),
            session_id: Some(session.into()),
            kind: kind.into(),
            quiet: true,
            replace,
            ..Default::default()
        }
    }

    #[test]
    fn quiet_ones_are_listed_but_never_alert_and_replace_their_earlier_one() {
        let store = Store::in_memory().unwrap();
        let (phone, _) = store.add_device("phone").unwrap();
        let now = now_ms();
        // One per run: the same outcome again changes nothing, another one
        // updates it (unread again, still one item).
        let (run, r) = store
            .record_notification(&activity("job", "run1", "Finished", Replace::Same))
            .unwrap();
        assert_eq!(r, Recorded::New);
        assert!(run.quiet && run.sent && !run.seen && run.kind == "job");
        assert!(store.unshown_notifications(&phone, now).unwrap().is_empty());
        assert!(
            store
                .take_due_notifications(now + 60_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.next_notification_due().unwrap(), None);
        store.mark_notifications_seen(&[run.id], &[]).unwrap();
        let (again, r) = store
            .record_notification(&activity("job", "run1", "Finished", Replace::Same))
            .unwrap();
        assert_eq!((again.id, again.seen, r), (run.id, true, Recorded::Same));
        // A failure alerts: delivered to the devices, unread again.
        let failed = NewNotification {
            quiet: false,
            ..activity("job", "run1", "Failed", Replace::Same)
        };
        let (n, r) = store.record_notification(&failed).unwrap();
        assert_eq!(
            (n.id, n.seen, n.quiet, r),
            (n.id, false, false, Recorded::Changed { previous: run.id })
        );
        assert_ne!(n.id, run.id);
        assert_eq!(store.unshown_notifications(&phone, now).unwrap().len(), 1);
        store.mark_notifications_shown(&phone, &[n.id]).unwrap();
        assert!(store.unshown_notifications(&phone, now).unwrap().is_empty());
        // Changed once more: shown again.
        let failed = NewNotification {
            body: "again".into(),
            ..failed
        };
        store.record_notification(&failed).unwrap();
        assert_eq!(store.unshown_notifications(&phone, now).unwrap().len(), 1);
        assert_eq!(store.notifications(500).unwrap().len(), 1);

        // One per chat while unread: a read one makes way for a new one.
        let a = store
            .add_notification(&activity(
                "helper",
                "chat",
                "A helper reported back",
                Replace::Unread,
            ))
            .unwrap();
        let b = store
            .add_notification(&activity(
                "helper",
                "chat",
                "2 helpers reported back",
                Replace::Unread,
            ))
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(
            store
                .unread_notification("chat", "helper")
                .unwrap()
                .unwrap()
                .title,
            "2 helpers reported back"
        );
        assert!(
            store
                .unread_notification("chat", "approval")
                .unwrap()
                .is_none()
        );
        // Kinds don't mix, nor chats.
        let c = store
            .add_notification(&activity(
                "approval",
                "chat",
                "Needs your approval",
                Replace::Unread,
            ))
            .unwrap();
        assert_ne!(c.id, b.id);
    }

    #[test]
    fn activity_read_at_once_was_no_news_but_messages_stay_as_history() {
        let store = Store::in_memory().unwrap();
        let quick = store
            .add_notification(&activity("finished", "chat", "Finished", Replace::Unread))
            .unwrap();
        let message = store.add_notification(&note("Build done", 0)).unwrap();
        let run = store
            .add_notification(&activity(
                "job",
                "run",
                "Scheduled task finished",
                Replace::Same,
            ))
            .unwrap();
        let mut seen = store
            .mark_notifications_seen(&[quick.id, message.id, run.id], &[])
            .unwrap();
        seen.sort_unstable();
        assert_eq!(seen.len(), 3);
        assert!(store.notification(quick.id).unwrap().is_none(), "gone");
        assert!(store.notification(message.id).unwrap().unwrap().seen);
        assert!(store.notification(run.id).unwrap().unwrap().seen);
        // One that waited longer stays, read.
        let slow = store
            .add_notification(&activity(
                "approval",
                "chat",
                "Needs your approval",
                Replace::Unread,
            ))
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE notifications SET due_at = due_at - 120000 WHERE id = ?1",
                [slow.id as i64],
            )
            .unwrap();
        store.mark_notifications_seen(&[slow.id], &[]).unwrap();
        assert!(store.notification(slow.id).unwrap().unwrap().seen);
        // Clear forgets sent ones only.
        let later = store
            .add_notification(&note("Later", now_ms() + 3_600_000))
            .unwrap();
        let mut gone = store
            .clear_notifications(&[message.id, later.id, 999_999])
            .unwrap();
        gone.sort_unstable();
        assert_eq!(gone, [message.id]);
        assert!(store.notification(later.id).unwrap().is_some());
    }

    #[test]
    fn an_older_server_never_alerts_a_quiet_one_and_bounds_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sqlite");
        let store = Store::open(&path).unwrap();
        store
            .add_notification(&activity(
                "job",
                "run",
                "Scheduled task finished",
                Replace::Same,
            ))
            .unwrap();
        store.add_notification(&note("Alert", 0)).unwrap();
        // The deployed server's own queries, run on the same database.
        let old = Connection::open(&path).unwrap();
        let count = |sql: &str| old.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(
            count("SELECT count(*) FROM notifications WHERE sent = 1 AND seen = 0"),
            1
        );
        assert_eq!(
            count("SELECT count(*) FROM notifications WHERE sent = 0"),
            0
        );
        drop(old);
        for _ in 0..505 {
            store.add_notification(&note("Reminder", 0)).unwrap();
        }
        let scheduled = store
            .add_notification(&note("Tomorrow", now_ms() + 86_400_000))
            .unwrap();
        assert_eq!(store.notifications(500).unwrap().len(), 501);
        store
            .prune_notifications(now_ms() + KEEP_SENT_MS + 1)
            .unwrap();
        let items = store.notifications(500).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, scheduled.id);
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .notifications(500)
                .unwrap()
                .len(),
            1
        );
    }
}
