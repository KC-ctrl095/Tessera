//! Tessera app shell (desktop + Android).
//!
//! The web UI never talks to the network itself: this layer owns the server
//! connection so it can attach custom proxy headers (e.g. Cloudflare Access),
//! pick the reachable endpoint (LAN vs tunnel) and reconnect in the
//! background. See docs/PROTOCOL.md → "Tauri bridge".

mod notify;

use std::collections::BTreeMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tessera_client::{Client, ClientEvent, ConnectionStatus, ServerConfig};
use tessera_proto::{ApiError, ErrorCode, Request};

mod boards;
mod credential_write;
mod server_config;
mod voice;
#[cfg(test)]
use server_config::with_stored_secrets;
use server_config::{ClientConfig, SavedServer, redacted};
#[cfg(desktop)]
mod app_updates;
mod capture;
/// Running `tessera` with a time limit, never waiting on its output to close.
#[cfg(desktop)]
mod cli_run;
/// The composer's image paste on Linux (WebKitGTK keeps pasted images from the page).
#[cfg(desktop)]
mod clipboard;
/// Saving files from a chat on this device.
mod files;
/// Issue, durably save and only then retire the app's earlier local login.
#[cfg(desktop)]
mod local_login;
/// Settings → Devices → This computer: this computer as one of the user's machines.
#[cfg(desktop)]
mod machine;
mod private_file;
/// One desktop owner per app profile, held before settings are loaded.
#[cfg(desktop)]
mod profile_owner;
/// "Start Tessera here" as one job that "Stop waiting" doesn't cut short.
#[cfg(desktop)]
mod start_job;
#[cfg(any(all(windows, target_arch = "x86_64"), test))]
mod update_checks;
#[cfg(windows)]
mod win;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusPayload {
    state: tessera_client::ConnState,
    server_id: Option<String>,
    endpoint: Option<String>,
    error: Option<String>,
    /// End-to-end encrypted to the server's pinned key.
    encrypted: bool,
}

struct AppState {
    file: PathBuf,
    config: Mutex<ClientConfig>,
    conn: Mutex<Option<(String, Client)>>,
    lifecycle: tokio::sync::Mutex<()>,
    credentials: credential_write::CredentialWrites,
}

struct InstalledConnection {
    client: Client,
    events: tokio::sync::broadcast::Receiver<ClientEvent>,
    generation: u64,
}

impl AppState {
    /// Used by connect/disconnect, including setup and mobile resume. The
    /// caller retains the guard through event/status publication as well.
    async fn transition_connection<'a>(
        &'a self,
        notifier: &notify::Notifier,
        server: Option<&SavedServer>,
    ) -> (tokio::sync::MutexGuard<'a, ()>, Option<InstalledConnection>) {
        let lifecycle = self.lifecycle.lock().await;
        if let Some((_, old)) = self.conn.lock().take() {
            old.close();
        }
        let installed = if let Some(server) = server {
            let client = Client::connect_preferred(
                ServerConfig {
                    endpoints: server.endpoints.clone(),
                    headers: server.headers.clone(),
                    token: server.token.clone(),
                    server_key: server.server_key.clone(),
                },
                server.last_endpoint.clone(),
            );
            let events = client.subscribe();
            let generation = notifier
                .attach(
                    Some(client.clone()),
                    format!("{}:{}", server.id, server.server_key),
                )
                .await;
            *self.conn.lock() = Some((server.id.clone(), client.clone()));
            Some(InstalledConnection {
                client,
                events,
                generation,
            })
        } else {
            notifier.detach().await;
            None
        };
        (lifecycle, installed)
    }

    fn save(&self) -> Result<(), String> {
        // Keep writes serialized too: route-follow and a connection status can
        // save together, and write_private uses one temporary file per process.
        let cfg = self.config.lock();
        let data = serde_json::to_vec_pretty(&*cfg).map_err(|e| e.to_string())?;
        write_private(&self.file, &data).map_err(|e| e.to_string())
    }
}

/// Device tokens live here: the directory is owner-only and the file is
/// created 0600 (never readable in between, never written through a
/// planted symlink), then renamed into place.
fn write_private(file: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    private_file::write(file, data)
}

fn new_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("srv_{t:x}")
}

#[tauri::command]
fn servers_get(state: State<'_, AppState>) -> ClientConfig {
    redacted(&state.config.lock())
}

/// Replace the server list. Tokens and pinned keys always come from the
/// stored copy: the UI never has them and cannot change them.
#[tauri::command]
fn servers_save(state: State<'_, AppState>, config: ClientConfig) -> Result<(), String> {
    server_config::save_ui(&mut state.config.lock(), config, &state.file)
}

#[tauri::command]
async fn server_pair(
    app: AppHandle,
    state: State<'_, AppState>,
    endpoints: Vec<String>,
    headers: BTreeMap<String, String>,
    code: String,
    device_name: String,
    name: String,
) -> Result<SavedServer, String> {
    let endpoints: Vec<String> = endpoints
        .into_iter()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect();
    if endpoints.is_empty() {
        return Err("Enter at least one server address.".into());
    }
    state
        .credentials
        .run(|| async {
            let (endpoint, grant) = tessera_client::pair(&endpoints, &headers, &code, &device_name)
                .await
                .map_err(|e| e.to_string())?;
            let server = SavedServer {
                id: new_id(),
                name: if name.trim().is_empty() {
                    grant.server.name.clone()
                } else {
                    name
                },
                endpoints,
                headers,
                token: grant.token,
                server_key: grant.server_key,
                fingerprint: None,
                device_name,
                route: None,
                route_rev: 0,
                last_endpoint: Some(endpoint),
            };
            let (server, retire) = save_paired(&state, server)?;
            connect_retiring(&app, &state, &server.id, retire).await?;
            Ok(SavedServer {
                token: String::new(),
                fingerprint: tessera_client::key_fingerprint(&server.server_key),
                server_key: String::new(),
                route: None,
                ..server
            })
        })
        .await
}

/// Save a server just paired with, as the active one. Paired with that
/// same server before (the same key): its entry is replaced in place
/// instead of listed twice, and the login it held comes back, for the
/// server to sign out once the new one works (`retire_login`).
fn save_paired(
    state: &AppState,
    server: SavedServer,
) -> Result<(SavedServer, Option<String>), String> {
    server_config::save_paired(&mut state.config.lock(), server, &state.file)
}

/// Pair from a scanned `tessera://pair` QR code: addresses, proxy headers and
/// code come from the link, and the server's key must match its fingerprint.
#[tauri::command]
async fn server_pair_link(
    app: AppHandle,
    state: State<'_, AppState>,
    link: String,
    device_name: String,
) -> Result<SavedServer, String> {
    state
        .credentials
        .run(|| async {
            let (link, endpoint, grant) = tessera_client::pair_with_link(&link, &device_name)
                .await
                .map_err(|e| e.to_string())?;
            let route = Some(tessera_client::KnownRoute::from_pair_link(&link));
            let server = SavedServer {
                id: new_id(),
                name: if link.name.trim().is_empty() {
                    grant.server.name.clone()
                } else {
                    link.name.clone()
                },
                endpoints: link.urls,
                headers: link.headers.into_iter().collect(),
                token: grant.token,
                server_key: grant.server_key,
                fingerprint: None,
                device_name,
                route,
                route_rev: 0,
                last_endpoint: Some(endpoint),
            };
            let (server, retire) = save_paired(&state, server)?;
            connect_retiring(&app, &state, &server.id, retire).await?;
            Ok(SavedServer {
                token: String::new(),
                fingerprint: tessera_client::key_fingerprint(&server.server_key),
                server_key: String::new(),
                route: None,
                ..server
            })
        })
        .await
}

/// Connect to a saved server. `keep`: a connection to it that is already up
/// (or on its way, as right after pairing) stays; its status is sent again
/// instead of starting over.
#[tauri::command]
async fn server_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    keep: Option<bool>,
) -> Result<(), String> {
    server_config::save_active(&mut state.config.lock(), &id, &state.file)?;
    if keep.unwrap_or(false) {
        let current = state
            .conn
            .lock()
            .as_ref()
            .filter(|(sid, _)| *sid == id)
            .map(|(_, c)| c.status());
        if let Some(s) = current {
            emit_status(&app, &id, s);
            return Ok(());
        }
    }
    connect(&app, &state, &id).await
}

#[tauri::command]
async fn server_disconnect(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    disconnect(&app, &state).await;
    Ok(())
}

/// "Disconnect" on Android's keep-alive notification: close the connection
/// now and open it again when the user comes back to the app.
#[cfg_attr(not(mobile), allow(dead_code))]
async fn pause_connection(app: &AppHandle) {
    disconnect(app, &app.state::<AppState>()).await;
    app.state::<notify::Notifier>().pause();
}

async fn disconnect(app: &AppHandle, state: &AppState) {
    let (_lifecycle, _) = state
        .transition_connection(&app.state::<notify::Notifier>(), None)
        .await;
    let _ = app.emit(
        "tessera://status",
        StatusPayload {
            state: tessera_client::ConnState::Disconnected,
            server_id: None,
            endpoint: None,
            error: None,
            encrypted: false,
        },
    );
}

#[tauri::command]
async fn rpc(state: State<'_, AppState>, request: Request) -> Result<Value, ApiError> {
    let client = state.conn.lock().as_ref().map(|(_, c)| c.clone());
    match client {
        Some(c) => c.call(request).await,
        None => Err(ApiError::new(
            ErrorCode::Unauthorized,
            "not connected to a server",
        )),
    }
}

/// The app came back to the foreground (or the machine woke up): check the
/// connection right away instead of waiting for the next keep-alive.
#[tauri::command]
fn app_resumed(state: State<'_, AppState>) {
    nudge(&state);
}

/// What this device is, for the page's `device_hello` (Settings → Devices
/// shows the desktop app, terminal and machine of one computer as one):
/// computers give their key and name, phones their maker and model
/// ("samsung", "SM-F966B"), which the page turns into "Galaxy Z Fold".
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceReport {
    computer: Option<String>,
    host: Option<String>,
    manufacturer: Option<String>,
    model: Option<String>,
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
fn device_report() -> DeviceReport {
    DeviceReport {
        computer: tessera_client::computer_key(),
        host: tessera_client::host_name(),
        manufacturer: None,
        model: None,
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
fn device_report() -> DeviceReport {
    DeviceReport {
        computer: None,
        host: None,
        manufacturer: android_prop("ro.product.manufacturer"),
        model: android_prop("ro.product.model"),
    }
}

/// An Android system property (`Build.MANUFACTURER`, `Build.MODEL`).
#[cfg(target_os = "android")]
fn android_prop(name: &str) -> Option<String> {
    let key = std::ffi::CString::new(name).ok()?;
    let mut buf: [libc::c_char; libc::PROP_VALUE_MAX as usize] = [0; libc::PROP_VALUE_MAX as usize];
    // SAFETY: `key` ends with a zero byte; `buf` has room for the longest
    // value (PROP_VALUE_MAX, its zero byte included).
    let n = unsafe { libc::__system_property_get(key.as_ptr(), buf.as_mut_ptr()) };
    if n <= 0 {
        return None;
    }
    // SAFETY: the system wrote a zero-terminated value into `buf`.
    let v = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    let v = v
        .to_string_lossy()
        .trim()
        .chars()
        .take(60)
        .collect::<String>();
    (!v.is_empty()).then_some(v)
}

fn nudge(state: &AppState) {
    if let Some((_, c)) = state.conn.lock().as_ref() {
        c.nudge();
    }
}

/// A Tessera server on this computer, for the "run it here" setup path.
#[cfg(desktop)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalServer {
    /// The `tessera` program is installed.
    available: bool,
    running: bool,
    url: Option<String>,
    /// Already saved in this app.
    saved: bool,
}

#[cfg(desktop)]
fn tessera_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "tessera.exe"
    } else {
        "tessera"
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(PathBuf::from))
    {
        dirs.push(dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    // Windows: where the PowerShell installer puts it, and cargo's folder.
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs").join("Tessera"));
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(profile).join(".cargo").join("bin"));
        }
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Run `tessera` without a console window (Windows); nothing elsewhere.
#[cfg(desktop)]
fn quiet(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    win::quiet(cmd);
    cmd
}

/// The app's server is the one on this computer ("Start Tessera here"),
/// and nothing answers when the app opens: an update ended it (the
/// installer starts again only a server that starts at sign-in), or it ran
/// only until the PC restarted. It is started again the same way, so the
/// chats are there; a server that is still starting is waited for, and a
/// second one never runs (the launcher and the server each refuse that).
#[cfg(windows)]
async fn resume_local_server(app: AppHandle) {
    let outcome = try_resume_local_server(&app).await;
    recovery_said(&app, outcome.err());
}

/// Why the server on this computer didn't start again when the app opened
/// (or when asked again), for the connect screen: `None` once it did, or
/// when there was nothing to start.
#[cfg(desktop)]
static LOCAL_RECOVERY: Mutex<Option<String>> = Mutex::new(None);

#[cfg(windows)]
fn recovery_said(app: &AppHandle, why: Option<String>) {
    let why = why.map(|w| {
        match local_lock(app).and_then(|l| l.parent().map(|d| d.join("logs").join("tessera.log"))) {
            Some(log) => format!("{w} What it wrote is in {}.", log.display()),
            None => w,
        }
    });
    *LOCAL_RECOVERY.lock() = why.clone();
    let _ = app.emit("tessera://local-server-recovery", why);
}

/// See [`resume_local_server`]; an error says what went wrong, plainly.
#[cfg(windows)]
async fn try_resume_local_server(app: &AppHandle) -> Result<(), String> {
    // An update cut short took the startup entries out: back first, exactly
    // as they were (the installer's notes), which starts them too. If that
    // fails it is said (after the server's own start, below).
    let mut restore_failed = None;
    if let Some(bin) = tessera_binary() {
        restore_failed = match run_tessera(
            &bin,
            &["service", "restore-update", "--if-interrupted"],
            SERVICE_INSTALL_LIMIT,
        )
        .await
        {
            Ok(out) if out.success => None,
            Ok(out) => Some(cli_error(&out.stderr).unwrap_or_else(|| {
                "Tessera couldn't start again as it was before the update.".into()
            })),
            Err(e) => Some(ran_error(e, "finish the update")),
        }
        .map(|why| format!("After the update: {why}"));
    }
    let outcome = resume_this_server(app).await;
    match (outcome, restore_failed) {
        (Err(e), _) => Err(e),
        (Ok(()), Some(why)) => Err(why),
        (Ok(()), None) => Ok(()),
    }
}

/// The app's own server on this computer, started again when nothing
/// answers (see [`resume_local_server`]).
#[cfg(windows)]
async fn resume_this_server(app: &AppHandle) -> Result<(), String> {
    let app = app.clone();
    let saved_key = || {
        let state = app.state::<AppState>();
        let cfg = state.config.lock();
        cfg.active
            .as_ref()
            .and_then(|id| cfg.servers.iter().find(|s| &s.id == id))
            .map(|s| s.server_key.clone())
            .filter(|k| !k.is_empty())
    };
    let (Some(bin), Some(key)) = (tessera_binary(), saved_key()) else {
        return Ok(());
    };
    // Only this computer's own server: the one whose key the app pinned
    // (never another reached through a local address, a tunnel, another
    // data folder).
    if this_computers_key(&bin).await.as_deref() != Some(key.as_str()) {
        return Ok(());
    }
    // One started at sign-in (or by the installer) may still be coming up;
    // and the app's choice may have changed meanwhile.
    if wait_until_up(&app, std::time::Duration::from_secs(10)).await
        || saved_key().as_deref() != Some(key.as_str())
    {
        return Ok(());
    }
    start_server(&app, &bin)
        .map_err(|e| format!("Tessera's server on this computer didn't start again ({e})."))?;
    if !wait_until_up(&app, std::time::Duration::from_secs(60)).await {
        return Err("Tessera's server on this computer didn't start again within a minute.".into());
    }
    nudge(&app.state::<AppState>());
    Ok(())
}

/// Why the server on this computer didn't start again, if it didn't (see
/// [`resume_local_server`]).
#[cfg(desktop)]
#[tauri::command]
fn local_server_recovery() -> Option<String> {
    LOCAL_RECOVERY.lock().clone()
}

/// Try starting the server on this computer again (the connect screen's
/// button after it didn't); the error says why it still didn't.
#[cfg(desktop)]
#[tauri::command]
async fn local_server_resume(app: AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        let outcome = try_resume_local_server(&app).await;
        recovery_said(&app, outcome.clone().err());
        outcome
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Ok(())
    }
}

/// This computer's server key (its `device-token --key-only`), without
/// starting or issuing anything; `None` without one (or an older program).
#[cfg(all(desktop, any(windows, test)))]
async fn this_computers_key(bin: &std::path::Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Key {
        server_key: String,
    }
    let out = run_tessera(bin, &["device-token", "--key-only"], STATUS_LIMIT)
        .await
        .ok()
        .filter(|o| o.success)?;
    serde_json::from_slice::<Key>(&out.stdout)
        .ok()
        .map(|k| k.server_key)
        .filter(|k| !k.is_empty())
}

/// Where a running server announces itself (see the CLI's `server.json`).
#[cfg(desktop)]
fn local_lock(app: &AppHandle) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("TESSERA_HOME") {
        return Some(PathBuf::from(home).join("server.json"));
    }
    Some(
        app.path()
            .data_dir()
            .ok()?
            .join("tessera")
            .join("server.json"),
    )
}

#[cfg(desktop)]
async fn local_running(app: &AppHandle) -> Option<String> {
    #[derive(Deserialize)]
    struct Lock {
        url: String,
    }
    let lock: Lock = serde_json::from_slice(&std::fs::read(local_lock(app)?).ok()?).ok()?;
    tessera_client::probe(std::slice::from_ref(&lock.url), &BTreeMap::new())
        .await
        .ok()
}

#[cfg(desktop)]
fn is_local_endpoint(e: &str) -> bool {
    tessera_client::is_loopback_endpoint(e)
}

#[cfg(desktop)]
#[tauri::command]
async fn local_server_detect(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LocalServer, String> {
    let url = local_running(&app).await;
    let saved = state
        .config
        .lock()
        .servers
        .iter()
        .any(|s| s.endpoints.iter().any(|e| is_local_endpoint(e)));
    Ok(LocalServer {
        available: tessera_binary().is_some(),
        running: url.is_some(),
        url,
        saved,
    })
}

/// `tessera service install`: it waits up to a minute for the server itself.
#[cfg(desktop)]
const SERVICE_INSTALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);
#[cfg(desktop)]
const STATUS_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(desktop)]
const DEVICE_TOKEN_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Run `tessera args` for at most `limit` (see [`cli_run`]).
#[cfg(desktop)]
async fn run_tessera(
    bin: &std::path::Path,
    args: &[&str],
    limit: std::time::Duration,
) -> Result<cli_run::Ran, cli_run::RunError> {
    let mut cmd = tokio::process::Command::new(bin);
    quiet(&mut cmd).args(args);
    cli_run::run(&mut cmd, None, limit).await
}

/// Why a `tessera` command didn't run to its end, in words.
#[cfg(desktop)]
fn ran_error(e: cli_run::RunError, what: &str) -> String {
    match e {
        cli_run::RunError::Start(e) => format!("Couldn't run the tessera program to {what}: {e}"),
        cli_run::RunError::TimedOut => format!("Tessera took too long to {what}."),
    }
}

/// One step of "Start Tessera here", for the setup screen while it runs.
#[cfg(desktop)]
#[derive(Clone, Serialize)]
struct StartStep {
    stage: &'static str,
    message: &'static str,
}

#[cfg(desktop)]
fn start_step(app: &AppHandle, stage: &'static str, message: &'static str) {
    let _ = app.emit(
        "tessera://local-server-progress",
        StartStep { stage, message },
    );
}

#[cfg(desktop)]
use local_login::Issued;

/// "Start Tessera here": one job at a time, which "Stop waiting" doesn't
/// cut short (see [`start_job`]).
#[cfg(desktop)]
static START_JOB: start_job::OneJob<Result<String, String>> = start_job::OneJob::new();
#[cfg(desktop)]
static START_STOPS: start_job::Stops = start_job::Stops::new();

#[cfg(desktop)]
const STOPPED_WAITING: &str = "Stopped waiting. Tessera may still be starting in the background: press “Try again” in a moment to connect to it.";

/// Stop waiting for "Start Tessera here" (the setup screen's button): the
/// attempt that began at `attempt`, and any before it.
#[cfg(desktop)]
#[tauri::command]
fn local_server_cancel(attempt: Option<u64>) {
    START_STOPS.stop(attempt.unwrap_or_else(start_job::now_ms));
}

/// Whether the server on this computer answers within `limit` (each look
/// itself can take a few seconds when nothing answers).
#[cfg(desktop)]
async fn wait_until_up(app: &AppHandle, limit: std::time::Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        if let Ok(Some(_)) = tokio::time::timeout(left, local_running(app)).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Set up Tessera on this computer in one step: start the server if it isn't
/// running (optionally as a login service), give this app its own device
/// token straight from the local store (no pairing code needed, nothing
/// crosses the network) and connect end-to-end encrypted.
#[cfg(desktop)]
#[tauri::command]
async fn local_server_start(
    app: AppHandle,
    autostart: bool,
    attempt: Option<u64>,
) -> Result<String, String> {
    let attempt = attempt.unwrap_or_else(start_job::now_ms);
    if START_STOPS.stopped(attempt) {
        return Err(STOPPED_WAITING.into());
    }
    let bin = tessera_binary().ok_or("The tessera program isn't installed on this computer.")?;
    // A start that is still under way (the last try, stopped waiting for)
    // is waited for, never begun a second time.
    let (mut job, joined) =
        START_JOB.join_or_begin(|| start_and_connect(app.clone(), bin, autostart));
    if joined {
        start_step(
            &app,
            "waiting",
            "Still starting from the last try, waiting for it…",
        );
    }
    match start_job::wait(&mut job, START_STOPS.until_stopped(attempt)).await {
        start_job::Waited::Done(result) => result,
        start_job::Waited::Stopped => Err(STOPPED_WAITING.into()),
        start_job::Waited::Lost => Err("Starting Tessera ended unexpectedly. Try again.".into()),
    }
}

/// Saving and retirement belong to the same job as issuance, not to its
/// watchers: Stop/retry cannot race two saves or abandon the login swap.
#[cfg(desktop)]
async fn start_and_connect(
    app: AppHandle,
    bin: std::path::PathBuf,
    autostart: bool,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    state
        .credentials
        .run(|| async {
            let issued = start_and_issue(app.clone(), bin.clone(), autostart).await?;
            if !is_local_endpoint(&issued.url) {
                // Only ever hand a token to this machine's own server.
                return Err("The local server isn't listening on this computer.".into());
            }
            let (id, retired) = local_login::save_then_retire(
                || {
                    let mut cfg = state.config.lock();
                    server_config::save_local(
                        &mut cfg,
                        SavedServer {
                            id: new_id(),
                            name: host_name(),
                            endpoints: vec![issued.url.clone()],
                            headers: BTreeMap::new(),
                            token: issued.token.clone(),
                            server_key: issued.server_key.clone(),
                            fingerprint: None,
                            device_name: "Desktop app".into(),
                            route: None,
                            route_rev: 0,
                            last_endpoint: None,
                        },
                        &state.file,
                    )
                },
                || async {
                    let args = ["device-token-retire", "--keep", &issued.device_id];
                    match run_tessera(&bin, &args, DEVICE_TOKEN_LIMIT).await {
                        Ok(out) if out.success => Ok(()),
                        // No output from a credential-related command reaches a
                        // log or the UI, even if an older binary emits it by mistake.
                        _ => Err(concat!(
                            "The new local login is saved, but Tessera couldn't confirm ",
                            "retiring its earlier logins; some may remain valid."
                        )
                        .into()),
                    }
                },
            )
            .await?;
            if let Err(why) = retired {
                tracing::warn!("{why}");
            }
            connect(&app, &state, &id).await?;
            Ok(id)
        })
        .await
}

/// This computer's name, for the app's key and its server entry.
#[cfg(desktop)]
fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

/// The job behind "Start Tessera here": the server running (started if it
/// isn't), and a key for this app. It runs to its end within its own time
/// limits, whether or not the setup screen still waits: a command it ran is
/// never cut off half-way by "Stop waiting".
#[cfg(desktop)]
async fn start_and_issue(
    app: AppHandle,
    bin: std::path::PathBuf,
    autostart: bool,
) -> Result<Issued, String> {
    start_step(
        &app,
        "checking",
        "Looking for a Tessera server on this computer…",
    );
    if local_running(&app).await.is_none() {
        // Something was started that will come up by itself: never a second.
        let mut started = false;
        if autostart {
            start_step(
                &app,
                "installing",
                if cfg!(windows) {
                    "Adding Tessera to the programs Windows starts when you sign in, and starting it…"
                } else {
                    "Setting Tessera to start when you log in, and starting it…"
                },
            );
            match run_tessera(&bin, &["service", "install"], SERVICE_INSTALL_LIMIT).await {
                Ok(ran) if ran.success => started = true,
                // Windows: the startup entry and its launcher are always
                // there, so a failed install means the server itself failed:
                // say why rather than start a second one beside a launcher
                // that may still be trying (unless it answers after all).
                // Elsewhere it is started the plain way below.
                Ok(ran) => {
                    if cfg!(windows) {
                        if local_running(&app).await.is_none() {
                            return Err(cli_error(&ran.stderr)
                                .unwrap_or_else(|| didnt_start(&app, "start")));
                        }
                        started = true;
                    }
                }
                // Past its own minute of waiting for the server: what it
                // started may still come up, so only wait for it below.
                Err(cli_run::RunError::TimedOut) => started = true,
                Err(e) => return Err(ran_error(e, "start the server")),
            }
        }
        if !started {
            start_step(&app, "starting", "Starting the server…");
            start_server(&app, &bin)?;
        }
        // A first start (a new database, an antivirus looking at a program
        // it hasn't seen) can take a while.
        start_step(
            &app,
            "waiting",
            "Waiting for the server to answer (a first start can take a minute)…",
        );
        if !wait_until_up(&app, std::time::Duration::from_secs(60)).await {
            return Err(didnt_start(&app, "start"));
        }
    }
    start_step(&app, "connecting", "Connecting this app to it…");
    let name = format!("{} (desktop app)", host_name());
    let args = ["device-token", "--keep-old", "--name", &name];
    let out = run_tessera(&bin, &args, DEVICE_TOKEN_LIMIT)
        .await
        .map_err(|e| ran_error(e, "give this app its key"))?;
    if !out.success {
        return Err("Tessera couldn't give this app its key. Update Tessera and try again.".into());
    }
    local_login::parse(&out.stdout)
}

/// "Keep Tessera running" (Settings → Devices → This computer): the server
/// on this computer starts whenever the user signs in (`tessera service`,
/// a startup program on Windows, a user service on Linux). What setup's
/// tick box chose, changed later.
#[cfg(desktop)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeepRunning {
    /// This computer can run it that way (Windows; Linux with systemd).
    #[serde(default)]
    supported: bool,
    /// It starts at sign-in.
    #[serde(default)]
    installed: bool,
    #[serde(default)]
    running: bool,
    /// A Linux system service: only root changes it.
    #[serde(default)]
    system: bool,
}

/// `tessera service status --json`; an older `tessera` without it: not supported.
#[cfg(desktop)]
async fn keep_running_state(bin: &std::path::Path) -> KeepRunning {
    run_tessera(bin, &["service", "status", "--json"], STATUS_LIMIT)
        .await
        .ok()
        .filter(|o| o.success)
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default()
}

/// The reason a `tessera` command gave (its last `Error: ` line).
#[cfg(desktop)]
fn cli_error(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let why = text
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("Error: "))
        .unwrap_or(text.trim())
        .trim();
    (!why.is_empty()).then(|| why.chars().take(600).collect())
}

#[cfg(desktop)]
#[tauri::command]
async fn local_server_keep_running() -> Result<KeepRunning, String> {
    Ok(match tessera_binary() {
        Some(bin) => keep_running_state(&bin).await,
        None => KeepRunning::default(),
    })
}

/// Turn "Keep Tessera running" on or off. On: the server becomes the
/// sign-in one (a server already running is stopped cleanly and started
/// that way; chats that were working carry on). Off: it leaves the
/// startup programs, and runs on until the computer restarts. Either way
/// this app reconnects once it answers again.
#[cfg(desktop)]
#[tauri::command]
async fn local_server_set_keep_running(
    app: AppHandle,
    state: State<'_, AppState>,
    on: bool,
) -> Result<KeepRunning, String> {
    let bin = tessera_binary().ok_or("The tessera program isn't installed on this computer.")?;
    let now = keep_running_state(&bin).await;
    if !now.supported {
        return Err("This computer can't start Tessera by itself when you sign in.".into());
    }
    if now.system {
        return Err("Tessera runs as a system service on this computer: change it in a terminal with `sudo tessera service install` or `sudo tessera service uninstall`.".into());
    }
    if on != now.installed {
        let args: &[&str] = match (on, cfg!(windows)) {
            (true, true) => &["service", "install", "--replace"],
            (true, false) => &["service", "install", "--replace", "--no-linger"],
            (false, _) => &["service", "uninstall"],
        };
        let out = match run_tessera(&bin, args, SERVICE_INSTALL_LIMIT).await {
            Ok(out) => Some(out),
            // Past its own minute of waiting: see whether the server answers.
            Err(cli_run::RunError::TimedOut) => None,
            Err(e) => return Err(ran_error(e, "change that")),
        };
        if let Some(out) = out.filter(|o| !o.success) {
            return Err(cli_error(&out.stderr).unwrap_or_else(|| didnt_start(&app, "start")));
        }
        // Leaving the startup programs stopped it too: on for now.
        if !on && local_running(&app).await.is_none() {
            start_server(&app, &bin)?;
        }
        if !wait_until_up(&app, std::time::Duration::from_secs(60)).await {
            return Err(didnt_start(&app, "come back"));
        }
        nudge(&state);
    }
    Ok(keep_running_state(&bin).await)
}

/// Restart the server on this computer, e.g. after changing where it
/// listens. A login service is reinstalled (which restarts it with the new
/// settings); a server this app started is stopped and started again.
#[cfg(desktop)]
#[tauri::command]
async fn local_server_restart(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let bin = tessera_binary().ok_or("The tessera program isn't installed on this computer.")?;
    let service = run_tessera(&bin, &["service", "status"], STATUS_LIMIT)
        .await
        .is_ok_and(|o| o.success);
    if service {
        match run_tessera(&bin, &["service", "install"], SERVICE_INSTALL_LIMIT).await {
            Ok(out) if !out.success => {
                return Err(cli_error(&out.stderr).unwrap_or_else(|| didnt_start(&app, "start")));
            }
            // Past its own minute of waiting: see below whether it answers.
            Ok(_) | Err(cli_run::RunError::TimedOut) => {}
            Err(e) => return Err(ran_error(e, "restart the server")),
        }
    } else {
        #[derive(Deserialize)]
        struct Lock {
            pid: u32,
        }
        let lock: Option<Lock> = local_lock(&app)
            .and_then(|l| std::fs::read(l).ok())
            .and_then(|b| serde_json::from_slice(&b).ok());
        // Windows: only a Tessera has the stop event this asks through.
        #[cfg(windows)]
        let stopped = lock.as_ref().is_some_and(|l| win::request_stop(l.pid));
        #[cfg(not(windows))]
        let stopped = match &lock {
            Some(Lock { pid }) if is_tessera_process(*pid, &bin) => {
                let _ = tokio::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status()
                    .await;
                true
            }
            _ => false,
        };
        if stopped {
            for _ in 0..40 {
                if local_running(&app).await.is_none() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
        start_server(&app, &bin)?;
    }
    if wait_until_up(&app, std::time::Duration::from_secs(60)).await {
        nudge(&state);
        return Ok(());
    }
    Err(didnt_start(&app, "come back"))
}

/// Why the server on this computer didn't start or come back, and where
/// to look: on Windows its log (nobody there has a terminal open), else
/// `tessera serve` in a terminal.
#[cfg(desktop)]
fn didnt_start(app: &AppHandle, what: &str) -> String {
    #[cfg(windows)]
    if let Some(dir) = local_lock(app).and_then(|l| l.parent().map(std::path::Path::to_path_buf)) {
        return format!(
            "The server didn't {what}. The end of its log says why: {}",
            dir.join("logs").join("tessera.log").display()
        );
    }
    let _ = app;
    format!("The server didn't {what}. Run `tessera serve` in a terminal to see why.")
}

/// Start the server on this computer for as long as it should run (not at
/// sign-in: that is `tessera service install`). Windows: through
/// `tessera-service.exe`, which starts it again after a crash; elsewhere,
/// or without it, detached from this app.
#[cfg(desktop)]
fn start_server(app: &AppHandle, bin: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    if let Some(log) =
        local_lock(app).and_then(|l| l.parent().map(|d| d.join("logs").join("tessera.log")))
        && let Some(started) = win::start_server_supervised(bin, &log)
    {
        return started;
    }
    spawn_detached(app, bin)
}

/// Only ever stop a process that really is this `tessera` program.
#[cfg(all(desktop, not(windows)))]
fn is_tessera_process(pid: u32, bin: &std::path::Path) -> bool {
    let named_tessera = |p: &std::path::Path| p.file_name().is_some_and(|n| n == "tessera");
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        // The server hides `/proc/<pid>/exe` from other processes (it holds
        // keys); its command line still says what it is: `tessera serve`.
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let mut args = cmdline.split(|b| *b == 0);
        let program = args.next().map(String::from_utf8_lossy);
        return program.is_some_and(|p| named_tessera(std::path::Path::new(p.as_ref())))
            && args.any(|a| a == b"serve");
    };
    let same = |a: &std::path::Path, b: &std::path::Path| {
        std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
    };
    same(&exe, bin) || named_tessera(&exe)
}

#[cfg(desktop)]
fn spawn_detached(app: &AppHandle, bin: &std::path::Path) -> Result<(), String> {
    let log = local_lock(app)
        .and_then(|l| l.parent().map(|d| d.join("server.log")))
        .and_then(|p| {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("serve").stdin(std::process::Stdio::null());
    match log.and_then(|f| f.try_clone().ok().map(|g| (f, g))) {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        None => {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }
    // Its own process group: it keeps running when the app is closed.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // Windows: the same, and no console window.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(win::CREATE_NO_WINDOW | win::CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't start the server: {e}"))
}

fn connect<'a>(
    app: &'a AppHandle,
    state: &'a AppState,
    id: &'a str,
) -> futures::future::BoxFuture<'a, Result<(), String>> {
    Box::pin(connect_retiring(app, state, id, None))
}

/// [`connect`]; `retire`: the login this device held before it paired
/// again, signed out on the server once the new one is connected.
async fn connect_retiring(
    app: &AppHandle,
    state: &AppState,
    id: &str,
    mut retire: Option<String>,
) -> Result<(), String> {
    let server = state
        .config
        .lock()
        .servers
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or("unknown server")?;
    let (_lifecycle, installed) = state
        .transition_connection(&app.state::<notify::Notifier>(), Some(&server))
        .await;
    let InstalledConnection {
        client,
        mut events,
        generation,
    } = installed.expect("connect installed a client");
    let app2 = app.clone();
    let sid = server.id.clone();
    let pinned = !server.server_key.is_empty();
    let conn = client.clone();
    let route_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));

    tauri::async_runtime::spawn(async move {
        let notifier = app2.state::<notify::Notifier>();
        loop {
            match events.recv().await {
                Ok(ClientEvent::Event(e)) => {
                    if !is_current_client(&app2.state::<AppState>(), &sid, &conn) {
                        break;
                    }
                    if matches!(e, tessera_proto::Event::ServerRouteChanged) {
                        tauri::async_runtime::spawn(follow_route(
                            app2.clone(),
                            sid.clone(),
                            conn.clone(),
                            route_lock.clone(),
                        ));
                    }
                    notifier.event(generation, &e);
                    let _ = app2.emit("tessera://event", e);
                }
                Ok(ClientEvent::Status(s)) => {
                    if !is_current_client(&app2.state::<AppState>(), &sid, &conn) {
                        break;
                    }
                    if s.state == tessera_client::ConnState::Connected {
                        remember_endpoint(&app2, &sid, &conn, s.endpoint.as_deref());
                    }
                    notifier
                        .link(generation, s.state == tessera_client::ConnState::Connected)
                        .await;
                    emit_status(&app2, &sid, s)
                }
                Ok(ClientEvent::Connected) => {
                    if !is_current_client(&app2.state::<AppState>(), &sid, &conn) {
                        break;
                    }
                    notifier.link(generation, true).await;
                    notify::resync(&app2, generation, &conn);
                    if !pinned && upgrade_local(&app2, &sid, &conn).await {
                        break; // replaced by an encrypted connection
                    }
                    if let Some(token) = retire.take()
                        && let Err(error) = conn.post(tessera_proto::Request::RetireLogin {
                            token: token.clone(),
                        })
                    {
                        retire = Some(token); // retry admission on a later connection, never silently lose it
                        let _ = app2.emit(
                            "tessera://event",
                            &tessera_proto::Event::Notice {
                                session_id: None,
                                level: tessera_proto::NoticeLevel::Error,
                                message: error.message,
                            },
                        );
                    }
                    tauri::async_runtime::spawn(follow_route(
                        app2.clone(),
                        sid.clone(),
                        conn.clone(),
                        route_lock.clone(),
                    ));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // A lag may have skipped the disconnect status entirely.
                    // Invalidate transport-bound work before a fresh hydration.
                    notifier.link(generation, false).await;
                    notifier
                        .link(
                            generation,
                            conn.status().state == tessera_client::ConnState::Connected,
                        )
                        .await;
                    notify::resync(&app2, generation, &conn);
                    let _ = app2.emit("tessera://event", tessera_proto::Event::Resync);
                }
                Err(_) => {
                    notifier.link(generation, false).await;
                    break;
                }
            }
        }
    });
    emit_status(app, &server.id, client.status());
    Ok(())
}

/// Keep a saved server up with how it says its devices reach it (its
/// remote address and Cloudflare Access key, set up or changed after this
/// device paired): asked after each connect and whenever the server says
/// it changed. Saved in place, used from the next connection on, and the
/// page told to read the list again (Settings → Connection), never reloaded.
async fn follow_route(
    app: AppHandle,
    server_id: String,
    client: Client,
    route_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    // Connect and route-change events can arrive together. Fetch in order so
    // an older answer cannot undo a newer key or address.
    let _guard = route_lock.lock().await;
    let Some(route) = client.server_route().await else {
        return;
    };
    let state = app.state::<AppState>();
    if !is_current_client(&state, &server_id, &client) {
        return;
    }
    let changed = {
        let mut cfg = state.config.lock();
        let Some(s) = cfg.servers.iter_mut().find(|s| s.id == server_id) else {
            return;
        };
        match tessera_client::merge_route(&s.endpoints, &s.headers, s.route.as_ref(), &route) {
            Some((endpoints, headers, known)) => {
                let visible_changed = endpoints != s.endpoints || headers != s.headers;
                if visible_changed {
                    s.route_rev = s.route_rev.wrapping_add(1);
                }
                s.endpoints = endpoints.clone();
                s.headers = headers.clone();
                s.route = Some(known);
                Some((endpoints, headers, visible_changed))
            }
            None => None,
        }
    };
    let Some((endpoints, headers, visible_changed)) = changed else {
        return;
    };
    if state.save().is_ok() {
        client.set_route(endpoints, headers);
        // Learning only ownership metadata doesn't change what Settings shows.
        if visible_changed {
            let _ = app.emit("tessera://servers", ());
        }
    }
}

fn is_current_client(state: &AppState, server_id: &str, client: &Client) -> bool {
    state
        .conn
        .lock()
        .as_ref()
        .is_some_and(|(id, c)| id == server_id && c.same_connection(client))
}

fn remember_endpoint(app: &AppHandle, server_id: &str, client: &Client, endpoint: Option<&str>) {
    let state = app.state::<AppState>();
    if !is_current_client(&state, server_id, client) {
        return;
    }
    {
        let mut cfg = state.config.lock();
        let Some(s) = cfg.servers.iter_mut().find(|s| s.id == server_id) else {
            return;
        };
        if s.last_endpoint.as_deref() == endpoint {
            return;
        }
        s.last_endpoint = endpoint.map(str::to_string);
    }
    let _ = state.save();
}

/// A server paired before encryption, reached on this same machine: nothing
/// can sit in between, so learn its key over the authenticated connection,
/// pin it and reconnect encrypted. Never done for other hosts.
async fn upgrade_local(app: &AppHandle, server_id: &str, client: &Client) -> bool {
    let status = client.status();
    if status.encrypted
        || !status
            .endpoint
            .as_deref()
            .is_some_and(tessera_client::is_loopback_endpoint)
    {
        return false;
    }
    let state = app.state::<AppState>();
    state
        .credentials
        .run(|| async {
            let Ok(info) = client
                .call_as::<tessera_proto::ServerKeyInfo>(tessera_proto::Request::ServerKey)
                .await
            else {
                return false;
            };
            if !is_current_client(&state, server_id, client) {
                return false;
            }
            {
                let mut cfg = state.config.lock();
                let mut next = cfg.clone();
                let Some(s) = next
                    .servers
                    .iter_mut()
                    .find(|s| s.id == server_id && s.server_key.is_empty())
                else {
                    return false;
                };
                s.server_key = info.key;
                if server_config::commit(&mut cfg, next, &state.file).is_err() {
                    return false;
                }
            }
            connect(app, &state, server_id).await.is_ok()
        })
        .await
}

fn emit_status(app: &AppHandle, server_id: &str, s: ConnectionStatus) {
    let _ = app.emit(
        "tessera://status",
        StatusPayload {
            state: s.state,
            server_id: Some(server_id.to_string()),
            endpoint: s.endpoint,
            error: s.error,
            encrypted: s.encrypted,
        },
    );
}

/// WebKitGTK's DMA-BUF renderer crashes on NVIDIA + Wayland with
/// "Error 71 (Protocol error) dispatching to Wayland display". Fall back to
/// the shared-memory renderer there unless the user chose otherwise.
#[cfg(target_os = "linux")]
fn webkit_workarounds() {
    let nvidia = std::path::Path::new("/proc/driver/nvidia/version").exists();
    let user_set = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some();
    let forced_on = std::env::var("TESSERA_WEBKIT_DMABUF").is_ok_and(|v| v == "1");
    if nvidia && !user_set && !forced_on {
        // SAFETY: called first thing in `run`, before any other thread exists.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
}

/// Voice input: WebKitGTK ships with media capture off and no permission UI.
/// Turn capture on and grant microphone-only requests from our own pages
/// (camera or any other origin is denied).
#[cfg(target_os = "linux")]
fn enable_voice_input(window: &tauri::WebviewWindow) {
    let _ = window.with_webview(|pw| {
        use webkit2gtk::glib::prelude::Cast;
        use webkit2gtk::{
            PermissionRequestExt, SettingsExt, UserMediaPermissionRequest,
            UserMediaPermissionRequestExt, WebViewExt,
        };
        let wv = pw.inner();
        if let Some(settings) = WebViewExt::settings(&wv) {
            settings.set_enable_media_stream(true);
        }
        wv.connect_permission_request(|wv, req| {
            let Some(media) = req.downcast_ref::<UserMediaPermissionRequest>() else {
                return false; // not ours to decide: WebKit's default
            };
            let own = wv
                .uri()
                .and_then(|u| tauri::Url::parse(u.as_str()).ok())
                .is_some_and(|u| is_own_url(&u));
            if own && media.is_for_audio_device() && !media.is_for_video_device() {
                req.allow();
            } else {
                req.deny();
            }
            true
        });
    });
}

/// The app's own pages: the bundled UI, or the dev server in debug builds.
/// Compared by origin: `http://tauri.localhost.evil.example` is not ours.
fn is_own_url(url: &tauri::Url) -> bool {
    let host = url.host_str().unwrap_or("");
    match url.scheme() {
        "tauri" => host == "localhost",
        "http" | "https" => {
            (host == "tauri.localhost" && url.port().is_none())
                || (cfg!(debug_assertions)
                    && matches!(host, "localhost" | "127.0.0.1")
                    && url.port() == Some(5173))
        }
        _ => false,
    }
}

/// The app and its board routes only, checked before a navigation sends any
/// request (including subframes). Trusted UI link handlers use the scoped
/// opener command themselves. Never turn a refused script navigation into
/// a request in the system browser.
fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            is_own_url(url)
                || url.as_str() == "about:blank"
                || boards::is_frame_url(webview.app_handle(), url)
        })
        .build()
}

#[cfg(desktop)]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        voice::voice_status,
        voice::voice_listen,
        voice::voice_finish,
        voice::voice_cancel,
        voice::voice_say,
        voice::voice_quiet,
        servers_get,
        servers_save,
        server_pair,
        server_pair_link,
        server_connect,
        server_disconnect,
        rpc,
        app_resumed,
        device_report,
        local_server_detect,
        app_updates::app_update_available,
        app_updates::app_update_check,
        app_updates::app_update_install,
        local_server_start,
        local_server_cancel,
        local_server_restart,
        local_server_recovery,
        local_server_resume,
        local_server_keep_running,
        local_server_set_keep_running,
        machine::this_computer_status,
        machine::this_computer_enable,
        machine::this_computer_scope,
        machine::this_computer_service,
        machine::this_computer_log,
        machine::this_computer_list_dir,
        machine::this_computer_home,
        machine::this_computer_preset,
        machine::this_computer_access,
        machine::this_computer_startup_changes,
        machine::this_computer_restore,
        machine::this_computer_name,
        machine::this_computer_stop,
        machine::this_computer_guard_install,
        machine::this_computer_admin_password,
        notify::notifications_get,
        notify::notifications_set,
        notify::notifications_request_permission,
        notify::notifications_test,
        notify::notifications_seen,
        notify::notifications_take_opened,
        files::file_download,
        files::file_open,
        files::file_save,
        files::file_open_with,
        files::file_window,
        files::file_window_take,
        boards::board_frame_url,
        boards::board_local_url,
        boards::preview_frame_url,
        capture::board_capture,
        clipboard::clipboard_image,
        clipboard::clipboard_files,
        clipboard::dropped_files
    ]
}

#[cfg(mobile)]
fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        voice::voice_status,
        voice::voice_listen,
        voice::voice_finish,
        voice::voice_cancel,
        voice::voice_say,
        voice::voice_quiet,
        servers_get,
        servers_save,
        server_pair,
        server_pair_link,
        server_connect,
        server_disconnect,
        rpc,
        app_resumed,
        device_report,
        notify::notifications_get,
        notify::notifications_set,
        notify::notifications_request_permission,
        notify::notifications_test,
        notify::notifications_seen,
        notify::notifications_take_opened,
        files::file_download,
        files::file_open,
        files::file_save,
        files::file_window_take,
        notify::notifications_open_settings,
        notify::battery_optimization_request,
        boards::board_frame_url,
        boards::board_local_url,
        boards::preview_frame_url,
        capture::board_capture
    ]
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    webkit_workarounds();
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(navigation_guard());
    // Phones pair by scanning the QR code from `tessera pair`; notifications
    // and the keep-alive service are our own plugin there.
    #[cfg(mobile)]
    let builder = builder
        .plugin(tauri_plugin_barcode_scanner::init())
        .plugin(tessera_background::init());
    // Linux notifications go over D-Bus directly (see notify/xdg.rs).
    #[cfg(all(desktop, not(target_os = "linux")))]
    let builder = builder.plugin(tauri_plugin_notification::init());
    // The node this app runs while no background service runs it.
    #[cfg(desktop)]
    let builder = builder.manage(machine::NodeRunner::default());
    builder
        .manage(boards::Proxies::default())
        .manage(boards::Frames::default())
        .manage(files::Saved::default())
        .manage(files::Views::default())
        .manage(boards::Previews::default())
        .register_asynchronous_uri_scheme_protocol(boards::SCHEME, |ctx, request, responder| {
            boards::protocol(ctx.app_handle().clone(), request, responder)
        })
        .setup(|app| {
            // `TESSERA_APP_DATA` (desktop) starts from a separate profile,
            // e.g. to walk through the first run without touching yours.
            let custom = if cfg!(desktop) {
                std::env::var_os("TESSERA_APP_DATA").map(PathBuf::from)
            } else {
                None
            };
            let dir = match custom {
                Some(dir) => dir,
                None => app.path().app_data_dir()?,
            };
            // Not on mobile: Android's separate background check must still
            // read servers.json and write its own notification-state file.
            #[cfg(desktop)]
            app.manage(profile_owner::Owner::acquire(&dir)?);
            let file = dir.join("servers.json");
            let config: ClientConfig = std::fs::read(&file)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();

            let active = config.active.clone();
            let (notifier, actions) = notify::load(dir.join("notifications.json"));
            app.manage(notifier);
            notify::start(app.handle(), actions);
            app.manage(AppState {
                file,
                config: Mutex::new(config),
                conn: Mutex::new(None),
                lifecycle: tokio::sync::Mutex::new(()),
                credentials: credential_write::CredentialWrites::default(),
            });
            #[cfg(target_os = "linux")]
            if let Some(w) = app.get_webview_window("main") {
                enable_voice_input(&w);
                clipboard::watch_drops(&w);
            }
            if let Some(id) = active {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = connect(&handle, &handle.state::<AppState>(), &id).await;
                });
            }
            // This computer is on as a machine: run its node while the app is open.
            #[cfg(desktop)]
            tauri::async_runtime::spawn(machine::ensure_running(app.handle().clone()));
            // This app's server is the one on this computer, and it isn't running.
            #[cfg(windows)]
            tauri::async_runtime::spawn(resume_local_server(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(handlers())
        .build(tauri::generate_context!())
        .expect("error while running Tessera")
        .run(|app, event| match event {
            // Phones freeze the app in the background; the socket is often
            // dead when it comes back. Check it the moment we resume.
            // (Desktop platforms also send Resumed once at startup, before
            // setup has run: hence `try_state`.)
            tauri::RunEvent::Resumed => {
                if let Some(state) = app.try_state::<AppState>() {
                    nudge(&state);
                }
            }
            tauri::RunEvent::WindowEvent { label, event, .. } if label == "main" => {
                window_event(app, event)
            }
            // A text file's own window (files::file_window) closed: its text goes.
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Destroyed,
                ..
            } if label.starts_with("file-") => files::window_closed(app, &label),
            #[cfg(desktop)]
            tauri::RunEvent::Exit => machine::shutdown(app),
            _ => {}
        });
}

/// Notifications are only posted while the app isn't in front: window focus
/// on desktop, the activity's onPause/onResume on Android.
fn window_event(app: &AppHandle, event: tauri::WindowEvent) {
    // Window events can arrive before setup has managed the state.
    let Some(notifier) = app.try_state::<notify::Notifier>() else {
        return;
    };
    match event {
        #[cfg(desktop)]
        tauri::WindowEvent::Focused(focused) => notifier.foreground(focused),
        // Its text files' windows close with it.
        #[cfg(desktop)]
        tauri::WindowEvent::Destroyed => files::close_windows(app),
        #[cfg(mobile)]
        tauri::WindowEvent::Suspended => notifier.foreground(false),
        #[cfg(mobile)]
        tauri::WindowEvent::Resumed => {
            // On Android this is the activity's onResume; RunEvent::Resumed
            // above doesn't fire there.
            if let Some(state) = app.try_state::<AppState>() {
                nudge(&state);
            }
            notifier.foreground(true);
            notify::resumed(app);
            if notifier.take_paused() {
                let state = app.state::<AppState>();
                let active = state.config.lock().active.clone();
                if let Some(id) = active {
                    let handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = connect(&handle, &handle.state::<AppState>(), &id).await;
                    });
                }
            }
        }
        #[cfg(mobile)]
        tauri::WindowEvent::Destroyed => notifier.activity_gone(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This computer's server key comes from the tessera program's
    /// `device-token --key-only`; a program that can't say (an older one, no
    /// server yet) gives none, so the app starts nothing.
    #[cfg(all(desktop, unix))]
    #[tokio::test]
    async fn this_computers_key_comes_from_the_tessera_program() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tessera-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = |name: &str, script: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let new = program(
            "new",
            r#"[ "$1 $2" = "device-token --key-only" ] && echo '{"server_key":"S3rv3rK3y"}' || exit 2"#,
        );
        let old = program(
            "old",
            "echo 'error: unexpected argument --key-only' >&2; exit 2",
        );
        let none = program("none", "exit 3");
        assert_eq!(this_computers_key(&new).await.as_deref(), Some("S3rv3rK3y"));
        assert_eq!(this_computers_key(&old).await, None);
        assert_eq!(this_computers_key(&none).await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_apps_own_origin_counts_as_ours() {
        for u in ["tauri://localhost/", "http://tauri.localhost/index.html"] {
            assert!(is_own_url(&tauri::Url::parse(u).unwrap()), "{u}");
        }
        for u in [
            "http://tauri.localhost.evil.example/",
            "tauri://localhost.evil.example/",
            "https://evil.example/",
            "http://tauri.localhost:8080/",
            "file:///etc/passwd",
            "data:text/html,hi",
        ] {
            assert!(!is_own_url(&tauri::Url::parse(u).unwrap()), "{u}");
        }
    }

    #[test]
    fn the_ui_cannot_redirect_a_token() {
        let server = |id: &str, endpoint: &str, key: &str| SavedServer {
            id: id.into(),
            endpoints: vec![endpoint.into()],
            token: "tsr_secret".into(),
            server_key: key.into(),
            ..Default::default()
        };
        let cur = ClientConfig {
            servers: vec![
                server("a", "https://home", "KEY"),
                server("b", "https://old", ""),
            ],
            active: None,
            revision: 0,
        };
        let mut next = redacted(&cur);
        for s in &mut next.servers {
            s.endpoints = vec!["https://elsewhere".into()];
        }
        next.servers.push(server("c", "https://x", "FAKE"));
        let saved = with_stored_secrets(&cur, next);
        // Pinned: the token stays (it only goes to the key's owner).
        assert_eq!(
            (
                saved.servers[0].token.as_str(),
                saved.servers[0].server_key.as_str()
            ),
            ("tsr_secret", "KEY")
        );
        // Unpinned and moved: the token is dropped.
        assert!(saved.servers[1].token.is_empty());
        // Invented by the UI: no secrets, whatever it claims.
        assert!(saved.servers[2].token.is_empty() && saved.servers[2].server_key.is_empty());
    }

    /// The server gave new addresses (`follow_route`) while the page held
    /// the list it read before: saving that list (a rename, another server
    /// made active) doesn't put the old addresses back.
    #[test]
    fn a_stale_copy_from_the_ui_keeps_the_servers_new_route() {
        let stored = SavedServer {
            id: "a".into(),
            endpoints: vec!["http://lan:7878".into(), "https://t.example.com".into()],
            headers: BTreeMap::from([("CF-Access-Client-Id".into(), "id".into())]),
            token: "tsr_secret".into(),
            server_key: "KEY".into(),
            route: Some(tessera_client::KnownRoute::default()),
            route_rev: 1,
            ..Default::default()
        };
        let cur = ClientConfig {
            servers: vec![stored.clone()],
            active: None,
            revision: 0,
        };
        let mut stale = redacted(&cur);
        stale.servers[0].endpoints = vec!["http://lan:7878".into()];
        stale.servers[0].headers = BTreeMap::new();
        stale.servers[0].route_rev = 0;
        stale.servers[0].device_name = "Phone".into();
        let saved = with_stored_secrets(&cur, stale);
        let s = &saved.servers[0];
        assert_eq!(s.endpoints, stored.endpoints);
        assert_eq!(s.headers, stored.headers);
        assert_eq!((s.route_rev, s.device_name.as_str()), (1, "Phone"));
        assert!(s.route.is_some() && s.token == "tsr_secret");
        // A current copy may change them (Settings → Connection → Edit).
        let mut edit = redacted(&cur);
        edit.servers[0].endpoints = vec!["https://mine.example.com".into()];
        let saved = with_stored_secrets(&cur, edit);
        assert_eq!(saved.servers[0].endpoints, ["https://mine.example.com"]);
        // The page never sees what the server said.
        assert!(redacted(&cur).servers[0].route.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn saved_servers_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tessera-app-{}", std::process::id()));
        let file = dir.join("app/servers.json");
        write_private(&file, b"{}").unwrap();
        write_private(&file, b"{\"servers\":[]}").unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
        assert_eq!(std::fs::read(&file).unwrap(), b"{\"servers\":[]}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
