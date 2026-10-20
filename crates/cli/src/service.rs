//! `tessera service install|uninstall|status`: run the server in the
//! background with systemd.
//!
//! As root it is a system unit (`/etc/systemd/system/tessera.service`,
//! `User=root` unless `--user`); otherwise a user unit
//! (`~/.config/systemd/user/tessera.service`), plus `loginctl enable-linger`
//! so it keeps running without a login (asked first). The unit runs this
//! executable through the user's login shell so the agent's tools get the
//! same `PATH` as a terminal. Installing again rewrites the unit and
//! restarts it.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use tessera_core::config::{Config, Paths};

use crate::setup::term::{S_ACCENT, S_HINT, Term};

pub const DEFAULT_NAME: &str = "tessera";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `/etc/systemd/system`, managed by root.
    System,
    /// `~/.config/systemd/user`, `systemctl --user`.
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linger {
    Ask,
    Yes,
    No,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Listen address; `None`: `server.bind` from the config (local-only by
    /// default).
    pub bind: Option<String>,
    /// Listen on every interface (LAN), on the configured port.
    pub lan: bool,
    /// System units only: run as this user instead of root.
    pub user: Option<String>,
    pub linger: Linger,
    pub dry_run: bool,
    /// Unit name (without `.service`).
    pub name: String,
    /// Set `TESSERA_HOME` in the unit (the CLI was pointed at a custom home).
    pub home: Option<PathBuf>,
    /// Stop a server that runs outside the service (e.g. one the desktop
    /// app started) so the service can take over its port.
    pub replace: bool,
}

/// Everything the unit file depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSpec {
    pub exe: PathBuf,
    pub bind: String,
    pub shell: PathBuf,
    pub scope: Scope,
    /// `User=` for system units.
    pub run_as: Option<String>,
    pub home: Option<PathBuf>,
}

/// Characters that can't be passed safely through systemd quoting and
/// the shell command line.
fn check_path(p: &Path, what: &str) -> Result<String> {
    let s = p
        .to_str()
        .ok_or_else(|| anyhow!("the {what} path isn't valid UTF-8: {}", p.display()))?;
    if let Some(c) = s
        .chars()
        .find(|c| c.is_control() || matches!(c, '\'' | '"' | '\\' | '$' | '`'))
    {
        bail!(
            "the {what} path {s:?} contains {c:?}, which can't be used in a service file; move it somewhere simpler"
        );
    }
    // `%` starts a systemd specifier.
    Ok(s.replace('%', "%%"))
}

/// The unit file text.
pub fn unit_text(spec: &UnitSpec) -> Result<String> {
    let exe = check_path(&spec.exe, "tessera executable")?;
    let shell = check_path(&spec.shell, "shell")?;
    if shell.contains(' ') {
        bail!("the shell path {shell:?} contains a space");
    }
    let bind: std::net::SocketAddr = spec
        .bind
        .parse()
        .with_context(|| format!("invalid listen address `{}`", spec.bind))?;
    let mut service = vec![
        "# A login shell gives the agent's commands the same PATH as your terminal.".to_string(),
        format!("ExecStart={shell} -lc 'exec \"{exe}\" serve --bind \"{bind}\"'"),
        "Restart=on-failure".into(),
        "RestartSec=3".into(),
        "Environment=TESSERA_LOG=info".into(),
    ];
    if let Some(home) = &spec.home {
        let h = check_path(home, "TESSERA_HOME")?;
        service.push(format!("Environment=\"TESSERA_HOME={h}\""));
    }
    if spec.scope == Scope::System {
        let user = spec.run_as.as_deref().unwrap_or("root");
        if !valid_user_name(user) {
            bail!("`{user}` isn't a valid user name");
        }
        // Setting User= also gives the service that user's $HOME.
        service.insert(0, format!("User={user}"));
    }
    service.push("KillSignal=SIGINT".into());
    // The stop signal goes to the server alone: it notes what is working and
    // winds down, and what it runs (Claude Code, commands) ends with it. Sent
    // to every process, a Ctrl+C makes Claude Code stop its turn as if the
    // user had, and the turn can't continue after the restart.
    service.push("KillMode=mixed".into());
    // Something the server runs (a browser, a build) killed for running out
    // of memory takes only itself down, not the server and every device.
    service.push("OOMPolicy=continue".into());
    // The server's watchdog says it still runs tasks; it switches systemd's
    // watchdog on itself (crates/cli/src/watchdog.rs).
    service.push("NotifyAccess=main".into());
    service.push("TimeoutStopSec=20".into());
    let wanted = match spec.scope {
        Scope::System => "multi-user.target",
        Scope::User => "default.target",
    };
    Ok(format!(
        "# Written by `tessera service install`; run it again to update this file.\n\
         [Unit]\n\
         Description=Tessera agent server\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         {}\n\
         \n\
         [Install]\n\
         WantedBy={wanted}\n",
        service.join("\n")
    ))
}

fn valid_user_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !s.starts_with('-')
}

/// The `TESSERA_HOME` an installed unit uses (`None`: the default
/// directories).
pub fn unit_home_of(text: &str) -> Option<PathBuf> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("Environment="))
        .map(|v| v.trim_matches('"'))
        .find_map(|v| v.strip_prefix("TESSERA_HOME="))
        .map(|h| PathBuf::from(h.replace("%%", "%")))
}

/// The listen address a unit passes to `serve --bind`.
pub fn unit_bind_of(text: &str) -> Option<String> {
    let exec = text.lines().find(|l| l.starts_with("ExecStart="))?;
    let rest = exec.split_once("--bind \"")?.1;
    rest.split('"').next().map(str::to_string)
}

/// `User=` of a system unit.
fn unit_user_of(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("User="))
        .map(str::to_string)
}

/// Refuse to take over a unit that serves another data directory.
fn check_same_home(existing: &str, home: Option<&Path>, name: &str) -> Result<()> {
    let theirs = unit_home_of(existing);
    if theirs.as_deref() != home {
        let dir = |h: Option<&Path>| {
            h.map(|p| p.display().to_string())
                .unwrap_or_else(|| "the default data directory".into())
        };
        bail!(
            "{name}.service already runs Tessera for {}, not {}. Remove it first (`tessera service uninstall`), or install another one with --name <other-name>",
            dir(theirs.as_deref()),
            dir(home)
        );
    }
    Ok(())
}

/// Where the unit file goes: where systemd looks, which follows the user
/// manager's own XDG directories, not this process's (a unit written
/// elsewhere would never be read, and `restart` would hit another one).
pub fn unit_path(scope: Scope, name: &str) -> Result<PathBuf> {
    Ok(match scope {
        Scope::System => PathBuf::from("/etc/systemd/system").join(format!("{name}.service")),
        Scope::User => manager_dir("XDG_CONFIG_HOME", ".config")?
            .join("systemd/user")
            .join(format!("{name}.service")),
    })
}

/// A base directory as the systemd user manager sees it.
fn manager_dir(var: &str, fallback: &str) -> Result<PathBuf> {
    let env = query(&systemctl(Scope::User, &["show-environment"]));
    let prefix = format!("{var}=");
    env.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim_matches(['"', '\'']))
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(fallback)))
        .ok_or_else(|| anyhow!("no home directory"))
}

/// A service started by systemd resolves the default data directory with
/// the manager's environment. If this process resolved another one (custom
/// XDG directories), a unit without `TESSERA_HOME` would serve the wrong
/// data, so installing needs an explicit home.
fn check_default_home(paths: &Paths, scope: Scope, home: Option<&Path>) -> Result<()> {
    if home.is_some() || scope != Scope::User {
        return Ok(());
    }
    let theirs = manager_dir("XDG_DATA_HOME", ".local/share")?.join("tessera");
    if paths.data_dir != theirs {
        bail!(
            "this Tessera uses custom data directories ({}), which a background service wouldn't see. Set TESSERA_HOME to install a service for it, or run `tessera serve` yourself",
            paths.data_dir.display()
        );
    }
    Ok(())
}

fn systemctl(scope: Scope, args: &[&str]) -> Vec<String> {
    let mut v = vec!["systemctl".to_string()];
    if scope == Scope::User {
        v.push("--user".into());
    }
    v.extend(args.iter().map(|s| s.to_string()));
    v
}

/// The commands `install` runs after writing the unit.
pub fn install_commands(scope: Scope, name: &str) -> Vec<Vec<String>> {
    let unit = format!("{name}.service");
    vec![
        systemctl(scope, &["daemon-reload"]),
        systemctl(scope, &["enable", &unit]),
        // Starts it, or restarts it with the new unit when already running.
        systemctl(scope, &["restart", &unit]),
    ]
}

pub fn uninstall_commands(scope: Scope, name: &str) -> Vec<Vec<String>> {
    let unit = format!("{name}.service");
    vec![
        systemctl(scope, &["disable", "--now", &unit]),
        systemctl(scope, &["daemon-reload"]),
    ]
}

/// Prefer the user's own login shell when it is bash or zsh (where their
/// PATH lives), then bash, then sh.
pub fn pick_shell(login: Option<&str>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if let Some(sh) = login {
        let p = Path::new(sh);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "bash" | "zsh") && p.is_absolute() && exists(p) {
            return p.to_path_buf();
        }
    }
    for cand in ["/usr/bin/bash", "/bin/bash"] {
        if exists(Path::new(cand)) {
            return PathBuf::from(cand);
        }
    }
    PathBuf::from("/bin/sh")
}

/// (name, home, shell) from an `/etc/passwd` line.
fn passwd_entry(
    passwd: &str,
    pred: impl Fn(&str, u32) -> bool,
) -> Option<(String, String, String)> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 7 {
            return None;
        }
        let uid: u32 = f[2].parse().ok()?;
        pred(f[0], uid).then(|| (f[0].to_string(), f[5].to_string(), f[6].to_string()))
    })
}

fn effective_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

/// The user who ran `sudo`, unless that was root itself.
fn sudo_user() -> Option<String> {
    std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")
}

fn is_root() -> bool {
    effective_uid() == Some(0)
}

pub fn running_as_root() -> bool {
    is_root()
}

pub fn user_exists(name: &str) -> bool {
    valid_user_name(name) && user_by_name(name).is_some()
}

fn user_by_uid(uid: u32) -> Option<(String, String, String)> {
    let p = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd_entry(&p, |_, u| u == uid)
}

fn user_by_name(name: &str) -> Option<(String, String, String)> {
    let p = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd_entry(&p, |n, _| n == name)
}

fn current_user() -> Option<(String, String, String)> {
    effective_uid()
        .and_then(user_by_uid)
        .or_else(|| std::env::var("USER").ok().and_then(|u| user_by_name(&u)))
}

/// Background services can be installed here (Linux with systemd).
pub fn available() -> bool {
    supported().is_ok()
}

fn supported() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "background services aren't supported on {} yet (Linux with systemd only). Run `tessera serve` in a terminal, or start it with your system's service manager.",
            std::env::consts::OS
        );
    }
    if !Path::new("/run/systemd/system").exists() {
        bail!(
            "systemd isn't running on this machine, so Tessera can't install a service. Run `tessera serve` under your init system (for example an OpenRC or runit service) instead."
        );
    }
    Ok(())
}

fn run(cmd: &[String]) -> Result<String> {
    let out = Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .with_context(|| format!("running {}", cmd[0]))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "`{}` failed{}",
            cmd.join(" "),
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn query(cmd: &[String]) -> String {
    Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn lingering(user: &str) -> bool {
    Path::new("/var/lib/systemd/linger").join(user).exists()
}

/// No graphical session: most likely a server reached over SSH.
fn headless() -> bool {
    std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
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

fn quote(cmd: &[String]) -> String {
    cmd.iter()
        .map(|a| {
            if a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./@=:".contains(c))
            {
                a.clone()
            } else {
                format!("'{a}'")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Write (or rewrite) the unit, enable it and (re)start it.
pub async fn install(
    paths: &Paths,
    opts: &InstallOptions,
    mut term: Option<&mut Term>,
) -> Result<()> {
    if !opts.dry_run {
        supported()?;
    }
    let root = is_root();
    let scope = if root { Scope::System } else { Scope::User };
    if opts.user.is_some() && !root {
        bail!(
            "only root can install a service that runs as another user; run `sudo tessera service install --user <name>`, or leave out --user to install it for yourself"
        );
    }
    // `sudo tessera service install` means "for me": the agent's commands
    // must not run as root, where they could change system files.
    let user = opts.user.clone().or_else(|| sudo_user().filter(|_| root));
    let target = match (&user, scope) {
        (Some(u), _) => {
            Some(user_by_name(u).ok_or_else(|| anyhow!("there is no user `{u}` on this machine"))?)
        }
        (None, Scope::System) => user_by_name("root"),
        (None, Scope::User) => current_user(),
    };
    let exe = std::env::current_exe()
        .and_then(|e| tessera_core::platform::canonicalize(&e))
        .context("finding the tessera executable")?;
    let bind = resolve_bind(opts, paths)?;
    let shell = pick_shell(target.as_ref().map(|t| t.2.as_str()), |p| p.exists());
    let spec = UnitSpec {
        exe: exe.clone(),
        bind: bind.clone(),
        shell,
        scope,
        run_as: match scope {
            Scope::System => Some(user.clone().unwrap_or_else(|| "root".into())),
            Scope::User => None,
        },
        home: opts.home.clone(),
    };
    if scope == Scope::System && user.is_none() && !opts.dry_run {
        let warn = "This service will run as root, so the agent's commands will too. Prefer `sudo tessera service install --user <your user>`, or install it without sudo.";
        match term.as_deref_mut() {
            Some(t) => t.warn(warn),
            None => eprintln!("warning: {warn}"),
        }
    }
    let text = unit_text(&spec)?;
    let path = unit_path(scope, &opts.name)?;
    check_default_home(paths, scope, opts.home.as_deref())?;
    let commands = install_commands(scope, &opts.name);
    let say = |t: &Option<&mut Term>, s: &str| match t {
        Some(t) => t.ok(s),
        None => println!("{s}"),
    };
    if opts.dry_run {
        println!("Would write {}:\n", path.display());
        for l in text.lines() {
            println!("    {l}");
        }
        println!("\nThen run:");
        for c in &commands {
            println!("    {}", quote(c));
        }
        if scope == Scope::User {
            match opts.linger {
                Linger::Yes => println!("    loginctl enable-linger"),
                Linger::Ask => println!(
                    "\nand, if you agree, `loginctl enable-linger` so it keeps running when you're logged out."
                ),
                Linger::No => {}
            }
        }
        if let Ok(existing) = std::fs::read_to_string(&path)
            && let Err(e) = check_same_home(&existing, opts.home.as_deref(), &opts.name)
        {
            println!("\nNote: a real install would stop here: {e}");
        }
        return Ok(());
    }
    // A server started by hand holds the port: the service would fail.
    if let Some(pid) = foreign_server(paths, scope, &opts.name, target.as_ref()) {
        if !opts.replace {
            bail!(
                "a Tessera server is already running (pid {pid}) outside the service. Stop it first (Ctrl+C where it runs), or run this again with --replace to stop it for you"
            );
        }
        stop_server(pid).await?;
        say(
            &term,
            &format!("Stopped the Tessera server that was running (pid {pid})."),
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let previous = std::fs::read_to_string(&path).ok();
    if let Some(p) = &previous {
        check_same_home(p, opts.home.as_deref(), &opts.name)?;
    }
    tessera_core::config::write_atomic(&path, text.as_bytes(), false)
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
    }
    let shown = match dirs::home_dir() {
        Some(h) if path.starts_with(&h) => {
            format!("~/{}", path.strip_prefix(&h).unwrap_or(&path).display())
        }
        _ => path.display().to_string(),
    };
    say(
        &term,
        &format!(
            "{} {shown}{}",
            if previous.is_some() {
                "Updated"
            } else {
                "Installed"
            },
            if previous.as_deref() == Some(text.as_str()) {
                " (unchanged)"
            } else {
                ""
            }
        ),
    );
    for c in &commands {
        if let Err(e) = run(c) {
            if scope == Scope::User && e.to_string().contains("bus") {
                bail!(
                    "{e}\nThe user service manager isn't reachable from this session. Log in directly (not with su), or run `sudo tessera service install` for a system service."
                );
            }
            return Err(e);
        }
    }
    // Give it a moment to come up (or fail).
    let unit = format!("{}.service", opts.name);
    let mut state = String::new();
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        state = query(&systemctl(scope, &["is-active", &unit]));
        if state == "active" || state == "failed" {
            break;
        }
    }
    if state != "active" {
        let journal = match scope {
            Scope::System => format!("journalctl -u {} -n 30", opts.name),
            Scope::User => format!("journalctl --user -u {} -n 30", opts.name),
        };
        bail!("the service didn't start (state: {state}). See what happened with: {journal}");
    }
    let who = match scope {
        Scope::System => format!("as {}", spec.run_as.as_deref().unwrap_or("root")),
        Scope::User => "for your user".to_string(),
    };
    say(
        &term,
        &format!("Running in the background {who}, on {bind}."),
    );
    if scope == Scope::System
        && let Some(u) = opts.user.as_deref().filter(|u| *u != "root")
    {
        let home = target.as_ref().map(|t| t.1.clone()).unwrap_or_default();
        if opts.home.is_none() {
            say(
                &term,
                &format!(
                    "It uses {u}'s Tessera data in {home}/.local/share/tessera; run `tessera setup` as {u} to configure it."
                ),
            );
        }
    }
    if scope == Scope::User {
        let user = target.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        if !user.is_empty() && !lingering(&user) {
            let want = match opts.linger {
                Linger::Yes => true,
                Linger::No => false,
                Linger::Ask => match term.as_deref_mut() {
                    Some(t) if t.caps.stdin_tty => {
                        t.blank();
                        t.para(
                            "A user service stops when you log out. On a machine you reach over SSH (like a Proxmox container or VM) it should keep running on its own; that is what \"lingering\" does.",
                            S_HINT,
                        );
                        t.confirm(
                            "Keep Tessera running when you're logged out? (loginctl enable-linger)",
                            headless(),
                        )
                        .await?
                        .unwrap_or(false)
                    }
                    _ => false,
                },
            };
            if want {
                run(&["loginctl".to_string(), "enable-linger".to_string()])?;
                say(
                    &term,
                    "Lingering is on: Tessera starts at boot and keeps running without a login.",
                );
            } else {
                let msg = "It stops when you log out. On a headless machine (e.g. a Proxmox LXC/VM), keep it running with: loginctl enable-linger";
                match term.as_deref() {
                    Some(t) => t.todo(msg),
                    None => println!("{msg}"),
                }
            }
        }
    }
    if let Some(t) = term.as_deref() {
        t.line(&format!(
            "{} {}",
            t.paint("  Manage it with", S_HINT),
            t.paint("tessera service status|uninstall", S_ACCENT)
        ));
    }
    Ok(())
}

/// Ask a `tessera serve` process to shut down (SIGINT, as systemd would)
/// and wait for it to exit.
async fn stop_server(pid: u32) -> Result<()> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let args: Vec<String> = cmdline
        .split(|b| *b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let is_server =
        args.first().is_some_and(|a| a.contains("tessera")) && args.iter().any(|a| a == "serve");
    if !is_server {
        bail!("process {pid} doesn't look like a Tessera server; stop it yourself first");
    }
    run(&["kill".into(), "-INT".into(), pid.to_string()])?;
    for _ in 0..80 {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(125)).await;
    }
    bail!("the running Tessera server (pid {pid}) didn't stop within 10 seconds")
}

/// Pid of a running server (per the lock file) that isn't this unit's.
fn foreign_server(
    paths: &Paths,
    scope: Scope,
    name: &str,
    target: Option<&(String, String, String)>,
) -> Option<u32> {
    // Only when the service would use the same data directory.
    if scope == Scope::System
        && target.is_some_and(|t| t.0 != "root")
        && std::env::var_os("TESSERA_HOME").is_none()
    {
        return None;
    }
    let lock: crate::backend::ServerLock =
        serde_json::from_str(&std::fs::read_to_string(crate::backend::lock_file(paths)).ok()?)
            .ok()?;
    if !Path::new(&format!("/proc/{}", lock.pid)).exists() {
        return None;
    }
    let main = query(&systemctl(
        scope,
        &[
            "show",
            "-p",
            "MainPID",
            "--value",
            &format!("{name}.service"),
        ],
    ));
    (main.parse::<u32>().ok() != Some(lock.pid)).then_some(lock.pid)
}

pub fn uninstall(name: &str, dry_run: bool) -> Result<()> {
    if !dry_run {
        supported()?;
    }
    let root = is_root();
    let scope = if root { Scope::System } else { Scope::User };
    let path = unit_path(scope, name)?;
    let commands = uninstall_commands(scope, name);
    if dry_run {
        println!("Would run:");
        for c in &commands[..1] {
            println!("    {}", quote(c));
        }
        println!("    rm {}", path.display());
        for c in &commands[1..] {
            println!("    {}", quote(c));
        }
        return Ok(());
    }
    if !path.exists() {
        let other = unit_path(if root { Scope::User } else { Scope::System }, name)?;
        if other.exists() && !root {
            bail!(
                "the Tessera service is a system service ({}); remove it with `sudo tessera service uninstall`",
                other.display()
            );
        }
        println!(
            "No Tessera service is installed ({} doesn't exist).",
            path.display()
        );
        return Ok(());
    }
    // Not loaded or already stopped is fine.
    let _ = run(&commands[0]);
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    let _ = run(&commands[1]);
    println!("Removed the Tessera service. Your data and settings are untouched.");
    Ok(())
}

/// Print the service state. Returns the exit code (0 = running).
pub async fn status(paths: &Paths, name: &str) -> Result<i32> {
    supported()?;
    let root = is_root();
    let unit = format!("{name}.service");
    let candidates = [
        (Scope::User, unit_path(Scope::User, name)?),
        (Scope::System, unit_path(Scope::System, name)?),
    ];
    let found: Vec<&(Scope, PathBuf)> = candidates.iter().filter(|(_, p)| p.exists()).collect();
    if found.is_empty() {
        println!("Not installed. `tessera service install` runs Tessera in the background.");
        return Ok(3);
    }
    let mut code = 3;
    for (scope, path) in found {
        let active = query(&systemctl(*scope, &["is-active", &unit]));
        let enabled = query(&systemctl(*scope, &["is-enabled", &unit]));
        let kind = match scope {
            Scope::System => "system service",
            Scope::User => "user service",
        };
        let running = active == "active";
        if running {
            code = 0;
        }
        println!(
            "{} {kind}: {} ({})",
            if running { "●" } else { "○" },
            if active.is_empty() {
                "unknown"
            } else {
                &active
            },
            if enabled.is_empty() {
                "not enabled"
            } else {
                &enabled
            },
        );
        println!("  unit:  {}", path.display());
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(exec) = text.lines().find(|l| l.starts_with("ExecStart=")) {
                println!("  runs:  {}", exec.trim_start_matches("ExecStart="));
            }
            println!(
                "  data:  {}",
                unit_home_of(&text)
                    .map(|h| h.display().to_string())
                    .unwrap_or_else(|| "the default data directory".into())
            );
        }
        if *scope == Scope::User
            && !root
            && let Some((user, _, _)) = current_user()
        {
            println!(
                "  linger: {}",
                if lingering(&user) {
                    "on (keeps running when you're logged out)"
                } else {
                    "off (stops when you log out; `loginctl enable-linger` keeps it running)"
                }
            );
        }
        if !running {
            let journal = match scope {
                Scope::System => format!("journalctl -u {name} -n 30"),
                Scope::User => format!("journalctl --user -u {name} -n 30"),
            };
            println!("  logs:  {journal}");
        }
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
    Ok(code)
}

/// An installed unit, for the setup wizard.
pub struct Summary {
    pub scope: Scope,
    pub active: bool,
    /// Its data directory (`None`: the default one).
    pub home: Option<PathBuf>,
    /// Where it listens (`serve --bind` in the unit).
    pub bind: Option<String>,
    /// The account a system unit runs as.
    pub user: Option<String>,
}

pub fn summary(name: &str) -> Option<Summary> {
    if supported().is_err() {
        return None;
    }
    let unit = format!("{name}.service");
    for scope in [Scope::User, Scope::System] {
        let Ok(path) = unit_path(scope, name) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            let active = query(&systemctl(scope, &["is-active", &unit])) == "active";
            return Some(Summary {
                scope,
                active,
                home: unit_home_of(&text),
                bind: unit_bind_of(&text),
                user: unit_user_of(&text),
            });
        }
    }
    None
}

/// Restart the installed unit (after settings changed).
pub fn restart(scope: Scope, name: &str) -> Result<()> {
    run(&systemctl(scope, &["restart", &format!("{name}.service")])).map(|_| ())
}

pub fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::System => "system service",
        Scope::User => "user service",
    }
}

// ---- the machine (`tessera node`) service --------------------------------

/// The user unit that keeps this computer on as a machine ("stay on").
pub const NODE_UNIT: &str = "tessera-node";

/// The node's unit file: `tessera node run` through the login shell (so
/// commands get the terminal's PATH), restarted if it fails, never as root.
pub fn node_unit_text(exe: &Path, shell: &Path, home: Option<&Path>) -> Result<String> {
    let exe = check_path(exe, "tessera executable")?;
    let shell = check_path(shell, "shell")?;
    if shell.contains(' ') {
        bail!("the shell path {shell:?} contains a space");
    }
    let mut service = vec![
        "# A login shell gives commands run here the same PATH as your terminal.".to_string(),
        format!("ExecStart={shell} -lc 'exec \"{exe}\" node run'"),
        // Exits 0 when turned off or revoked: then it stays stopped.
        "Restart=on-failure".into(),
        "RestartSec=5".into(),
        "Environment=TESSERA_LOG=info".into(),
    ];
    if let Some(home) = home {
        let h = check_path(home, "TESSERA_HOME")?;
        service.push(format!("Environment=\"TESSERA_HOME={h}\""));
    }
    service.push("KillSignal=SIGTERM".into());
    service.push("TimeoutStopSec=10".into());
    Ok(format!(
        "# Written by `tessera node service install`; `tessera node service uninstall` removes it.\n\
         [Unit]\n\
         Description=Tessera machine (lets your Tessera server use this computer, within what you allowed here)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         {}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        service.join("\n")
    ))
}

/// The machine service's state (for `tessera node status`).
pub fn node_service_state() -> tessera_proto::NodeService {
    let mut st = tessera_proto::NodeService {
        available: supported().is_ok() && !is_root(),
        installed: false,
        active: false,
        enabled: false,
        linger: false,
    };
    if !st.available {
        return st;
    }
    let unit = format!("{NODE_UNIT}.service");
    st.installed = unit_path(Scope::User, NODE_UNIT).is_ok_and(|p| p.exists());
    if st.installed {
        st.active = query(&systemctl(Scope::User, &["is-active", &unit])) == "active";
        st.enabled = query(&systemctl(Scope::User, &["is-enabled", &unit])) == "enabled";
    }
    st.linger = current_user().is_some_and(|(u, _, _)| lingering(&u));
    st
}

/// The Windows installer's notes of startup entries to put back after an
/// update (`service_windows.rs`): none here.
pub async fn restore_update(_if_interrupted: bool) -> Result<()> {
    println!("Nothing to restore: only Tessera's Windows installer takes startup entries out.");
    Ok(())
}

/// Install (or update) and start the machine service for this user. With
/// `linger`, it also runs without a login.
pub fn node_install(home: Option<&Path>, linger: bool) -> Result<()> {
    supported()?;
    if is_root() {
        bail!("a machine runs as your normal user, never as root");
    }
    let exe = std::env::current_exe()
        .and_then(|e| tessera_core::platform::canonicalize(&e))
        .context("finding the tessera executable")?;
    let user = current_user();
    let shell = pick_shell(user.as_ref().map(|t| t.2.as_str()), |p| p.exists());
    let text = node_unit_text(&exe, &shell, home)?;
    let path = unit_path(Scope::User, NODE_UNIT)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    tessera_core::config::write_atomic(&path, text.as_bytes(), false)
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
    }
    for c in install_commands(Scope::User, NODE_UNIT) {
        run(&c)?;
    }
    if linger
        && let Some((u, _, _)) = &user
        && !lingering(u)
    {
        run(&["loginctl".to_string(), "enable-linger".to_string()])?;
    }
    Ok(())
}

/// Stop and remove the machine service (if installed).
pub fn node_uninstall() -> Result<()> {
    if supported().is_err() || is_root() {
        return Ok(());
    }
    let path = unit_path(Scope::User, NODE_UNIT)?;
    if !path.exists() {
        return Ok(());
    }
    let commands = uninstall_commands(Scope::User, NODE_UNIT);
    let _ = run(&commands[0]);
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    let _ = run(&commands[1]);
    Ok(())
}

/// Ask whether to keep it running without a login (on a terminal).
pub fn wants_linger_hint() -> bool {
    headless()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_unit_runs_the_node_as_its_user() {
        let text = node_unit_text(
            Path::new("/home/ada/.local/bin/tessera"),
            Path::new("/usr/bin/bash"),
            Some(Path::new("/srv/t home")),
        )
        .unwrap();
        assert!(
            text.contains(
                "ExecStart=/usr/bin/bash -lc 'exec \"/home/ada/.local/bin/tessera\" node run'"
            ),
            "{text}"
        );
        assert!(text.contains("Environment=\"TESSERA_HOME=/srv/t home\""));
        assert!(text.contains("WantedBy=default.target"));
        assert!(!text.contains("User="));
        assert!(node_unit_text(Path::new("/tmp/a'b"), Path::new("/usr/bin/bash"), None).is_err());
    }

    fn spec(scope: Scope) -> UnitSpec {
        UnitSpec {
            exe: PathBuf::from("/home/ada/.local/bin/tessera"),
            bind: "127.0.0.1:7878".into(),
            shell: PathBuf::from("/usr/bin/bash"),
            scope,
            run_as: (scope == Scope::System).then(|| "root".to_string()),
            home: None,
        }
    }

    #[test]
    fn user_unit_matches_the_known_good_pattern() {
        let text = unit_text(&spec(Scope::User)).unwrap();
        assert!(text.contains(
            "ExecStart=/usr/bin/bash -lc 'exec \"/home/ada/.local/bin/tessera\" serve --bind \"127.0.0.1:7878\"'"
        ), "{text}");
        for l in [
            "Description=Tessera agent server",
            "After=network-online.target",
            "Wants=network-online.target",
            "Restart=on-failure",
            "RestartSec=3",
            "Environment=TESSERA_LOG=info",
            "KillSignal=SIGINT",
            "KillMode=mixed",
            "OOMPolicy=continue",
            "NotifyAccess=main",
            "TimeoutStopSec=20",
            "WantedBy=default.target",
        ] {
            assert!(text.lines().any(|x| x == l), "missing {l}\n{text}");
        }
        assert!(!text.contains("User="), "user units run as their owner");
        assert!(!text.contains("TESSERA_HOME"));
    }

    #[test]
    fn system_unit_runs_as_root_or_the_given_user() {
        let text = unit_text(&spec(Scope::System)).unwrap();
        assert!(text.lines().any(|l| l == "User=root"), "{text}");
        assert!(text.contains("WantedBy=multi-user.target"));
        let mut s = spec(Scope::System);
        s.run_as = Some("tessera".into());
        s.bind = "0.0.0.0:7878".into();
        s.home = Some(PathBuf::from("/srv/tessera data"));
        let text = unit_text(&s).unwrap();
        assert!(text.lines().any(|l| l == "User=tessera"));
        assert!(text.contains("serve --bind \"0.0.0.0:7878\""));
        assert!(text.contains("Environment=\"TESSERA_HOME=/srv/tessera data\""));
        s.run_as = Some("-rf".into());
        assert!(unit_text(&s).is_err());
    }

    #[test]
    fn unsafe_paths_are_refused_and_percent_escaped() {
        let mut s = spec(Scope::User);
        s.exe = PathBuf::from("/data/Coding projects/100%/tessera");
        let text = unit_text(&s).unwrap();
        assert!(
            text.contains("\"/data/Coding projects/100%%/tessera\""),
            "{text}"
        );
        for bad in [
            "/tmp/a'b",
            "/tmp/a\"b",
            "/tmp/$(x)",
            "/tmp/a`b`",
            "/tmp/a\\b",
            "/tmp/a\nb",
        ] {
            s.exe = PathBuf::from(bad);
            assert!(unit_text(&s).is_err(), "{bad:?}");
        }
        let mut s = spec(Scope::User);
        s.bind = "0.0.0.0:7878; rm -rf ~".into();
        assert!(unit_text(&s).is_err());
        s.bind = "[::]:7878".into();
        assert!(unit_text(&s).unwrap().contains("--bind \"[::]:7878\""));
    }

    #[test]
    fn units_for_another_home_are_left_alone() {
        let mut s = spec(Scope::User);
        s.home = Some(PathBuf::from("/srv/100%/tessera"));
        let text = unit_text(&s).unwrap();
        assert_eq!(
            unit_home_of(&text),
            Some(PathBuf::from("/srv/100%/tessera"))
        );
        assert!(check_same_home(&text, Some(Path::new("/srv/100%/tessera")), "tessera").is_ok());
        assert!(check_same_home(&text, None, "tessera").is_err());
        let default = unit_text(&spec(Scope::User)).unwrap();
        assert_eq!(unit_home_of(&default), None);
        // The wizard compares these with the settings to offer an update.
        assert_eq!(unit_bind_of(&default).as_deref(), Some("127.0.0.1:7878"));
        assert_eq!(unit_user_of(&default), None);
        let mut sys = spec(Scope::System);
        sys.run_as = Some("ann".into());
        sys.bind = "0.0.0.0:7900".into();
        let sys = unit_text(&sys).unwrap();
        assert_eq!(unit_bind_of(&sys).as_deref(), Some("0.0.0.0:7900"));
        assert_eq!(unit_user_of(&sys).as_deref(), Some("ann"));
        assert!(check_same_home(&default, None, "tessera").is_ok());
        let err = check_same_home(&default, Some(Path::new("/tmp/x")), "tessera").unwrap_err();
        assert!(err.to_string().contains("--name"), "{err}");
    }

    #[test]
    fn commands_and_paths() {
        assert_eq!(
            install_commands(Scope::User, "tessera"),
            vec![
                vec!["systemctl", "--user", "daemon-reload"],
                vec!["systemctl", "--user", "enable", "tessera.service"],
                vec!["systemctl", "--user", "restart", "tessera.service"],
            ]
        );
        assert_eq!(
            uninstall_commands(Scope::System, "tessera")[0],
            vec!["systemctl", "disable", "--now", "tessera.service"]
        );
        assert_eq!(
            unit_path(Scope::System, "tessera").unwrap(),
            PathBuf::from("/etc/systemd/system/tessera.service")
        );
        assert!(
            unit_path(Scope::User, "tessera-test")
                .unwrap()
                .ends_with("systemd/user/tessera-test.service")
        );
    }

    #[test]
    fn shell_choice() {
        let all = |_: &Path| true;
        let none = |_: &Path| false;
        assert_eq!(
            pick_shell(Some("/usr/bin/zsh"), all),
            PathBuf::from("/usr/bin/zsh")
        );
        assert_eq!(
            pick_shell(Some("/usr/bin/fish"), all),
            PathBuf::from("/usr/bin/bash")
        );
        assert_eq!(
            pick_shell(Some("/usr/sbin/nologin"), all),
            PathBuf::from("/usr/bin/bash")
        );
        assert_eq!(pick_shell(None, none), PathBuf::from("/bin/sh"));
        let only_bin = |p: &Path| p == Path::new("/bin/bash");
        assert_eq!(
            pick_shell(Some("zsh"), only_bin),
            PathBuf::from("/bin/bash")
        );
    }

    #[test]
    fn passwd_parsing() {
        let p =
            "root:x:0:0:root:/root:/bin/bash\nada:x:1000:1000:Ada:/home/ada:/usr/bin/zsh\nbroken\n";
        assert_eq!(
            passwd_entry(p, |_, u| u == 1000),
            Some(("ada".into(), "/home/ada".into(), "/usr/bin/zsh".into()))
        );
        assert_eq!(passwd_entry(p, |n, _| n == "root").unwrap().1, "/root");
        assert!(passwd_entry(p, |n, _| n == "nobody").is_none());
        assert!(valid_user_name("ada") && !valid_user_name("a b") && !valid_user_name(""));
    }

    #[test]
    fn bind_resolution() {
        let dir = std::env::temp_dir().join(format!("tessera-svc-{}", std::process::id()));
        let paths = Paths::at(&dir);
        paths.ensure().unwrap();
        let opts = |bind: Option<&str>, lan: bool| InstallOptions {
            bind: bind.map(str::to_string),
            lan,
            user: None,
            linger: Linger::No,
            dry_run: true,
            name: "tessera".into(),
            home: None,
            replace: false,
        };
        // Default: the config's (local-only) address.
        assert_eq!(
            resolve_bind(&opts(None, false), &paths).unwrap(),
            "127.0.0.1:7878"
        );
        assert_eq!(
            resolve_bind(&opts(None, true), &paths).unwrap(),
            "0.0.0.0:7878"
        );
        assert_eq!(
            resolve_bind(&opts(Some("192.168.1.5:9000"), true), &paths).unwrap(),
            "192.168.1.5:9000"
        );
        assert!(resolve_bind(&opts(Some("nope"), false), &paths).is_err());
        let mut cfg = Config::default();
        cfg.server.bind = "127.0.0.1:7900".into();
        cfg.save(&paths).unwrap();
        assert_eq!(
            resolve_bind(&opts(None, true), &paths).unwrap(),
            "0.0.0.0:7900"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
