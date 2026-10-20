//! `tessera service install|uninstall|status` on Windows: the server (and,
//! with `tessera node service`, the machine) runs in the background through
//! `tessera-service.exe`, which Windows starts whenever the user signs in.
//!
//! The entry lives in the user's own startup list
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`): no administrator
//! rights, and Task Manager → Startup apps shows it and can turn it off.
//! `tessera-service.exe` has no window, restarts Tessera when it fails, and
//! writes its output to `logs\<name>.log` in the data folder. Stopping asks
//! the server to stop cleanly (a named event it waits on), as SIGTERM does
//! on Linux, and ends it only if it doesn't.
//!
//! The same interface as `service.rs` (systemd), which the rest of the CLI
//! uses unchanged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tessera_core::config::{Config, Paths};
use tessera_core::platform::split_windows_args;
use tessera_core::platform::windows as win;

use crate::service_entry::{NODE_NAME, bind_of, command_line, entry_name, parse_entry};

use crate::setup::term::{S_ACCENT, S_HINT, Term};

pub use crate::service_entry::DEFAULT_NAME;

/// Only per-user entries exist on Windows; `System` is never used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Scope {
    System,
    User,
}

/// Linux's "keep running without a login": nothing to choose on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Linger {
    Ask,
    Yes,
    No,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub bind: Option<String>,
    pub lan: bool,
    /// Linux system services only; refused here.
    pub user: Option<String>,
    #[allow(dead_code)]
    pub linger: Linger,
    pub dry_run: bool,
    /// The entry's name (a second server with another `--home` needs its own).
    pub name: String,
    pub home: Option<PathBuf>,
    pub replace: bool,
}

pub struct Summary {
    pub scope: Scope,
    pub active: bool,
    pub home: Option<PathBuf>,
    pub bind: Option<String>,
    pub user: Option<String>,
}

/// The program next to `tessera.exe` that runs it without a window.
const LAUNCHER: &str = "tessera-service.exe";

fn log_file(paths: &Paths, name: &str) -> PathBuf {
    paths.data_dir.join("logs").join(format!("{name}.log"))
}

fn launcher() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .and_then(|e| tessera_core::platform::canonicalize(&e))
        .context("finding tessera.exe")?;
    let l = exe.with_file_name(LAUNCHER);
    if !l.is_file() {
        bail!(
            "{LAUNCHER} is missing next to {} (it keeps Tessera running in the background). Install Tessera again with the PowerShell installer, or copy {LAUNCHER} from the release zip next to it.",
            exe.display()
        );
    }
    Ok(l)
}

/// `tessera-service.exe`'s pid and Tessera's, while it runs.
fn running(log: &Path) -> Option<(u32, u32)> {
    let mut p = log.as_os_str().to_owned();
    p.push(".pid");
    let text = std::fs::read_to_string(PathBuf::from(p)).ok()?;
    let mut it = text.split_whitespace().map(|n| n.parse::<u32>().ok());
    let (l, c) = (it.next()??, it.next()??);
    (win::alive(l) && win::runs(l, "tessera-service")).then_some((l, c))
}

/// The address the service will listen on.
pub fn resolve_bind(opts: &InstallOptions, paths: &Paths) -> Result<String> {
    let configured = Config::load(paths)
        .map(|c| c.server.bind)
        .unwrap_or_else(|_| "127.0.0.1:7878".into());
    let port = configured
        .parse::<std::net::SocketAddr>()
        .map(|a| a.port())
        .unwrap_or(7878);
    let bind = match (&opts.bind, opts.lan) {
        (Some(b), _) => b.clone(),
        (None, true) => format!("0.0.0.0:{port}"),
        (None, false) => configured,
    };
    bind.parse::<std::net::SocketAddr>()
        .with_context(|| format!("invalid listen address `{bind}`"))?;
    Ok(bind)
}

/// The server on this data folder answers (its health check), within a few
/// seconds; not merely runs.
async fn answering(paths: &Paths) -> bool {
    let Some(lock) = std::fs::read_to_string(crate::backend::lock_file(paths))
        .ok()
        .and_then(|t| serde_json::from_str::<crate::backend::ServerLock>(&t).ok())
    else {
        return false;
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        tessera_client::probe(std::slice::from_ref(&lock.url), &Default::default()),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

/// The server running on this data folder, per its lock file.
fn server_pid(paths: &Paths) -> Option<u32> {
    let lock: crate::backend::ServerLock =
        serde_json::from_str(&std::fs::read_to_string(crate::backend::lock_file(paths)).ok()?)
            .ok()?;
    (win::alive(lock.pid) && win::runs(lock.pid, "tessera")).then_some(lock.pid)
}

/// Ask `pid` to stop and wait for it; end it if it hasn't stopped after 10
/// seconds. Its launcher goes too, so it doesn't start it again: after a
/// clean stop it ends by itself, but one between two tries (waiting after
/// a failure, or pausing after several) would start it again later.
fn stop(pid: u32, launcher_pid: Option<u32>) -> Result<()> {
    win::request_stop(pid);
    let mut gone = false;
    for _ in 0..80 {
        if !win::alive(pid) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(125));
    }
    if let Some(l) = launcher_pid
        && win::alive(l)
        && win::runs(l, "tessera-service")
    {
        win::terminate(l);
    }
    if gone {
        return Ok(());
    }
    win::kill_tree(pid);
    std::thread::sleep(Duration::from_millis(500));
    if win::alive(pid) {
        bail!("the Tessera process (pid {pid}) didn't stop; end it in Task Manager");
    }
    Ok(())
}

/// How a start went.
enum Started {
    /// The server runs (its lock file names a live Tessera).
    Up,
    /// A minute later it still runs but isn't up yet.
    Slow,
    /// It stopped before it was up (with the launcher that would try
    /// again, if that still runs).
    Failed(Option<u32>),
}

/// Wait up to a minute for the server just started through `log`'s
/// launcher to come up.
async fn wait_up(paths: &Paths, log: &Path) -> Started {
    let mut launcher = None;
    for i in 0..240 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if server_pid(paths).is_some() {
            return Started::Up;
        }
        match running(log) {
            Some((l, c)) => {
                launcher = Some(l);
                if !win::alive(c) {
                    // Exited before it was up: the launcher waits to try again.
                    return Started::Failed(Some(l));
                }
            }
            // The launcher writes its pid file right after starting Tessera
            // (an antivirus may hold that up): gone after that, or not there
            // within half a minute, it ended.
            None if launcher.is_some() || i >= 120 => return Started::Failed(None),
            None => {}
        }
    }
    Started::Slow
}

/// Start the entry's command now (as Windows will at the next sign-in).
fn start_now(line: &str) -> Result<()> {
    let mut args = split_windows_args(line).into_iter();
    let program = args.next().context("empty startup entry")?;
    let args: Vec<String> = args.collect();
    win::spawn_detached(Path::new(&program), &args, &[])
        .with_context(|| format!("starting {program}"))?;
    Ok(())
}

fn say(t: &Option<&mut Term>, s: &str) {
    match t {
        Some(t) => t.ok(s),
        None => println!("{s}"),
    }
}

/// Write (or rewrite) the startup entry and (re)start the server.
pub async fn install(paths: &Paths, opts: &InstallOptions, term: Option<&mut Term>) -> Result<()> {
    let _entries = (!opts.dry_run).then(entries_lock).transpose()?;
    if opts.user.is_some() {
        bail!("--user is for Linux system services; on Windows the service always runs as you");
    }
    let launcher = launcher()?;
    let bind = resolve_bind(opts, paths)?;
    let log = log_file(paths, &opts.name);
    let line = command_line(
        &launcher,
        &log,
        opts.home.as_deref(),
        &["serve".into(), "--bind".into(), bind.clone()],
    );
    let name = entry_name(&opts.name);
    if opts.dry_run {
        println!("Would add \"{name}\" to the programs Windows starts when you sign in:\n");
        println!("    {line}");
        println!("\nand start it now. Its output goes to {}.", log.display());
        return Ok(());
    }
    // A server already running here: ours (rewritten and restarted, as on
    // Linux) or started by hand (it holds the port).
    let previous = win::run_entry(&name);
    let ours = previous
        .as_deref()
        .and_then(parse_entry)
        .and_then(|e| running(&e.log));
    // Ours still starting (not up yet) from this same entry: wait for it.
    let mut already_starting = false;
    if let Some(pid) = server_pid(paths) {
        match ours {
            Some((l, c)) if c == pid => stop(pid, Some(l))?,
            _ if opts.replace => {
                stop(pid, None)?;
                say(
                    &term,
                    &format!("Stopped the Tessera server that was running (pid {pid})."),
                );
            }
            _ => bail!(
                "a Tessera server is already running (pid {pid}) outside the service. Stop it first (Ctrl+C where it runs), or run this again with --replace to stop it for you"
            ),
        }
    } else if let Some((l, c)) = ours {
        if win::alive(c) && win::runs(c, "tessera") {
            // Its server is still starting. Ending the launcher would leave
            // that server without one, and starting another would race it.
            if previous.as_deref() == Some(line.as_str()) {
                already_starting = true;
            } else {
                stop(c, Some(l))?;
            }
        } else {
            // Its launcher between two tries.
            win::terminate(l);
        }
    }
    win::set_run_entry(&name, &line).context("adding Tessera to the startup programs")?;
    forget_update(&name);
    say(
        &term,
        &format!(
            "{} \"{name}\" in the programs Windows starts when you sign in{}",
            if previous.is_some() {
                "Updated"
            } else {
                "Added"
            },
            if previous.as_deref() == Some(line.as_str()) {
                " (unchanged)"
            } else {
                ""
            }
        ),
    );
    if !already_starting {
        start_now(&line)?;
    }
    // Give it time to come up: a first start (a new database, an antivirus
    // looking at a program it hasn't seen) can take a while.
    let mut still_starting = false;
    match wait_up(paths, &log).await {
        Started::Up => {}
        Started::Slow => still_starting = true,
        Started::Failed(launcher) => {
            // It would try again and again; the next sign-in tries afresh.
            if let Some(l) = launcher {
                win::terminate(l);
            }
            bail!(
                "the server didn't start. The end of its log says why: {}",
                log.display()
            );
        }
    }
    if still_starting {
        say(
            &term,
            "It is still starting (a first start can take a minute); `tessera service status` says when it answers.",
        );
    }
    let awake = Config::load(paths)
        .map(|c| c.server.keep_awake)
        .unwrap_or(true);
    say(
        &term,
        &format!(
            "Running in the background for your user, on {bind}. It starts again whenever you sign in to Windows{}.",
            if awake {
                ", and keeps this computer from sleeping while it's plugged in"
            } else {
                ""
            }
        ),
    );
    if let Some(t) = term.as_deref() {
        t.line(&format!(
            "{} {}",
            t.paint("  Manage it with", S_HINT),
            t.paint("tessera service status|uninstall", S_ACCENT)
        ));
        t.line(&t.paint(&format!("  Its log: {}", log.display()), S_HINT));
    }
    Ok(())
}

pub fn uninstall(name: &str, dry_run: bool) -> Result<()> {
    let _entries = (!dry_run).then(entries_lock).transpose()?;
    let entry = entry_name(name);
    let line = win::run_entry(&entry);
    if dry_run {
        match &line {
            Some(l) => println!(
                "Would stop it and remove \"{entry}\" from the programs Windows starts when you sign in:\n    {l}"
            ),
            None => println!("Nothing to remove: \"{entry}\" isn't in the startup programs."),
        }
        return Ok(());
    }
    // The user's choice, also over an update's note (one cut short may
    // have taken the entry out already).
    forget_update(&entry);
    let Some(line) = line else {
        println!("No Tessera service is installed.");
        return Ok(());
    };
    win::remove_run_entry(&entry).context("removing Tessera from the startup programs")?;
    if let Some((l, c)) = parse_entry(&line).and_then(|e| running(&e.log)) {
        stop(c, Some(l))?;
    }
    println!("Removed the Tessera service. Your data and settings are untouched.");
    Ok(())
}

/// Print the service state. Returns the exit code (0 = running).
pub async fn status(paths: &Paths, name: &str) -> Result<i32> {
    let entry = entry_name(name);
    let Some(line) = win::run_entry(&entry) else {
        println!("Not installed. `tessera service install` runs Tessera in the background.");
        return Ok(3);
    };
    let parsed = parse_entry(&line);
    let run = parsed.as_ref().and_then(|e| running(&e.log));
    println!(
        "{} startup program \"{entry}\": {}",
        if run.is_some() { "●" } else { "○" },
        if run.is_some() {
            "running"
        } else {
            "not running"
        }
    );
    println!("  runs:  {line}");
    if let Some(e) = &parsed {
        println!(
            "  data:  {}",
            e.home
                .as_ref()
                .map(|h| h.display().to_string())
                .unwrap_or_else(|| "the default data folder".into())
        );
        println!("  log:   {}", e.log.display());
    }
    if let Ok(text) = std::fs::read_to_string(crate::backend::lock_file(paths))
        && let Ok(lock) = serde_json::from_str::<crate::backend::ServerLock>(&text)
    {
        let up = tessera_client::probe(std::slice::from_ref(&lock.url), &Default::default())
            .await
            .is_ok();
        println!(
            "  server: {} (pid {}) {}",
            lock.url,
            lock.pid,
            if up { "answering" } else { "not answering" }
        );
    }
    Ok(if run.is_some() { 0 } else { 3 })
}

pub fn summary(name: &str) -> Option<Summary> {
    let e = parse_entry(&win::run_entry(&entry_name(name))?)?;
    Some(Summary {
        scope: Scope::User,
        active: running(&e.log).is_some(),
        bind: bind_of(&e.args),
        home: e.home,
        user: None,
    })
}

/// Restart the server (after settings changed).
pub fn restart(_scope: Scope, name: &str) -> Result<()> {
    let line = win::run_entry(&entry_name(name)).context("the Tessera service isn't installed")?;
    if let Some((l, c)) = parse_entry(&line).and_then(|e| running(&e.log)) {
        stop(c, Some(l))?;
    }
    start_now(&line)
}

pub fn scope_label(_scope: Scope) -> &'static str {
    "startup program"
}

/// Windows has no root account in the Linux sense, and the service always
/// runs as the signed-in user.
pub fn running_as_root() -> bool {
    false
}

pub fn user_exists(_name: &str) -> bool {
    false
}

// ---- the machine (`tessera node`) ---------------------------------------------

pub fn node_service_state() -> tessera_proto::NodeService {
    let line = win::run_entry(&entry_name(NODE_NAME));
    tessera_proto::NodeService {
        available: true,
        installed: line.is_some(),
        active: line
            .as_deref()
            .and_then(parse_entry)
            .and_then(|e| running(&e.log))
            .is_some(),
        enabled: line.is_some(),
        linger: false,
    }
}

/// Add (or update) and start the machine's startup entry. `linger` means
/// nothing on Windows: it runs while the user is signed in.
pub fn node_install(home: Option<&Path>, _linger: bool) -> Result<()> {
    let _entries = entries_lock()?;
    let launcher = launcher()?;
    let paths = match home {
        Some(h) => Paths::at(h),
        None => Paths::resolve(),
    };
    let log = log_file(&paths, NODE_NAME);
    let line = command_line(&launcher, &log, home, &["node".into(), "run".into()]);
    let name = entry_name(NODE_NAME);
    if let Some((l, c)) = win::run_entry(&name)
        .and_then(|old| parse_entry(&old))
        .and_then(|e| running(&e.log))
    {
        stop(c, Some(l))?;
    }
    win::set_run_entry(&name, &line).context("adding the machine to the startup programs")?;
    forget_update(&name);
    start_now(&line)
}

/// Stop and remove the machine's startup entry (if there is one).
pub fn node_uninstall() -> Result<()> {
    let _entries = entries_lock()?;
    let name = entry_name(NODE_NAME);
    forget_update(&name);
    let Some(line) = win::run_entry(&name) else {
        return Ok(());
    };
    win::remove_run_entry(&name).context("removing the machine from the startup programs")?;
    if let Some((l, c)) = parse_entry(&line).and_then(|e| running(&e.log)) {
        stop(c, Some(l))?;
    }
    Ok(())
}

/// Linux asks about running without a login; Windows has nothing to ask.
pub fn wants_linger_hint() -> bool {
    false
}

// ---- updates (the installer's steps, `hooks.nsh`) ------------------------------

/// Run by the installer during an update (it sets this): the update's notes
/// are its own to keep.
fn by_installer() -> bool {
    std::env::var_os("TESSERA_UPDATE").is_some_and(|v| v == "1")
}

/// The user chose anew for entry `name` (added or removed it): an update cut
/// short must not bring back what it noted before.
fn forget_update(name: &str) {
    if !by_installer() {
        let _ = win::remove_user_value(win::UPDATE_KEY, name);
    }
}

/// Put back the startup entries an update noted before taking them out
/// (`hooks.nsh`), exactly as they were (data folder, address, everything),
/// and start them; each note goes once its entry is back. `if_interrupted`:
/// only when the installer that noted them no longer runs (the app, when it
/// opens, finishes an update that was cut short).
pub async fn restore_update(if_interrupted: bool) -> Result<()> {
    let installer =
        win::user_value(win::UPDATE_KEY, "Installer").and_then(|p| p.trim().parse::<u32>().ok());
    if if_interrupted && installer.is_some_and(win::alive) {
        // Still at work: it finishes this itself.
        return Ok(());
    }
    // One change of the startup entries at a time (an uninstall meanwhile
    // waits; the notes are read under it, so a removed one isn't restored).
    let _entries = entries_lock()?;
    let noted: Vec<(String, String)> = [entry_name(DEFAULT_NAME), entry_name(NODE_NAME)]
        .into_iter()
        .filter_map(|name| win::user_value(win::UPDATE_KEY, &name).map(|line| (name, line)))
        .collect();
    if noted.is_empty() {
        let _ = win::remove_user_value(win::UPDATE_KEY, "Folder");
        let _ = win::remove_user_value(win::UPDATE_KEY, "Installer");
        return Ok(());
    }
    let launcher = launcher()?;
    let mut problems = Vec::new();
    for (name, line) in noted {
        match restore_one(&launcher, &name, &line).await {
            Ok(said) => println!("{said}"),
            Err(e) => problems.push(format!("\"{name}\": {e:#}")),
        }
    }
    if !problems.is_empty() {
        bail!("{}. Your chats and settings are kept", problems.join("; "));
    }
    // Notes left (one still starting): kept with them, tried again.
    if [entry_name(DEFAULT_NAME), entry_name(NODE_NAME)]
        .iter()
        .all(|n| win::user_value(win::UPDATE_KEY, n).is_none())
    {
        let _ = win::remove_user_value(win::UPDATE_KEY, "Folder");
        let _ = win::remove_user_value(win::UPDATE_KEY, "Installer");
    }
    Ok(())
}

/// One change of Tessera's startup entries at a time, across programs
/// (installing, removing, an update's restore): a lock file held while it
/// is made (waited for, a while at most).
fn entries_lock() -> Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join("tessera-startup-entries.lock");
    for _ in 0..300 {
        match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .share_mode(0)
            .open(&path)
        {
            Ok(f) => return Ok(f),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    bail!("another change of Tessera's startup programs is under way; try again in a moment")
}

async fn restore_one(launcher: &Path, name: &str, noted: &str) -> Result<String> {
    let entry = parse_entry(noted).context("what the update noted can't be read")?;
    let program = split_windows_args(noted)
        .into_iter()
        .next()
        .unwrap_or_default();
    // Only this Tessera's own entry (the installer noted only those).
    if tessera_core::platform::canonicalize(Path::new(&program))
        .ok()
        .as_deref()
        != Some(launcher)
    {
        bail!(
            "it starts {program}, not this Tessera ({}); kept as noted",
            launcher.display()
        );
    }
    if let Some(home) = &entry.home
        && !home.is_dir()
    {
        bail!(
            "its data folder {} isn't there; kept as noted",
            home.display()
        );
    }
    // Written again since (the user chose, or the update did): that one
    // stays. Otherwise exactly the noted one.
    let line = match win::run_entry(name) {
        Some(now) => now,
        None => {
            win::set_run_entry(name, noted).context("adding it back to the startup programs")?;
            noted.to_string()
        }
    };
    let entry = parse_entry(&line).context("its startup entry can't be read")?;
    // The server: up (answering its lock), not merely launched, before the
    // note goes, also when it was launched already (a retry).
    if name == entry_name(DEFAULT_NAME) && running(&entry.log).is_some() {
        let paths = entry
            .home
            .as_deref()
            .map(Paths::at)
            .unwrap_or_else(Paths::resolve);
        if server_pid(&paths).is_none() || !answering(&paths).await {
            return Ok(format!(
                "\"{name}\" is back in the startup programs and still starting."
            ));
        }
    }
    if running(&entry.log).is_none() {
        start_now(&line)?;
        if name == entry_name(DEFAULT_NAME) {
            let paths = entry
                .home
                .as_deref()
                .map(Paths::at)
                .unwrap_or_else(Paths::resolve);
            match wait_up(&paths, &entry.log).await {
                // Up and answering, before the note goes.
                Started::Up if answering(&paths).await => {}
                Started::Up => {
                    return Ok(format!(
                        "\"{name}\" is back in the startup programs and still starting."
                    ));
                }
                // Not up yet: the note stays, and is looked at again (the
                // app, when it opens).
                Started::Slow => {
                    return Ok(format!(
                        "\"{name}\" is back in the startup programs and still starting."
                    ));
                }
                // The note stays: tried again (the app, the next installer).
                Started::Failed(_) => bail!(
                    "it is back in the startup programs but didn't start. The end of its log says why: {}",
                    entry.log.display()
                ),
            }
        } else {
            // The machine: running (its launcher and it) a few seconds on.
            let mut up = false;
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if let Some((_, c)) = running(&entry.log)
                    && win::alive(c)
                {
                    up = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            if !up || running(&entry.log).is_none_or(|(_, c)| !win::alive(c)) {
                bail!(
                    "it is back in the startup programs but didn't keep running. The end of its log says why: {}",
                    entry.log.display()
                );
            }
        }
    }
    // Back and running: the note has done its work.
    let _ = win::remove_user_value(win::UPDATE_KEY, name);
    Ok(format!(
        "\"{name}\" is back in the startup programs and running."
    ))
}
