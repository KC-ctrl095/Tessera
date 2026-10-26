//! Notifications, and the Android keep-alive.
//!
//! Decided here, from the server events the app already receives. Nothing is
//! shown while the app is in front; the text is generic unless the user turns
//! on previews; private chats never show their title; and a session gets at
//! most one notification every few seconds, replacing its previous one. On
//! Android the same events decide when a foreground service keeps the
//! connection alive (see `plugins/background`).

mod os;
/// A phone checking in from the background (Android, through JNI).
#[cfg(any(target_os = "android", test))]
pub(crate) mod pull;
#[cfg(any(windows, target_os = "linux", test))]
mod toast_content;
#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
mod xdg;

use tauri::{AppHandle, Emitter, Manager, State};

use tessera_proto::Request;

include!("notify/logic.rs");

impl Notifier {
    async fn save(&self) -> Result<(), String> {
        let _dispatch = self.dispatch.lock().await;
        let gate = self.save_gate.clone();
        let inner = self.inner.clone();
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            let _save = gate.lock();
            let data =
                serde_json::to_vec_pretty(&inner.lock().stored).map_err(std::io::Error::other)?;
            crate::private_file::write_verified(&file, &data)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("couldn't save notification settings: {e}"))
    }
}

/// Runs the platform side and the deadline timer. Call after `load`'s
/// notifier is managed.
pub(crate) fn start(app: &AppHandle, rx: tokio::sync::mpsc::UnboundedReceiver<Action>) {
    os::setup(app);
    tauri::async_runtime::spawn(os::run(app.clone(), rx));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let n = app.state::<Notifier>();
        loop {
            let next = n.inner.lock().next_deadline();
            match next {
                Some(t) => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(t.into()) => {}
                        _ = n.wake.notified() => {}
                    }
                }
                None => n.wake.notified().await,
            }
            n.tick(Instant::now());
        }
    });
}

/// Tell the server agents' messages were handled on this device, so a
/// background check doesn't show them again.
fn ack(app: &AppHandle, ack: &Ack) {
    let notifier = app.state::<Notifier>();
    let inner = notifier.inner.lock();
    if inner.generation != ack.generation
        || inner.transport_epoch != ack.epoch
        || !inner.link
        || !inner.attached
    {
        return;
    }
    let client = notifier.client.lock();
    if let (Some(client), Some(epoch)) = (client.as_ref(), ack.client_epoch)
        && inner.accepts_ack(ack, client)
    {
        if ack.delivered {
            client.post_in_epoch(
                epoch,
                Request::ReportNotificationDelivery {
                    ids: ack.ids.clone(),
                },
            );
        }
        client.post_in_epoch(
            epoch,
            Request::AckNotifications {
                ids: ack.ids.clone(),
            },
        );
    }
}

/// Where the notifier learns the session list after (re)connecting.
pub(crate) fn resync(app: &AppHandle, generation: u64, client: &Client) {
    let Some(token) = app.state::<Notifier>().inner.lock().begin_sync(generation) else {
        return;
    };
    let (app, client) = (app.clone(), client.clone());
    tauri::async_runtime::spawn(async move {
        let automatic = client
            .call_as::<bool>(Request::NegotiateNotifications)
            .await
            .unwrap_or(false)
            && client
                .call(Request::NotificationPresence {
                    active: false,
                    session_ids: Vec::new(),
                })
                .await
                .is_ok();
        // Do this before any hydration calls: a new-route JobUpdated must
        // not take the old fallback while its companion typed alert arrives.
        let notifier = app.state::<Notifier>();
        let mut valid = false;
        notifier.with(|i, now| {
            valid = i.negotiated(token, automatic);
            if valid {
                i.replay_jobs(now)
            } else {
                Vec::new()
            }
        });
        if !valid {
            return;
        }
        let jobs = if automatic {
            None
        } else {
            client.call_as::<Vec<Job>>(Request::ListJobs).await.ok()
        };
        let sessions = client
            .call_as::<Vec<SessionInfo>>(Request::ListSessions { archived: false })
            .await
            .ok();
        // Agents' messages sent while this device was away (servers from
        // before they existed answer with an error: nothing missed).
        let pending = client
            .call_as::<PendingNotifications>(if automatic {
                Request::PendingNotificationsV2
            } else {
                Request::PendingNotifications
            })
            .await
            .ok();
        app.state::<Notifier>()
            .with(|i, now| i.hydrated(token, now, sessions, jobs, pending));
    });
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenSession {
    session_id: String,
}

#[cfg(any(windows, target_os = "linux"))]
fn opened_on(
    app: &AppHandle,
    session: String,
    generation: u64,
    epoch: u64,
    client_epoch: u64,
    client: &Client,
) {
    let state = app.state::<crate::AppState>();
    let conn = state.conn.lock();
    if !conn
        .as_ref()
        .is_some_and(|(_, current)| current.same_connection(client))
    {
        return;
    }
    let notifier = app.state::<Notifier>();
    let mut inner = notifier.inner.lock();
    if !super::notify::toast_content::connection_allowed(
        generation,
        inner.generation,
        inner.attached,
    ) || inner.transport_epoch != epoch
        || client.connected_epoch() != Some(client_epoch)
        || !inner.link
        || !valid_id(&session)
    {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    inner.opened = Some((session.clone(), Instant::now()));
    let actions = inner.seen(&session);
    notifier.publish(&inner, actions);
    let _ = app.emit(
        "tessera://open-session",
        serde_json::json!({"sessionId": session}),
    );
}

/// A notification was tapped: the UI opens that session.
#[cfg_attr(not(mobile), allow(dead_code))]
fn opened(app: &AppHandle, session: String) {
    if !valid_id(&session) {
        return;
    }
    app.state::<Notifier>().with(|i, now| {
        i.opened = Some((session.clone(), now));
        i.seen(&session).into_iter().collect()
    });
    let _ = app.emit(
        "tessera://open-session",
        OpenSession {
            session_id: session,
        },
    );
}

/// The app is back in front (Android): refresh the permission, ask for it
/// if an approval was missed without it, reconnect after a "Disconnect".
#[cfg_attr(not(mobile), allow(dead_code))]
pub(crate) fn resumed(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let n = app.state::<Notifier>();
        let (permission, _) = os::status(&app).await;
        n.set_permission(permission);
        let ask = n.inner.lock().ask_after_miss();
        if ask {
            let _ = n.save().await;
            n.set_permission(os::request_permission(&app).await);
        }
    });
}

/// Settings plus what the platform says, for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationsState {
    #[serde(flatten)]
    settings: Settings,
    permission: Permission,
    /// Android: battery optimisation still applies to the app. Null elsewhere.
    battery_optimized: Option<bool>,
    /// The keep-alive setting does something here (Android).
    keep_alive_available: bool,
}

async fn state(app: &AppHandle) -> NotificationsState {
    let (permission, battery_optimized) = os::status(app).await;
    let n = app.state::<Notifier>();
    n.set_permission(permission);
    NotificationsState {
        settings: n.settings(),
        permission,
        battery_optimized,
        keep_alive_available: cfg!(target_os = "android"),
    }
}

/// Shows the system prompt if it can still be shown.
async fn ensure_permission(app: &AppHandle) -> Permission {
    let (permission, _) = os::status(app).await;
    if permission != Permission::Prompt {
        return permission;
    }
    let n = app.state::<Notifier>();
    n.inner.lock().stored.permission_asked = true;
    let _ = n.save().await;
    let permission = os::request_permission(app).await;
    n.set_permission(permission);
    permission
}

#[tauri::command]
pub(crate) async fn notifications_get(app: AppHandle) -> Result<NotificationsState, String> {
    Ok(state(&app).await)
}

/// Save the settings. Turning notifications on is when the system
/// permission is asked; choosing "always" offers the battery exemption.
#[tauri::command]
pub(crate) async fn notifications_set(
    app: AppHandle,
    settings: Settings,
) -> Result<NotificationsState, String> {
    let n = app.state::<Notifier>();
    let (before, asked) = {
        let i = n.inner.lock();
        (i.settings().clone(), i.stored.permission_asked)
    };
    n.apply_settings(settings.clone()).await?;
    if settings.enabled && (!before.enabled || !asked) {
        ensure_permission(&app).await;
    }
    if settings.keep_alive == KeepAlive::Always && before.keep_alive != KeepAlive::Always {
        os::request_battery_exemption(&app).await;
    }
    if settings.enabled != before.enabled {
        os::configure_pull(&app, settings.enabled).await;
    }
    Ok(state(&app).await)
}

#[tauri::command]
pub(crate) async fn notifications_request_permission(
    app: AppHandle,
) -> Result<NotificationsState, String> {
    ensure_permission(&app).await;
    Ok(state(&app).await)
}

/// A sample notification, shown even with the app in front.
#[tauri::command]
pub(crate) async fn notifications_test(app: AppHandle) -> Result<NotificationsState, String> {
    if ensure_permission(&app).await == Permission::Denied {
        return Err("Notifications are turned off for Tessera in the system settings.".into());
    }
    app.state::<Notifier>().sample();
    Ok(state(&app).await)
}

/// The UI shows this session: its notification is no longer news.
#[tauri::command]
pub(crate) fn notifications_seen(notifier: State<'_, Notifier>, session_id: String) {
    if valid_id(&session_id) {
        notifier.with(|i, _| i.seen(&session_id).into_iter().collect());
    }
}

/// The session a notification tap asked for, if it launched the app in
/// the last minute (the UI may not have been listening yet).
#[tauri::command]
pub(crate) fn notifications_take_opened(notifier: State<'_, Notifier>) -> Option<String> {
    let mut i = notifier.inner.lock();
    let (session, at) = i.opened.take()?;
    (at.elapsed() < OPEN_TTL).then_some(session)
}

#[cfg(mobile)]
#[tauri::command]
pub(crate) async fn battery_optimization_request(
    app: AppHandle,
) -> Result<NotificationsState, String> {
    os::request_battery_exemption(&app).await;
    Ok(state(&app).await)
}

#[cfg(mobile)]
#[tauri::command]
pub(crate) async fn notifications_open_settings(app: AppHandle) -> Result<(), String> {
    os::open_settings(&app).await
}
