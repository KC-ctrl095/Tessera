//! Showing notifications and running the keep-alive, per platform.
//!
//! - Linux: straight to the desktop's notification service over D-Bus, so
//!   a click can open the session and a session's notification can be
//!   replaced or withdrawn.
//! - Windows: WinRT toasts with live click-to-chat, replacement and withdrawal.
//! - macOS: tauri-plugin-notification (title and body only).
//! - Android: our own plugin (`plugins/background`), for immutable intents,
//!   lock-screen privacy and the foreground service.

use tauri::{AppHandle, Manager};

use super::{Action, Note, Permission, Service};

/// Before anything can post (a command may, right away).
pub(super) fn setup(_app: &AppHandle) {
    #[cfg(target_os = "linux")]
    _app.manage(super::xdg::Xdg::default());
    #[cfg(windows)]
    _app.manage(super::win::Toasts::default());
}

pub(super) async fn run(app: AppHandle, mut rx: tokio::sync::mpsc::UnboundedReceiver<Action>) {
    #[cfg(mobile)]
    init(&app).await;
    while let Some(action) = rx.recv().await {
        match action {
            Action::Post(note) => {
                let current = app
                    .state::<super::Notifier>()
                    .inner
                    .lock()
                    .accepts_post(&note);
                let accepted = current && post(&app, &note).await;
                let ack = app
                    .state::<super::Notifier>()
                    .inner
                    .lock()
                    .accepted(&note, accepted);
                if let Some(Action::Ack(ack)) = ack {
                    super::ack(&app, &ack);
                }
            }
            Action::Sample => sample(&app).await,
            Action::Withdraw(key) => withdraw(&app, &key).await,
            Action::Service(s) => service(&app, s).await,
            Action::Ack(ack) => super::ack(&app, &ack),
            Action::WakeAt(at) => wake_at(&app, at).await,
        }
    }
}

/// Local settings Test: generic content, no server session or activation.
async fn sample(app: &AppHandle) {
    #[cfg(windows)]
    if let Err(e) = app.state::<super::win::Toasts>().show_sample(app) {
        tracing::warn!(code = e.code().0, "sample notification not shown");
    }
    #[cfg(not(windows))]
    post(app, &Note::sample()).await;
}

// ---- Linux ---------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(super) async fn post(app: &AppHandle, note: &Note) -> bool {
    let xdg = app.state::<super::xdg::Xdg>();
    let sender = super::xdg::DbusSender {
        xdg: &xdg,
        app,
        connection: parking_lot::Mutex::new(None),
    };
    xdg.show(&app.state::<super::Notifier>(), note, &sender)
        .await
}

#[cfg(target_os = "linux")]
async fn withdraw(app: &AppHandle, key: &str) {
    app.state::<super::xdg::Xdg>().close(key).await;
}

// ---- macOS ---------------------------------------------------------------

#[cfg(all(desktop, not(any(target_os = "linux", windows))))]
pub(super) async fn post(app: &AppHandle, note: &Note) -> bool {
    use tauri_plugin_notification::NotificationExt;
    let notifier = app.state::<super::Notifier>();
    let inner = notifier.inner.lock();
    let Some(note) = inner.platform_payload(note) else {
        return false;
    };
    match app
        .notification()
        .builder()
        .title(&note.title)
        .body(&note.body)
        .show()
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("notification not shown: {e}");
            false
        }
    }
}

/// The plugin can't withdraw a desktop notification; the OS expires it.
#[cfg(all(desktop, not(any(target_os = "linux", windows))))]
async fn withdraw(_app: &AppHandle, _key: &str) {}

// ---- Windows -------------------------------------------------------------

#[cfg(windows)]
pub(super) async fn post(app: &AppHandle, note: &Note) -> bool {
    match app.state::<super::win::Toasts>().show(app, note) {
        Ok(accepted) => accepted,
        Err(e) => {
            tracing::warn!(code = e.code().0, "notification not shown");
            false
        }
    }
}

#[cfg(windows)]
async fn withdraw(app: &AppHandle, key: &str) {
    if let Err(e) = app.state::<super::win::Toasts>().close(app, key) {
        tracing::warn!(code = e.code().0, "notification not withdrawn");
    }
}

// ---- all desktops --------------------------------------------------------

/// Desktop apps aren't frozen in the background: nothing to keep alive.
#[cfg(desktop)]
async fn service(_app: &AppHandle, _s: Service) {}

/// Connected while open, and nothing runs while closed: nothing to wake.
#[cfg(desktop)]
async fn wake_at(_app: &AppHandle, _at: u64) {}

#[cfg(desktop)]
pub(super) async fn configure_pull(_app: &AppHandle, _enabled: bool) {}

/// Desktop notifications need no permission (macOS asks by itself).
#[cfg(desktop)]
pub(super) async fn status(_app: &AppHandle) -> (Permission, Option<bool>) {
    (Permission::Granted, None)
}

#[cfg(desktop)]
pub(super) async fn request_permission(_app: &AppHandle) -> Permission {
    Permission::Granted
}

#[cfg(desktop)]
pub(super) async fn request_battery_exemption(_app: &AppHandle) {}

// ---- Android -------------------------------------------------------------

#[cfg(mobile)]
use tessera_background::BackgroundExt;

/// Calls into Kotlin need the activity; once it is destroyed they would
/// abort the process instead of failing.
#[cfg(mobile)]
fn reachable(app: &AppHandle) -> bool {
    app.state::<super::Notifier>().reachable() && app.get_webview_window("main").is_some()
}

#[cfg(mobile)]
async fn init(app: &AppHandle) {
    let (open, disconnect) = (app.clone(), app.clone());
    let listening = app
        .background()
        .listen(
            move |session| super::opened(&open, session),
            move || {
                let app = disconnect.clone();
                tauri::async_runtime::spawn(async move { crate::pause_connection(&app).await });
            },
        )
        .await;
    if let Err(e) = listening {
        tracing::warn!("notification taps won't open sessions: {e}");
    }
    if let Ok(Some(session)) = app.background().take_opened().await {
        super::opened(app, session);
    }
    let (permission, _) = status(app).await;
    app.state::<super::Notifier>().set_permission(permission);
    let enabled = app.state::<super::Notifier>().settings().enabled;
    configure_pull(app, enabled).await;
}

#[cfg(mobile)]
pub(super) async fn post(app: &AppHandle, note: &Note) -> bool {
    if !reachable(app) {
        return false;
    }
    let note = {
        let notifier = app.state::<super::Notifier>();
        let inner = notifier.inner.lock();
        let Some(payload) = inner.platform_payload(note) else {
            return false;
        };
        payload
    };
    let post = tessera_background::Post {
        private: note.private,
        tag: note.key.clone(),
        session_id: note.session.clone(),
        urgent: note.urgent(),
        title: note.title.clone(),
        body: note.body.clone(),
        public_body: note.public_body().into(),
        agent: note.kind == super::Kind::Agent,
        silent: note.silent,
        sub: note.from.clone(),
        when: note.when,
    };
    match app.background().post(&post).await {
        Ok(accepted) => accepted,
        Err(e) => {
            tracing::warn!("notification not shown: {e}");
            false
        }
    }
}

/// Wakes up even with the app closed (an alarm, then a background check).
#[cfg(mobile)]
async fn wake_at(app: &AppHandle, at: u64) {
    if reachable(app) {
        let _ = app.background().wake_at(at).await;
    }
}

/// The background check (every ~15 minutes with the app closed) follows
/// the master switch.
#[cfg(mobile)]
pub(super) async fn configure_pull(app: &AppHandle, enabled: bool) {
    if !reachable(app) {
        return;
    }
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    if let Err(e) = app
        .background()
        .configure_pull(&dir.display().to_string(), enabled)
        .await
    {
        tracing::warn!("background check not set up: {e}");
    }
}

#[cfg(mobile)]
async fn withdraw(app: &AppHandle, key: &str) {
    if reachable(app) {
        let _ = app.background().withdraw(Some(key)).await;
    }
}

#[cfg(mobile)]
async fn service(app: &AppHandle, s: Service) {
    if !reachable(app) {
        return;
    }
    match app.background().keep_alive(s.run, s.eager).await {
        Ok(r) if r.error.is_some() => {
            tracing::warn!("keep-alive not started: {}", r.error.unwrap_or_default())
        }
        Err(e) => tracing::warn!("keep-alive: {e}"),
        Ok(_) => {}
    }
}

#[cfg(mobile)]
pub(super) async fn status(app: &AppHandle) -> (Permission, Option<bool>) {
    if !reachable(app) {
        return (Permission::Prompt, None);
    }
    match app.background().status().await {
        Ok(s) => (Permission::parse(&s.permission), Some(s.battery_optimized)),
        Err(_) => (Permission::Prompt, None),
    }
}

#[cfg(mobile)]
pub(super) async fn request_permission(app: &AppHandle) -> Permission {
    if !reachable(app) {
        return Permission::Prompt;
    }
    match app.background().request_permission().await {
        Ok(s) => Permission::parse(&s.permission),
        Err(_) => Permission::Prompt,
    }
}

#[cfg(mobile)]
pub(super) async fn request_battery_exemption(app: &AppHandle) {
    if reachable(app) {
        let _ = app.background().request_battery_exemption().await;
    }
}

#[cfg(mobile)]
pub(super) async fn open_settings(app: &AppHandle) -> Result<(), String> {
    if !reachable(app) {
        return Err("the app isn't on screen".into());
    }
    app.background().open_settings().await
}
