//! Linux desktop notifications, over D-Bus (`org.freedesktop.Notifications`).
//!
//! One connection sends them and hears the clicks: some desktops only send
//! a click to the connection that created the notification.

use std::collections::HashMap;

use futures::StreamExt;
use parking_lot::Mutex;
use tauri::{AppHandle, Manager};
use zbus::zvariant::Value;

use super::Note;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

pub(super) trait Sender: Sync {
    fn setup(&self) -> impl std::future::Future<Output = Result<(), String>> + Send;
    fn send(
        &self,
        note: &Note,
        replaces: u32,
    ) -> impl std::future::Future<Output = Result<u32, String>> + Send;
}

pub(super) struct DbusSender<'a> {
    pub xdg: &'a Xdg,
    pub app: &'a AppHandle,
    pub connection: Mutex<Option<zbus::Connection>>,
}

impl Sender for DbusSender<'_> {
    async fn setup(&self) -> Result<(), String> {
        let connection = self.xdg.conn(self.app).await;
        *self.connection.lock() = connection;
        self.connection
            .lock()
            .as_ref()
            .map(|_| ())
            .ok_or_else(|| "desktop notifications unavailable".into())
    }

    async fn send(&self, note: &Note, replaces: u32) -> Result<u32, String> {
        let conn = self
            .connection
            .lock()
            .clone()
            .ok_or("desktop notifications unavailable")?;
        let mut hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("desktop-entry", Value::from("tessera")),
            ("urgency", Value::U8(if note.silent { 0 } else { 1 })),
        ]);
        if note.silent {
            hints.insert("suppress-sound", Value::Bool(true));
        }
        conn.call_method(
            Some(NAME),
            PATH,
            Some(NAME),
            "Notify",
            &(
                "Tessera",
                replaces,
                "tessera",
                note.title.as_str(),
                escape(&note.body),
                vec!["default", "Open"],
                hints,
                -1i32,
            ),
        )
        .await
        .and_then(|m| m.body().deserialize::<u32>())
        .map_err(|e| e.to_string())
    }
}
type Record = (String, Option<(Note, tessera_client::Client)>);

#[derive(Default)]
pub(super) struct Xdg {
    /// Opened on first use: no D-Bus traffic unless something is shown.
    conn: tokio::sync::Mutex<Option<zbus::Connection>>,
    /// Our notification ids by key, and each id's key and session.
    ids: Mutex<HashMap<String, u32>>,
    keys: Mutex<HashMap<u32, Record>>,
}

impl Xdg {
    async fn conn(&self, app: &AppHandle) -> Option<zbus::Connection> {
        let mut conn = self.conn.lock().await;
        if conn.is_none() {
            match listen(app).await {
                Ok(c) => *conn = Some(c),
                Err(e) => tracing::warn!("desktop notifications unavailable: {e}"),
            }
        }
        conn.clone()
    }

    pub(super) async fn show(
        &self,
        notifier: &super::Notifier,
        note: &Note,
        sender: &impl Sender,
    ) -> bool {
        if sender.setup().await.is_err() {
            return false;
        }
        let _dispatch = notifier.dispatch.lock().await;
        let Some(note) = notifier.inner.lock().platform_payload(note) else {
            return false;
        };
        let replaces = self.ids.lock().get(&note.key).copied().unwrap_or(0);
        // Dispatch and reply stay serialized with installation, disconnect and
        // policy changes. Never hold the synchronous Inner guard over await.
        let id = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            sender.send(&note, replaces),
        )
        .await
        {
            Ok(Ok(id)) => id,
            _ => {
                tracing::warn!("desktop notification was not accepted");
                return false;
            }
        };
        let activation = note
            .session
            .as_ref()
            .and_then(|_| notifier.client.lock().clone())
            .map(|c| (note.clone(), c));
        if let Some(old) = self.ids.lock().insert(note.key.clone(), id)
            && old != id
        {
            self.keys.lock().remove(&old);
        }
        self.keys.lock().insert(id, (note.key.clone(), activation));
        notifier.inner.lock().accepted(&note, true);
        true
    }

    pub(super) async fn close(&self, key: &str) {
        let Some(id) = self.ids.lock().remove(key) else {
            return;
        };
        self.keys.lock().remove(&id);
        let conn = self.conn.lock().await.clone();
        if let Some(conn) = conn {
            let _ = conn
                .call_method(Some(NAME), PATH, Some(NAME), "CloseNotification", &(id,))
                .await;
        }
    }

    fn take(&self, id: u32) -> Option<Record> {
        let (key, session) = self.keys.lock().remove(&id)?;
        self.ids.lock().remove(&key);
        Some((key, session))
    }
}

/// The body may be read as markup (`body-markup`): links and images from a
/// title or preview must stay plain text.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn listen(app: &AppHandle) -> zbus::Result<zbus::Connection> {
    let conn = zbus::Connection::session().await?;
    // Only signals from the notification service itself count.
    let proxy = zbus::Proxy::new(&conn, NAME, PATH, NAME).await?;
    let mut clicks = proxy.receive_signal("ActionInvoked").await?;
    let mut closed = proxy.receive_signal("NotificationClosed").await?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                Some(m) = clicks.next() => {
                    // Ids we didn't hand out (another app's) are ignored.
                    if let Ok((id, action)) = m.body().deserialize::<(u32, String)>()
                        && action == "default"
                        && let Some((_, Some((note, client)))) = app.state::<Xdg>().take(id)
                        && let (Some(session), Some(epoch)) = (note.session, note.client_epoch)
                    {
                        super::opened_on(&app, session, note.generation, note.transport_epoch, epoch, &client);
                    }
                }
                Some(m) = closed.next() => {
                    if let Ok((id, _reason)) = m.body().deserialize::<(u32, u32)>() {
                        app.state::<Xdg>().take(id);
                    }
                }
                else => break,
            }
        }
    });
    Ok(conn)
}

#[cfg(test)]
mod tests {
    #[test]
    fn markup_stays_text() {
        assert_eq!(
            super::escape("<a href=\"https://x\">run</a> & <img src=x>"),
            "&lt;a href=\"https://x\"&gt;run&lt;/a&gt; &amp; &lt;img src=x&gt;"
        );
    }
}
