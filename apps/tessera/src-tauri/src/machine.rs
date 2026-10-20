//! Settings → Devices → This computer (the desktop app only): this computer
//! as one of the user's machines (docs/MACHINES.md): on or off, what it
//! allows (a preset, or Advanced folder rules), who can use it, desktop
//! control, and putting back startup files agents changed.
//!
//! Everything here runs `tessera node …` on this computer, as the user. None
//! of it is a server request, so no other device can turn this computer on or
//! widen what it allows: the server can only pause or revoke it. The page
//! confirms widening choices first; the CLI checks its own rules (folders
//! must exist, never Tessera's own data, …) and the node enforces its scope
//! itself.
//!
//! Only the app's own page reaches these commands: Tauri refuses app commands
//! from any other origin (no capability has a `remote` scope), its IPC bridge
//! is injected into the main frame only (whiteboards are sandboxed frames
//! without it), and each command checks again that it comes from the main
//! window showing the app.
//!
//! While no background service runs the node, this app runs `tessera node
//! run` itself, for as long as the app is open (it dies with the app).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Manager, Webview};
use tessera_proto::{
    DirEntry, MACHINE_TIMEOUT_MAX, MachineAsk, MachinePreset, MachineScope, NodeLogEntry,
    NodeStatus, StartupChange,
};

use crate::{AppState, is_own_url, tessera_binary};

const NOT_INSTALLED: &str = "The tessera program isn't installed on this computer.";
const TOO_OLD: &str =
    "The tessera program on this computer is too old for this. Update Tessera here.";

/// What Settings → Devices → This computer shows.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThisComputer {
    /// `None`: the `tessera` program isn't installed here.
    node: Option<NodeStatus>,
    /// The server this app uses (its name). Turning on without a `tessera
    /// connect` login gives the node this app's pairing.
    app_server: Option<String>,
    /// This app's server runs on this computer.
    server_here: bool,
}

/// The pairing handed to `tessera node enable --server-stdin`.
#[derive(Serialize)]
struct Pairing<'a> {
    name: &'a str,
    endpoints: &'a [String],
    headers: &'a std::collections::BTreeMap<String, String>,
    token: &'a str,
    server_key: &'a str,
}

/// Commands come from the main window showing the app, nothing else.
fn from_the_app(webview: &Webview) -> Result<(), String> {
    let own = webview.window().label() == "main" && webview.url().is_ok_and(|u| is_own_url(&u));
    if own {
        Ok(())
    } else {
        Err("Only the Tessera app itself can change this.".into())
    }
}

/// The CLI's failure in plain words: its `{"error": …}`, else its last words.
fn failure(sub: &str, stdout: &[u8], stderr: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct Failed {
        error: String,
    }
    if let Ok(f) = serde_json::from_slice::<Failed>(stdout)
        && !f.error.trim().is_empty()
    {
        return f.error.trim().to_string();
    }
    let err = String::from_utf8_lossy(stderr);
    if err.contains("unrecognized subcommand") {
        return TOO_OLD.into();
    }
    let msg = err
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let msg = msg.strip_prefix("Error: ").unwrap_or(&msg).trim();
    if msg.is_empty() {
        format!("`tessera node {sub}` failed.")
    } else {
        msg.chars().take(600).collect()
    }
}

/// `tessera node <args>` on this computer; its stdout on success.
async fn node_cli(args: &[&str], stdin: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    // A Resume with Tessera Guard (Windows) waits for the user to answer
    // Windows' administrator prompt.
    let limit = Duration::from_secs(if args.first() == Some(&"resume") {
        600
    } else {
        60
    });
    tessera_cli("node", args, stdin, limit).await
}

/// `tessera <group> <args>`, waited for at most `limit`.
async fn tessera_cli(
    group: &str,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let bin = tessera_binary().ok_or(NOT_INSTALLED)?;
    let mut cmd = tokio::process::Command::new(bin);
    #[cfg(windows)]
    crate::win::quiet(&mut cmd);
    cmd.arg(group).args(args);
    // Its end counts, not its output's: `node service install` starts the
    // machine, which outlives it (see `cli_run`).
    let out = crate::cli_run::run(&mut cmd, stdin, limit)
        .await
        .map_err(|e| match e {
            crate::cli_run::RunError::TimedOut => {
                "Tessera on this computer didn't answer in time.".to_string()
            }
            crate::cli_run::RunError::Start(e) => format!("couldn't run tessera: {e}"),
        })?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(failure(args[0], &out.stdout, &out.stderr))
    }
}

/// `tessera node status`; `None` without the `tessera` program.
async fn status() -> Result<Option<NodeStatus>, String> {
    if tessera_binary().is_none() {
        return Ok(None);
    }
    let out = node_cli(&["status", "--json"], None).await?;
    serde_json::from_slice(&out)
        .map(Some)
        .map_err(|e| format!("Unexpected answer from `tessera node status`: {e}"))
}

async fn this_computer(app: &AppHandle) -> Result<ThisComputer, String> {
    let node = status().await?;
    let (app_server, local) = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock();
        let active = cfg
            .active
            .as_ref()
            .and_then(|id| cfg.servers.iter().find(|s| &s.id == id))
            .filter(|s| !s.token.is_empty());
        (
            active.map(|s| s.name.clone()),
            active.is_some_and(|s| {
                !s.endpoints.is_empty()
                    && s.endpoints
                        .iter()
                        .all(|e| tessera_client::is_loopback_endpoint(e))
            }),
        )
    };
    // Loopback could be a tunnel to elsewhere: the server's own lock file says it runs here.
    let server_here = local && crate::local_running(app).await.is_some();
    Ok(ThisComputer {
        node,
        app_server,
        server_here,
    })
}

/// Scopes from the page are checked here too (the CLI and the node check
/// again): full paths, no duplicates, a time limit in range.
fn check_scope(scope: &MachineScope) -> Result<(), String> {
    if scope.timeout_secs == 0 || scope.timeout_secs > MACHINE_TIMEOUT_MAX {
        return Err(format!(
            "The time limit must be between 1 second and {} hours.",
            MACHINE_TIMEOUT_MAX / 3600
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for f in &scope.folders {
        let p = Path::new(&f.path);
        if f.path.contains('\0') || !p.is_absolute() {
            return Err(format!("`{}` is not a full path.", f.path));
        }
        if !seen.insert(p.components().collect::<PathBuf>()) {
            return Err(format!("`{}` is listed twice.", f.path));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn this_computer_status(
    app: AppHandle,
    webview: Webview,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    let info = this_computer(&app).await?;
    if info.node.as_ref().is_some_and(should_run) && start(&app).is_ok() {
        return this_computer(&app).await;
    }
    Ok(info)
}

/// Turn this computer on or off as a machine. Without a `tessera connect`
/// login here, the node gets this app's own pairing over stdin (it never
/// passes through the page).
#[tauri::command]
pub async fn this_computer_enable(
    app: AppHandle,
    webview: Webview,
    on: bool,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    if on {
        let cur = status().await?.ok_or(NOT_INSTALLED)?;
        // This app's own pairing whenever it has one (end-to-end encrypted): the
        // machine is then this app's device, one entry under Devices. Else the
        // terminal's (`tessera connect`) login.
        let app_pairing = {
            let state = app.state::<AppState>();
            let cfg = state.config.lock();
            cfg.active
                .as_ref()
                .and_then(|id| cfg.servers.iter().find(|s| &s.id == id))
                .filter(|s| !s.token.is_empty() && !s.server_key.is_empty())
                .map(|s| {
                    serde_json::to_vec(&Pairing {
                        name: &s.name,
                        endpoints: &s.endpoints,
                        headers: &s.headers,
                        token: &s.token,
                        server_key: &s.server_key,
                    })
                })
                .transpose()
                .map_err(|e| e.to_string())?
        };
        if app_pairing.is_none() && !cur.paired {
            return Err("Connect this app to your server first.".into());
        }
        let pairing = app_pairing;
        let mut args = vec!["enable", "--yes", "--json"];
        if pairing.is_some() {
            args.push("--server-stdin");
        }
        node_cli(&args, pairing).await?;
        if status().await?.as_ref().is_some_and(should_run) {
            start(&app)?;
        }
    } else {
        // Stops a running node at once (its running actions are killed).
        node_cli(&["disable", "--json"], None).await?;
        stop(&app).await;
    }
    this_computer(&app).await
}

/// Set what agents may do here (the whole scope; the page confirmed widening).
#[tauri::command]
pub async fn this_computer_scope(
    app: AppHandle,
    webview: Webview,
    scope: MachineScope,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    check_scope(&scope)?;
    let set = format!(
        "--set={}",
        serde_json::to_string(&scope).map_err(|e| e.to_string())?
    );
    node_cli(&["scope", &set, "--yes", "--json"], None).await?;
    this_computer(&app).await
}

/// The CLI's word for a preset.
fn preset_word(p: MachinePreset) -> Option<&'static str> {
    match p {
        MachinePreset::LookOnly => Some("look"),
        MachinePreset::WorkInFolders => Some("work"),
        MachinePreset::FullAccess => Some("full"),
        MachinePreset::DesktopControl => Some("desktop"),
        MachinePreset::Custom => None,
    }
}

fn ask_word(a: MachineAsk) -> &'static str {
    match a {
        MachineAsk::Always => "always",
        MachineAsk::Risky => "risky",
        MachineAsk::Chat => "chat",
    }
}

/// Choose a preset (the page showed what it allows, and confirmed giving
/// more; desktop control with its own warning). `folders`: for "Look only"
/// and "Work in folders". `ask` ("Ask first") and `webviews` ("Web views
/// here") change too, or alone without a preset.
#[tauri::command]
pub async fn this_computer_preset(
    app: AppHandle,
    webview: Webview,
    preset: Option<MachinePreset>,
    folders: Option<Vec<String>>,
    ask: Option<MachineAsk>,
    webviews: Option<bool>,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    let mut args: Vec<String> = vec!["preset".into()];
    if let Some(p) = preset {
        args.push(
            preset_word(p)
                .ok_or("Advanced has no fixed settings: change them one by one.")?
                .into(),
        );
    }
    for f in folders.unwrap_or_default() {
        let p = Path::new(&f);
        if f.contains('\0') || !p.is_absolute() {
            return Err(format!("`{f}` is not a full path."));
        }
        args.push(format!("--folder={f}"));
    }
    if let Some(a) = ask {
        args.push(format!("--ask={}", ask_word(a)));
    }
    if let Some(w) = webviews {
        args.push(format!("--webviews={}", if w { "on" } else { "off" }));
    }
    args.extend(["--yes".into(), "--json".into()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    node_cli(&refs, None).await?;
    this_computer(&app).await
}

/// Rename this computer as a machine (what agents and approvals call it).
/// Only here: no other device can rename a machine.
#[tauri::command]
pub async fn this_computer_name(
    app: AppHandle,
    webview: Webview,
    name: String,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err("A name is one line of 1 to 60 characters.".into());
    }
    node_cli(&["name", "--json", "--", name], None).await?;
    this_computer(&app).await
}

/// Stop every agent on this computer (`tessera node stop`, what the Stop
/// bar does), or resume after a Stop (`tessera node resume`): only this
/// computer can. Agents can't press it: their input to Tessera's own
/// windows is refused, and a stopped machine does nothing.
#[tauri::command]
pub async fn this_computer_stop(
    app: AppHandle,
    webview: Webview,
    stopped: bool,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    node_cli(&[if stopped { "stop" } else { "resume" }, "--json"], None).await?;
    this_computer(&app).await
}

/// Install Tessera Guard (Windows): Windows asks an administrator (UAC), and
/// this waits while the user answers.
#[tauri::command]
pub async fn this_computer_guard_install(
    app: AppHandle,
    webview: Webview,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    tessera_cli("guard", &["install"], None, Duration::from_secs(600)).await?;
    this_computer(&app).await
}

/// Save the password `sudo` asks for here, for administrator commands
/// (`tessera node admin password --stdin`: checked with sudo, kept on this
/// computer only), or forget it (`None`). It goes to the CLI on stdin, never
/// in its arguments.
#[tauri::command]
pub async fn this_computer_admin_password(
    app: AppHandle,
    webview: Webview,
    password: Option<String>,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    match password {
        Some(p) => {
            if p.is_empty() || p.contains(['\n', '\r', '\0']) {
                return Err("The password must be one line.".into());
            }
            node_cli(
                &["admin", "password", "--stdin", "--json"],
                Some(format!("{p}\n").into_bytes()),
            )
            .await?;
        }
        None => {
            node_cli(&["admin", "forget", "--json"], None).await?;
        }
    }
    this_computer(&app).await
}

/// Who can use this computer: any chat (`None`), or only these bots (ids).
#[tauri::command]
pub async fn this_computer_access(
    app: AppHandle,
    webview: Webview,
    bots: Option<Vec<String>>,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    let mut args: Vec<String> = vec!["access".into()];
    match bots {
        None => args.push("--any".into()),
        Some(list) if list.is_empty() => {
            return Err("Choose at least one bot, or let any chat use it.".into());
        }
        Some(list) => {
            for b in list {
                if b.trim().is_empty() || b.len() > 64 || b.contains('\0') {
                    return Err("That isn't a bot.".into());
                }
                args.push(format!("--bot={b}"));
            }
        }
    }
    args.extend(["--yes".into(), "--json".into()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    node_cli(&refs, None).await?;
    this_computer(&app).await
}

/// Startup files agents changed here (desktop control), newest first.
#[tauri::command]
pub async fn this_computer_startup_changes(
    webview: Webview,
    limit: u32,
) -> Result<Vec<StartupChange>, String> {
    from_the_app(&webview)?;
    let limit = limit.clamp(1, 500).to_string();
    let out = node_cli(&["restore", "--limit", &limit, "--json"], None).await?;
    serde_json::from_slice(&out)
        .map_err(|e| format!("Unexpected answer from `tessera node restore`: {e}"))
}

/// Put a startup file back as it was before change `id`.
#[tauri::command]
pub async fn this_computer_restore(webview: Webview, id: u64) -> Result<StartupChange, String> {
    from_the_app(&webview)?;
    let id = id.to_string();
    let out = node_cli(&["restore", &id, "--json"], None).await?;
    serde_json::from_slice(&out)
        .map_err(|e| format!("Unexpected answer from `tessera node restore`: {e}"))
}

/// "Keep it on in the background": the node's user service. While it runs,
/// this app doesn't run a node of its own.
#[tauri::command]
pub async fn this_computer_service(
    app: AppHandle,
    webview: Webview,
    on: bool,
) -> Result<ThisComputer, String> {
    from_the_app(&webview)?;
    if on {
        stop(&app).await;
        let r = node_cli(&["service", "install", "--no-linger", "--json"], None).await;
        if let Err(e) = r {
            if status().await?.as_ref().is_some_and(should_run) {
                let _ = start(&app);
            }
            return Err(e);
        }
    } else {
        node_cli(&["service", "uninstall", "--json"], None).await?;
        if status().await?.as_ref().is_some_and(should_run) {
            start(&app)?;
        }
    }
    this_computer(&app).await
}

/// This computer's log of what agents did here, newest first.
#[tauri::command]
pub async fn this_computer_log(webview: Webview, limit: u32) -> Result<Vec<NodeLogEntry>, String> {
    from_the_app(&webview)?;
    let limit = limit.clamp(1, 1000).to_string();
    let out = node_cli(&["log", "--limit", &limit, "--json"], None).await?;
    serde_json::from_slice(&out)
        .map_err(|e| format!("Unexpected answer from `tessera node log`: {e}"))
}

/// Folders on this computer, for the folder picker.
#[tauri::command]
pub async fn this_computer_list_dir(
    webview: Webview,
    path: String,
) -> Result<Vec<DirEntry>, String> {
    from_the_app(&webview)?;
    let p = PathBuf::from(&path);
    if path.contains('\0') || !p.is_absolute() {
        return Err(if cfg!(windows) {
            r"Enter a full path, like C:\Users\you\projects.".into()
        } else {
            "Enter a full path, like /home/you/projects.".into()
        });
    }
    tokio::task::spawn_blocking(move || list_dir(&p))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn this_computer_home(app: AppHandle, webview: Webview) -> Result<String, String> {
    from_the_app(&webview)?;
    app.path()
        .home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

fn list_dir(dir: &Path) -> Result<Vec<DirEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} doesn't exist.", dir.display()),
        std::io::ErrorKind::PermissionDenied => format!("You can't open {}.", dir.display()),
        _ => format!("Can't open {}: {e}", dir.display()),
    })?;
    let mut out: Vec<DirEntry> = rd
        .flatten()
        .take(5000)
        .map(|e| {
            let path = e.path();
            DirEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                is_dir: std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
                path: path.to_string_lossy().into_owned(),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

// ── the node this app runs ───────────────────────────────────────────────

/// The `tessera node run` this app started, while no service runs one.
#[derive(Default)]
pub struct NodeRunner(Mutex<Runner>);

#[derive(Default)]
struct Runner {
    pid: Option<u32>,
    /// This app stopped it (don't start it again).
    stopping: bool,
    /// Failed starts in a row.
    failures: u32,
}

/// On here, and nothing runs it yet.
fn should_run(s: &NodeStatus) -> bool {
    s.enabled && !s.revoked && !s.running && !s.service.active
}

/// The app started (or the page looked): run the node if it should be on.
pub async fn ensure_running(app: AppHandle) {
    if let Ok(Some(s)) = status().await
        && should_run(&s)
    {
        let _ = start(&app);
    }
}

fn start(app: &AppHandle) -> Result<(), String> {
    let runner = app.state::<NodeRunner>();
    let mut st = runner.0.lock();
    if st.pid.is_some() {
        return Ok(());
    }
    let bin = tessera_binary().ok_or(NOT_INSTALLED)?;
    let log = app
        .state::<AppState>()
        .file
        .parent()
        .map(|d| d.join("node-run.log"))
        .and_then(|p| std::fs::File::create(p).ok());
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["node", "run"]).stdin(Stdio::null());
    match log.and_then(|f| f.try_clone().ok().map(|g| (f, g))) {
        Some((out, err)) => cmd.stdout(out).stderr(err),
        None => cmd.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    // It goes when the app goes, even if the app crashes.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    // Windows: no console window; and it waits until it is tied to the app.
    #[cfg(windows)]
    let gate = crate::win::StartGate::new()?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::win::CREATE_NO_WINDOW);
        cmd.env("TESSERA_START_GATE", &gate.name);
    }
    // The thread that starts it waits for it: the death signal follows that
    // thread, which lives exactly as long as the node.
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    std::thread::Builder::new()
        .name("tessera-node".into())
        .spawn(move || {
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(format!("couldn't start the node: {e}")));
                    return;
                }
            };
            let pid = child.id();
            // Windows has no death signal: a Job Object ends it with the app;
            // it waits for that (the gate), and without it doesn't run at all.
            #[cfg(windows)]
            if !crate::win::end_with_app(pid) || !gate.open() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = tx.send(Err(
                    "couldn't start the node: it couldn't be tied to the app (a Windows job that ends it with the app), so it was ended".into(),
                ));
                return;
            }
            let _ = tx.send(Ok(pid));
            let since = Instant::now();
            let ok = child.wait().is_ok_and(|s| s.success());
            if exited(&app2, pid, ok, since.elapsed()) {
                std::thread::sleep(Duration::from_secs(10));
                tauri::async_runtime::spawn(ensure_running(app2));
            }
        })
        .map_err(|e| e.to_string())?;
    let pid = rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "couldn't start the node".to_string())??;
    st.pid = Some(pid);
    st.stopping = false;
    Ok(())
}

/// The node ended; whether to try again (it failed, not too often).
fn exited(app: &AppHandle, pid: u32, ok: bool, ran: Duration) -> bool {
    let runner = app.state::<NodeRunner>();
    let mut st = runner.0.lock();
    if st.pid == Some(pid) {
        st.pid = None;
    }
    if std::mem::take(&mut st.stopping) || ok {
        // Stopped here, or it turned itself off (disabled, revoked).
        st.failures = 0;
        return false;
    }
    if ran > Duration::from_secs(60) {
        st.failures = 0;
    }
    st.failures += 1;
    st.failures <= 5
}

/// Stop the node this app runs, if any (it gets a moment to finish).
pub async fn stop(app: &AppHandle) {
    let pid = {
        let runner = app.state::<NodeRunner>();
        let mut st = runner.0.lock();
        st.stopping = st.pid.is_some();
        st.pid
    };
    let Some(pid) = pid else { return };
    terminate(pid);
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if app.state::<NodeRunner>().0.lock().pid != Some(pid) {
            return;
        }
    }
}

/// The app is closing: the node it runs closes too.
pub fn shutdown(app: &AppHandle) {
    if let Some(runner) = app.try_state::<NodeRunner>() {
        let mut st = runner.0.lock();
        if let Some(pid) = st.pid {
            st.stopping = true;
            terminate(pid);
        }
    }
}

fn terminate(pid: u32) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: plain kill(2) of the child this app started and still waits for.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    // Windows: the stop event it waits on; if that fails, end it (a
    // windowless program ignores a plain `taskkill`).
    #[cfg(windows)]
    if !crate::win::request_stop(pid) {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(crate::win::CREATE_NO_WINDOW)
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_proto::FolderAccess;

    fn scope(folders: &[&str], timeout_secs: u64) -> MachineScope {
        MachineScope {
            folders: folders
                .iter()
                .map(|p| FolderAccess {
                    path: (*p).into(),
                    write: false,
                })
                .collect(),
            commands: false,
            ask: MachineAsk::Always,
            timeout_secs,
            ..Default::default()
        }
    }

    #[test]
    fn presets_and_asking_have_cli_words() {
        assert_eq!(preset_word(MachinePreset::WorkInFolders), Some("work"));
        assert_eq!(preset_word(MachinePreset::DesktopControl), Some("desktop"));
        assert_eq!(preset_word(MachinePreset::Custom), None);
        assert_eq!(ask_word(MachineAsk::Risky), "risky");
    }

    #[test]
    fn scopes_from_the_page_are_checked() {
        assert!(check_scope(&MachineScope::default()).is_ok());
        assert!(check_scope(&scope(&["/home/me/code", "/srv/data"], 900)).is_ok());
        for bad in [
            scope(&["code"], 300),
            scope(&["~/code"], 300),
            scope(&["/home/me/code", "/home/me/code/"], 300),
            scope(&["/home/me/\0x"], 300),
            scope(&[], 0),
            scope(&[], MACHINE_TIMEOUT_MAX + 1),
        ] {
            assert!(check_scope(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cli_failures_read_plainly() {
        assert_eq!(
            failure(
                "scope",
                br#"{"error":"/nope doesn't exist"}"#,
                b"Error: /nope doesn't exist\n"
            ),
            "/nope doesn't exist"
        );
        assert_eq!(
            failure("scope", b"", b"Error: the folder must exist\n"),
            "the folder must exist"
        );
        assert_eq!(
            failure(
                "status",
                b"",
                b"error: unrecognized subcommand 'node'\n\nUsage: tessera\n"
            ),
            TOO_OLD
        );
        assert_eq!(failure("log", b"", b""), "`tessera node log` failed.");
    }

    #[test]
    fn folders_list_directories_first() {
        let dir = std::env::temp_dir().join(format!("tessera-app-ls-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("b-dir")).unwrap();
        std::fs::create_dir_all(dir.join("A-dir")).unwrap();
        std::fs::write(dir.join("a-file"), b"x").unwrap();
        let l = list_dir(&dir).unwrap();
        let names: Vec<_> = l.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
        assert_eq!(names, [("A-dir", true), ("b-dir", true), ("a-file", false)]);
        assert!(l[0].path.ends_with("/A-dir"));
        assert!(
            list_dir(&dir.join("missing"))
                .unwrap_err()
                .contains("doesn't exist")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_node_runs_only_when_on_and_nothing_else_runs_it() {
        let mut s: NodeStatus = serde_json::from_value(serde_json::json!({
            "enabled": true, "revoked": false, "name": "devbox", "version": "0.1.0",
            "scope": { "folders": [], "commands": false, "ask": "always", "timeout_secs": 300 },
            "paired": true, "running": false, "connected": false, "paused": false, "calls": 0,
            "sandbox": "on", "sandbox_ok": true, "log_file": "/tmp/x", "preset": "look_only", "home": "/home/me",
            "service": { "available": true, "installed": false, "active": false, "enabled": false, "linger": false }
        }))
        .unwrap();
        assert!(should_run(&s));
        s.service.active = true;
        assert!(!should_run(&s), "the service runs it");
        s.service.active = false;
        s.running = true;
        assert!(!should_run(&s), "already running");
        s.running = false;
        s.revoked = true;
        assert!(!should_run(&s), "revoked from another device");
        s.revoked = false;
        s.enabled = false;
        assert!(!should_run(&s), "off");
    }
}
