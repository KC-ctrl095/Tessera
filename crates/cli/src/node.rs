//! `tessera node …`: this computer as a *machine* the user's other devices
//! can use through their Tessera server (see `tessera_core::node` and
//! docs/MACHINES.md).
//!
//! Everything here acts on this computer's own settings only, and only
//! from this computer: there is no server request behind any of it. The
//! desktop app runs these commands locally (`--json`, `--yes`); agents
//! can't, because the settings directory doesn't exist inside the sandbox
//! their commands run in, and these commands refuse to widen anything from
//! inside one.

use std::io::{IsTerminal, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Subcommand, ValueEnum};
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;
use tessera_client::{Client, ClientEvent, ConnState, ServerConfig};
use tessera_core::config::Paths;
use tessera_core::node::{self, Link, LinkEvent, NodeConfig, NodeState};
use tessera_proto::*;
use tokio_util::sync::CancellationToken;

use crate::backend;
use crate::service;

#[derive(Subcommand)]
pub enum NodeCmd {
    /// Show whether this computer is on as a machine, what it allows, and
    /// whether it is connected.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Turn this computer on as a machine. With a pairing link (from
    /// Settings → Devices → Add a machine) it pairs this computer first.
    Enable {
        /// The `tessera://pair…` link to pair this computer with your server.
        #[arg(value_name = "PAIRING_LINK")]
        link: Option<String>,
        /// The name your devices see (default: this computer's name).
        #[arg(long)]
        name: Option<String>,
        /// What agents may do here (default: ask, or nothing until you choose).
        #[arg(long, value_enum)]
        preset: Option<PresetArg>,
        /// A folder for `look` or `work` (repeatable).
        #[arg(long = "folder", value_name = "PATH")]
        folders: Vec<String>,
        /// Don't ask.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Read the server login to use from stdin (the desktop app's own pairing).
        #[arg(long)]
        server_stdin: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show or choose what agents may do here: off, look (read chosen
    /// folders), work (read and change them, run commands there), full
    /// (your home folder and commands), desktop (full, plus startup
    /// settings and your desktop session).
    Preset {
        #[arg(value_enum)]
        preset: Option<PresetArg>,
        /// The folders for `look` or `work` (repeatable; default: the ones chosen before).
        #[arg(long = "folder", value_name = "PATH")]
        folders: Vec<String>,
        /// Ask first: every action, only risky ones, or never (the chat's own mode decides).
        #[arg(long, value_enum)]
        ask: Option<AskArg>,
        /// Web views here (included in full and desktop; optional with work):
        /// pages opened here can reach this computer's local network.
        #[arg(long, value_enum)]
        webviews: Option<OnOff>,
        /// Don't ask before giving agents more.
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Who can use this computer: any chat, or only certain bots (and their
    /// subagents and scheduled duties).
    Access {
        /// Any chat may use it.
        #[arg(long, conflicts_with = "bots")]
        any: bool,
        /// Only this bot (its handle or id; repeatable).
        #[arg(long = "bot", value_name = "BOT")]
        bots: Vec<String>,
        /// Don't ask before letting more chats or bots use it.
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Startup files agents changed here (desktop control): list them, or
    /// put one back as it was.
    Restore {
        /// The change to undo (from the list).
        id: Option<u64>,
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Stop every agent here at once (what the Stop bar and its key do):
    /// running actions end, and nothing runs until you resume it here.
    Stop {
        #[arg(long)]
        json: bool,
    },
    /// Let agents use this computer again after a Stop (only here: no
    /// other device or agent can).
    Resume {
        #[arg(long)]
        json: bool,
    },
    /// Turn it off: running actions stop at once, and the background service is removed.
    Disable {
        #[arg(long)]
        json: bool,
    },
    /// Show or change what agents may do here.
    Scope {
        /// Replace the whole scope with this JSON (`MachineScope`).
        #[arg(long, value_name = "JSON")]
        set: Option<String>,
        /// Allow a folder (repeatable); read-only unless --write.
        #[arg(long = "add-folder", value_name = "PATH")]
        add: Vec<String>,
        /// The folders added with --add-folder are read-write.
        #[arg(long)]
        write: bool,
        /// Stop allowing a folder (repeatable).
        #[arg(long = "remove-folder", value_name = "PATH")]
        remove: Vec<String>,
        /// Let agents run commands here (in allowed folders).
        #[arg(long, value_enum)]
        commands: Option<OnOff>,
        /// Ask first: `always` (every action), `risky` (only risky ones) or
        /// `chat` (never: each chat's own mode decides).
        #[arg(long, value_enum)]
        ask: Option<AskArg>,
        /// Longest one action may run, in seconds.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Desktop control: startup files may change (backed up first) and
        /// commands get your desktop session.
        #[arg(long, value_enum)]
        desktop: Option<OnOff>,
        /// Web views here: chats may open web pages in a browser on this
        /// computer (they can reach its local network and localhost services).
        #[arg(long, value_enum)]
        webviews: Option<OnOff>,
        /// Terminal from my devices: your paired devices may open a terminal
        /// here, `sandboxed` (Tessera's own files out of reach, your startup
        /// files unchanged) or with `full` access. Agents never get one.
        /// Turning it on trusts your Tessera server with this account.
        #[arg(long, value_enum)]
        terminal: Option<TerminalArg>,
        /// Don't ask before widening.
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Administrator commands: agents may run commands here as this
    /// computer's administrator (root, with sudo), outside the sandbox. Off
    /// by default; on, each one asks you first, with the exact command,
    /// unless you choose `--ask chat`. `password` saves sudo's password on
    /// this computer (checked with sudo; never sent anywhere), `forget`
    /// removes it. Without an action: how it is now.
    Admin {
        #[arg(value_enum)]
        action: Option<AdminArg>,
        /// With `on`: ask before `every` administrator command (the
        /// default), or as each `chat`'s mode says (YOLO chats don't ask).
        #[arg(long, value_enum)]
        ask: Option<AdminAskArg>,
        /// With `password`: read it from stdin (one line) instead of asking.
        #[arg(long)]
        stdin: bool,
        /// Don't ask before giving agents more.
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Keep it on in the background (a user service that starts at login).
    Service {
        #[arg(value_enum)]
        action: ServiceAction,
        /// Also keep it running without a login (`loginctl enable-linger`).
        #[arg(long, conflicts_with = "no_linger")]
        linger: bool,
        #[arg(long)]
        no_linger: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show or change the name your devices and agents call this computer
    /// (only here: no other device can rename a machine).
    Name {
        /// The new name (1 to 60 characters).
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// What machines did here: every action, allowed or refused, newest first.
    Log {
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Run the machine in the foreground (what the background service runs).
    Run,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum TerminalArg {
    Off,
    Sandboxed,
    Full,
}

impl TerminalArg {
    fn access(self) -> tessera_proto::TerminalAccess {
        match self {
            TerminalArg::Off => tessera_proto::TerminalAccess::Off,
            TerminalArg::Sandboxed => tessera_proto::TerminalAccess::Sandboxed,
            TerminalArg::Full => tessera_proto::TerminalAccess::Full,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum AskArg {
    #[value(alias = "every")]
    Always,
    Risky,
    #[value(alias = "never")]
    Chat,
}

impl AskArg {
    fn ask(self) -> MachineAsk {
        match self {
            AskArg::Always => MachineAsk::Always,
            AskArg::Risky => MachineAsk::Risky,
            AskArg::Chat => MachineAsk::Chat,
        }
    }
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum PresetArg {
    Off,
    #[value(alias = "look-only")]
    Look,
    #[value(alias = "work-in-folders")]
    Work,
    #[value(alias = "full-access")]
    Full,
    #[value(alias = "desktop-control")]
    Desktop,
}

impl PresetArg {
    fn preset(self) -> Option<MachinePreset> {
        match self {
            PresetArg::Off => None,
            PresetArg::Look => Some(MachinePreset::LookOnly),
            PresetArg::Work => Some(MachinePreset::WorkInFolders),
            PresetArg::Full => Some(MachinePreset::FullAccess),
            PresetArg::Desktop => Some(MachinePreset::DesktopControl),
        }
    }
}

/// The warning before desktop control, in the terminal and the app alike.
pub const DESKTOP_WARNING: &str = if cfg!(windows) {
    DESKTOP_WARNING_WINDOWS
} else {
    "Desktop control is close to giving the agent your account. Agents could change what runs when you log in (shell startup files, autostart, your window manager's settings) and use your desktop session: open windows and programs, reload your window manager, change settings. Programs they start that way run as you, outside Tessera's sandbox: such a program could read your passwords and keys and change this computer's machine settings. Commands themselves still can't, and every startup file they change is backed up first so you can put it back. Turn it on only for a computer you'd let this agent use as you."
};

/// On Windows desktop control is about the screen, mouse and keyboard.
const DESKTOP_WARNING_WINDOWS: &str = "Desktop control is close to giving the agent your account. Agents can see your screens and use your mouse and keyboard while you're signed in: open programs, click, type and press shortcuts as you would, in anything you can reach yourself. They can't see or use the lock screen or Windows security prompts, or programs running as administrator, and it stays off while Tessera itself runs as administrator. Commands on Windows aren't sandboxed in any case (they need Full access). What agents type and the screenshots they take go to the chat's model and stay in the chat, like anything a tool returns; this computer's activity log and the Stop bar say only what kind of action it was. A bar at the top of your screen always shows what they do; its Stop button, or the Stop key it shows (usually Ctrl+Alt+Esc), stops every agent here at once until you resume. Turn it on only for a computer you'd let this agent use as you.";

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum AdminArg {
    On,
    Off,
    Password,
    Forget,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum AdminAskArg {
    #[value(alias = "always")]
    Every,
    #[value(alias = "never")]
    Chat,
}

/// The warning before administrator commands, in the terminal and the app alike.
pub const ADMIN_WARNING: &str = "Administrator lets agents run commands on this computer as root (with sudo), outside Tessera's sandbox. Such a command can change anything here: system files, other accounts, installed programs, and Tessera's own settings on this computer. Each one asks you first and shows you the exact command, unless you choose otherwise. Turn it on only for work you want done that way, and off again when you're done.";

#[derive(Clone, Copy, ValueEnum)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Status,
}

/// Run `tessera node …`; returns the exit code.
pub async fn main(paths: &Paths, cmd: NodeCmd, home: Option<PathBuf>) -> Result<i32> {
    let json = match &cmd {
        NodeCmd::Status { json }
        | NodeCmd::Enable { json, .. }
        | NodeCmd::Disable { json }
        | NodeCmd::Stop { json }
        | NodeCmd::Resume { json }
        | NodeCmd::Preset { json, .. }
        | NodeCmd::Access { json, .. }
        | NodeCmd::Restore { json, .. }
        | NodeCmd::Scope { json, .. }
        | NodeCmd::Service { json, .. }
        | NodeCmd::Name { json, .. }
        | NodeCmd::Admin { json, .. }
        | NodeCmd::Log { json, .. } => *json,
        NodeCmd::Run => false,
    };
    match dispatch(paths, cmd, home).await {
        Ok(code) => Ok(code),
        Err(e) if json => {
            println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
            eprintln!("{e:#}");
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

async fn dispatch(paths: &Paths, cmd: NodeCmd, home: Option<PathBuf>) -> Result<i32> {
    match cmd {
        NodeCmd::Status { json } => {
            let st = status(paths);
            if json {
                print_json(&st);
            } else {
                print_status(&st);
            }
            Ok(0)
        }
        NodeCmd::Enable {
            link,
            name,
            preset,
            folders,
            yes,
            server_stdin,
            json,
        } => {
            let opts = EnableOpts {
                link,
                name,
                preset,
                folders,
                yes,
                server_stdin,
                json,
            };
            enable(paths, opts, home).await
        }
        NodeCmd::Preset {
            preset,
            folders,
            ask,
            webviews,
            yes,
            json,
        } => match preset {
            Some(PresetArg::Off) => {
                disable(paths).await?;
                if json {
                    print_json(&status(paths));
                } else {
                    println!(
                        "This computer is off as a machine: nothing can use it until you turn it on here again."
                    );
                }
                Ok(0)
            }
            p => set_preset(
                paths,
                p.and_then(PresetArg::preset),
                folders,
                ask,
                webviews,
                yes,
                json,
            ),
        },
        NodeCmd::Access {
            any,
            bots,
            yes,
            json,
        } => access(paths, any, bots, yes, json),
        NodeCmd::Restore { id, limit, json } => restore(paths, id, limit, json),
        NodeCmd::Name { name, json } => rename(paths, name, json),
        NodeCmd::Stop { json } => stop_here(paths, true, json),
        NodeCmd::Resume { json } => stop_here(paths, false, json),
        NodeCmd::Disable { json } => {
            disable(paths).await?;
            if json {
                print_json(&status(paths));
            } else {
                println!(
                    "This computer is off as a machine: nothing can use it until you turn it on here again (`tessera node enable`)."
                );
            }
            Ok(0)
        }
        NodeCmd::Scope {
            set,
            add,
            write,
            remove,
            commands,
            ask,
            timeout,
            desktop,
            webviews,
            terminal,
            yes,
            json,
        } => {
            let change = ScopeChange {
                set,
                add,
                write,
                remove,
                commands,
                ask,
                timeout,
                desktop,
                webviews,
                terminal,
            };
            scope(paths, change, yes, json)
        }
        NodeCmd::Service {
            action,
            linger,
            no_linger,
            json,
        } => service_cmd(paths, action, linger, no_linger, home, json),
        NodeCmd::Admin {
            action,
            ask,
            stdin,
            yes,
            json,
        } => admin(paths, action, ask, stdin, yes, json),
        NodeCmd::Log { limit, json } => {
            let entries = node::log::NodeLog::read(&node::log_file(paths), limit.max(1));
            if json {
                print_json(&entries);
            } else if entries.is_empty() {
                println!("Nothing yet.");
            } else {
                for e in entries.iter().rev() {
                    println!("{}", log_line(e));
                }
            }
            Ok(0)
        }
        NodeCmd::Run => run(paths).await,
    }
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!(
        "{}",
        serde_json::to_string(v).unwrap_or_else(|_| "null".into())
    );
}

/// Commands an agent runs (here or on the server) carry `TESSERA=1`: they
/// must never turn a machine on or widen one. (The real guard is that the
/// settings don't exist in their sandbox; this explains it.)
fn refuse_from_agent() -> Result<()> {
    if std::env::var_os("TESSERA").is_some_and(|v| v == "1") {
        bail!(
            "this runs inside an agent's command; a machine is turned on and widened only by you, in your own terminal or the desktop app on that computer"
        );
    }
    Ok(())
}

/// `tessera node name [NEW]`: the running node tells the server within a
/// second (it watches its settings file).
fn rename(paths: &Paths, name: Option<String>, json: bool) -> Result<i32> {
    let mut config = NodeConfig::load(paths)?;
    if let Some(raw) = name {
        refuse_from_agent()?;
        let new = raw.trim();
        if new.is_empty() || new.chars().count() > 60 {
            bail!("a machine's name is 1 to 60 characters");
        }
        if new.chars().any(|c| c.is_control()) {
            bail!("a machine's name is one line of text");
        }
        if new != config.name {
            log(
                paths,
                NodeLogEvent::ScopeChanged,
                format!("renamed from {} to {new}", config.name),
            );
            config.name = new.to_string();
            config.save(paths)?;
        }
    }
    if json {
        print_json(&status(paths));
    } else {
        println!("{}", config.name);
    }
    Ok(0)
}

fn refuse_root() -> Result<()> {
    if service::running_as_root() {
        bail!("a machine runs as your normal user, never as root");
    }
    Ok(())
}

fn tty() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Ask a yes/no question on the terminal (`default` on an empty answer).
fn confirm(question: &str, default: bool) -> bool {
    print!("{question} {} ", if default { "[Y/n]" } else { "[y/N]" });
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => default,
        a => a.starts_with('y'),
    }
}

fn ask_line(question: &str, default: &str) -> String {
    print!("{question} [{default}]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let line = line.trim();
    if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    }
}

// ---- the server the node connects to ---------------------------------------

/// The login handed over on stdin by the desktop app: the CLI's
/// `client.json` shape, or a bare server config.
#[derive(Deserialize)]
struct StdinServer {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    server: Option<ServerConfig>,
    #[serde(default)]
    endpoints: Vec<String>,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    token: String,
    #[serde(default, alias = "serverKey")]
    server_key: String,
}

fn read_stdin_server() -> Result<backend::ClientFile> {
    let mut text = String::new();
    std::io::stdin()
        .take(64 * 1024)
        .read_to_string(&mut text)
        .context("reading the server login from stdin")?;
    let s: StdinServer =
        serde_json::from_str(text.trim()).context("the server login on stdin isn't valid JSON")?;
    let server = s.server.unwrap_or(ServerConfig {
        endpoints: s.endpoints,
        headers: s.headers,
        token: s.token,
        server_key: s.server_key,
    });
    let file = backend::ClientFile {
        name: s.name.unwrap_or_default(),
        server,
        machine: false,
        traded_from: String::new(),
        route: None,
    };
    check_server(&file.server)?;
    Ok(file)
}

/// A machine only ever connects end-to-end encrypted to a pinned key.
fn check_server(s: &ServerConfig) -> Result<()> {
    if s.endpoints.is_empty() || s.token.trim().is_empty() {
        bail!("the server login needs its addresses and this device's token");
    }
    if s.server_key.trim().is_empty() {
        bail!(
            "this login has no pinned server key, and a machine only connects end-to-end encrypted; pair this computer again (`tessera connect <pairing link>`)"
        );
    }
    Ok(())
}

/// A login that can only serve as this machine, for `login` (the desktop
/// app's pairing, or this computer's `tessera connect` login, which are
/// whole devices): asked of the server with that login, which keeps
/// working for the app or terminal. The machine's id stays that device's.
/// True when it is new, to be saved; a server from before machine logins
/// (or one out of reach) leaves `login` as it is, tried again next start.
/// `current`: the machine's saved login; when it was traded for this same
/// login it is kept (turning on again mustn't end the login a running node
/// uses), with the addresses as given now.
async fn machine_side(
    login: backend::ClientFile,
    current: Option<backend::ClientFile>,
) -> (backend::ClientFile, bool) {
    if login.machine {
        return (login, false);
    }
    let from = tessera_core::util::sha256_hex(login.server.token.as_bytes());
    if let Some(cur) = current.filter(|c| {
        c.machine && c.traded_from == from && c.server.server_key == login.server.server_key
    }) {
        let changed = cur.name != login.name
            || cur.server.endpoints != login.server.endpoints
            || cur.server.headers != login.server.headers;
        let kept = backend::ClientFile {
            server: ServerConfig {
                token: cur.server.token,
                ..login.server
            },
            machine: true,
            traded_from: from,
            name: login.name,
            route: cur.route,
        };
        return (kept, changed);
    }
    match tessera_client::machine_login(&login.server).await {
        Ok(server) => (
            backend::ClientFile {
                server,
                machine: true,
                traded_from: from,
                ..login
            },
            true,
        ),
        Err(e) => {
            tracing::warn!("keeping the device's own login for now ({e})");
            (login, false)
        }
    }
}

/// The login this machine saved for itself (`node-server.json`).
fn saved_node_login(paths: &Paths) -> Option<backend::ClientFile> {
    std::fs::read_to_string(node::server_file(paths))
        .ok()
        .and_then(|t| serde_json::from_str::<backend::ClientFile>(&t).ok())
}

fn save_server_login(paths: &Paths, file: &backend::ClientFile) -> Result<()> {
    tessera_core::config::write_atomic(
        &node::server_file(paths),
        serde_json::to_string_pretty(file)?.as_bytes(),
        true,
    )
}

/// The server this machine connects to: the app's login given to the node,
/// else this computer's `tessera connect` login.
fn server_login(paths: &Paths) -> Option<backend::ClientFile> {
    saved_node_login(paths).or_else(|| backend::load_client_file(paths))
}

// ---- status ------------------------------------------------------------------

fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        tessera_core::platform::windows::alive(pid)
            && tessera_core::platform::windows::runs(pid, "tessera")
    }
    #[cfg(not(windows))]
    {
        if cfg!(target_os = "linux") {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| {
                let c = String::from_utf8_lossy(&c);
                c.contains("tessera") && c.contains("node")
            })
        } else {
            true
        }
    }
}

pub fn status(paths: &Paths) -> NodeStatus {
    let config = NodeConfig::load(paths).unwrap_or_default();
    let home = node::home();
    let preset = node::preset_of(&config.scope, home.as_deref());
    let login = server_login(paths);
    let paired = login
        .as_ref()
        .is_some_and(|l| check_server(&l.server).is_ok());
    let state = NodeState::load(paths).filter(|s| alive(s.pid));
    let running = state.is_some();
    let st = state.unwrap_or_default();
    let error = if !paired {
        Some(match &login {
            Some(_) => "this computer's login has no pinned server key; pair it again (`tessera connect <pairing link>`)".to_string(),
            None => "this computer isn't paired with a Tessera server yet (`tessera connect <pairing link>`)".to_string(),
        })
    } else if running {
        st.error.clone().filter(|_| !st.connected)
    } else {
        None
    };
    let commands_run = node::commands_run_here(&config.scope);
    NodeStatus {
        enabled: config.enabled,
        revoked: config.revoked,
        name: config.name,
        version: env!("CARGO_PKG_VERSION").into(),
        os: node::os(),
        features: Some(node::features()),
        scope: config.scope,

        paired,
        server: st.server.clone().or_else(|| {
            login
                .as_ref()
                .map(|l| l.name.clone())
                .filter(|n| !n.is_empty())
                .or_else(|| {
                    login
                        .as_ref()
                        .and_then(|l| l.server.endpoints.first().cloned())
                })
        }),
        running,
        pid: running.then_some(st.pid),
        connected: running && st.connected,
        endpoint: st.endpoint.filter(|_| running && st.connected),
        paused: running && st.paused,
        error,
        calls: if running { st.calls } else { 0 },
        sandbox: tessera_core::sandbox::describe(),
        sandbox_ok: commands_run,
        service: service::node_service_state(),
        log_file: node::log_file(paths).display().to_string(),
        preset,
        home: home
            .as_ref()
            .map(|h| h.display().to_string())
            .unwrap_or_default(),
        desktop_session: node::desktop::discover()
            .map(|s| s.label)
            .or_else(node::screen::session_label),
        bus_filter: node::desktop::bus_filter_program().is_some(),
        startup_changes: node::backups::Backups::new(node::backups::dir(paths)).count(),
        admin_password: node::admin::password(paths).is_some(),
        stop_bar: if running { st.stop_bar.clone() } else { None },
        guard: tessera_core::guard::status_for(
            st.guard_error.clone().filter(|_| running).map(Err).as_ref(),
        ),
    }
}

fn print_status(st: &NodeStatus) {
    let state = if st.revoked && !st.enabled {
        "off (turned off from another device; `tessera node enable` turns it on again)".to_string()
    } else if !st.enabled {
        "off (`tessera node enable` turns it on)".to_string()
    } else if st.connected {
        format!(
            "on, connected to {}{}",
            st.server.as_deref().unwrap_or("the server"),
            if st.paused {
                " (paused from another device)"
            } else {
                ""
            }
        )
    } else if st.running {
        format!(
            "on, connecting{}",
            st.error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        )
    } else {
        "on, but not running (`tessera node run`, or `tessera node service install` to keep it on)"
            .to_string()
    };
    println!("{}: {state}", st.name);
    if st.scope.stopped {
        println!(
            "  STOPPED here: agents can't use this computer until you resume it (`tessera node resume`, This computer in the app, or Resume on the Stop bar)"
        );
    }
    let home = (!st.home.is_empty()).then(|| PathBuf::from(&st.home));
    println!("{}", node::describe_scope(&st.scope, home.as_deref()));
    if st.scope.desktop {
        println!(
            "  session:  {}",
            match (&st.desktop_session, st.bus_filter) {
                (Some(s), true) => s.clone(),
                // No session bus to filter there.
                (Some(s), false) if cfg!(windows) => s.clone(),
                (Some(s), false) => format!(
                    "{s} (no session bus for commands: install xdg-dbus-proxy for notifications and settings)"
                ),
                (None, _) => "none running now".into(),
            }
        );
        if let Some(bar) = &st.stop_bar {
            println!("  stop bar: {bar}");
        }
    }
    if st.startup_changes > 0 {
        println!(
            "  startup files changed by agents: {} (`tessera node restore` lists them)",
            st.startup_changes
        );
    }
    if st.scope.admin != AdminAccess::Off {
        println!(
            "  sudo:     {}",
            if st.admin_password {
                "its password is saved here (never sent anywhere; `tessera node admin forget` removes it)"
            } else {
                "no password saved here (`tessera node admin password` saves it, if sudo needs one)"
            }
        );
    }
    if st.scope.commands && !st.sandbox_ok {
        if cfg!(windows) {
            println!(
                "  note: commands are refused here: {}",
                node::commands_refused_why()
            );
        } else {
            println!(
                "  note: commands are refused here: they need the sandbox with Landlock, which is {}",
                st.sandbox
            );
        }
    }
    if st.scope.commands
        && st.sandbox_ok
        && !st.scope.desktop
        && tessera_core::sandbox::support().landlock < 6
    {
        println!(
            "  note: on this Linux (older than 6.12) commands could reach an X display's socket; they don't get its login cookie (~/.Xauthority reads as empty), so the display turns them away"
        );
    }
    if st.calls > 0 {
        println!("  running:  {} action(s)", st.calls);
    }
    if !st.paired {
        println!(
            "  {}",
            st.error.as_deref().unwrap_or("not paired with a server")
        );
    }
    if st.service.available {
        println!(
            "  stay on:  {}",
            match (st.service.installed, st.service.active) {
                (false, _) => "no (`tessera node service install` keeps it on in the background)",
                (true, true) => "yes, running in the background",
                (true, false) => "installed, not running",
            }
        );
    }
    println!("  log:      {}", st.log_file);
}

fn log_line(e: &NodeLogEntry) -> String {
    let when = tessera_core::jobs::fmt_time(e.ts);
    let what = serde_json::to_value(e.event)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut line = format!("{when}  {what:<13}");
    if let Some(a) = &e.action {
        line.push_str(&format!(" {a}"));
    }
    if let Some(t) = &e.target {
        line.push_str(&format!(" {}", tessera_core::util::one_line(t, 80)));
    }
    if let Some(c) = e.chat.as_deref().filter(|c| !c.is_empty()) {
        line.push_str(&format!(
            "  · chat \"{}\"",
            tessera_core::util::one_line(c, 40)
        ));
    }
    if !e.detail.is_empty() {
        line.push_str(&format!(
            "  · {}",
            tessera_core::util::one_line(&e.detail, 120)
        ));
    }
    if let Some(ms) = e.duration_ms {
        line.push_str(&format!("  · {:.1}s", ms as f64 / 1000.0));
    }
    line
}

fn log(paths: &Paths, event: NodeLogEvent, detail: impl Into<String>) {
    node::log::NodeLog::new(node::log_file(paths)).add(&node::log::entry(event, detail));
}

// ---- enable / disable --------------------------------------------------------

pub struct EnableOpts {
    link: Option<String>,
    name: Option<String>,
    preset: Option<PresetArg>,
    folders: Vec<String>,
    yes: bool,
    server_stdin: bool,
    json: bool,
}

/// Pair this computer from a pairing link, for the machine alone (the
/// login goes to `node-server.json`; this computer's own `tessera` stays
/// as it was).
async fn pair_from_link(link: &str, name: &str) -> Result<backend::ClientFile> {
    let (parsed, _endpoint, grant) = tessera_client::pair_with_link_as(link.trim(), name, true)
        .await
        .context("pairing with your server")?;
    let file = backend::ClientFile {
        name: Some(parsed.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| grant.server.name.clone()),
        server: ServerConfig {
            endpoints: parsed.urls.clone(),
            headers: parsed.headers.iter().cloned().collect(),
            token: grant.token,
            server_key: grant.server_key,
        },
        machine: grant.machine,
        traded_from: String::new(),
        route: None,
    };
    check_server(&file.server)?;
    Ok(file)
}

fn preset_choices(os: &str) -> Vec<(PresetArg, MachinePreset)> {
    let mut options = vec![
        (PresetArg::Look, MachinePreset::LookOnly),
        (PresetArg::Work, MachinePreset::WorkInFolders),
        (PresetArg::Full, MachinePreset::FullAccess),
    ];
    options.retain(|(p, _)| *p != PresetArg::Full || os != "macos");
    options
}
/// Ask which preset, in the terminal.
fn choose_preset() -> Option<PresetArg> {
    println!("What may agents do on this computer?");
    let options = preset_choices(std::env::consts::OS);
    let features = node::features();
    for (i, (_, p)) in options.iter().enumerate() {
        println!(
            "  {}) {:<16} {}",
            i + 1,
            node::preset_name(*p),
            node::preset_sentence(*p)
        );
    }
    if features
        .iter()
        .any(|c| c == "desktop" || c == "desktop_session")
    {
        println!("  (Desktop control: `tessera node preset desktop` afterwards, with a warning.)");
    }
    let answer = ask_line("Choose", if cfg!(target_os = "linux") { "2" } else { "1" });
    answer
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| options.get(i).map(|(p, _)| *p))
}

async fn enable(paths: &Paths, o: EnableOpts, home: Option<PathBuf>) -> Result<i32> {
    refuse_root()?;
    refuse_from_agent()?;
    let interactive = tty() && !o.yes && !o.server_stdin;
    if !interactive && !o.yes {
        bail!(
            "turning this computer on as a machine needs confirmation: run it in a terminal, or add --yes"
        );
    }
    if o.preset == Some(PresetArg::Desktop) && !interactive && !o.yes {
        bail!("desktop control needs confirmation: run it in a terminal, or add --yes");
    }
    let mut config = NodeConfig::load(paths)?;
    let mut new_name = o
        .name
        .clone()
        .map(|n| tessera_core::util::one_line(&n, 60))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| config.name.clone());
    let given = if o.server_stdin {
        Some(read_stdin_server()?)
    } else if let Some(link) = &o.link {
        if interactive {
            new_name = ask_line("Name your devices see", &new_name);
        }
        let file = pair_from_link(link, &new_name).await?;
        if !o.json {
            println!("✓ Paired with {}, end-to-end encrypted.", file.name);
        }
        Some(file)
    } else {
        None
    };
    let login = given.clone().or_else(|| server_login(paths)).ok_or_else(|| {
        anyhow!(
            "pair this computer with your Tessera server first: run the command from Settings → Devices → Add a machine (`tessera node enable '<pairing link>'`)"
        )
    })?;
    check_server(&login.server)?;
    let server_name = if login.name.is_empty() {
        login.server.endpoints.first().cloned().unwrap_or_default()
    } else {
        login.name.clone()
    };
    if interactive && o.link.is_none() {
        println!(
            "Turn this computer into a machine for {server_name}?\n\n\
             Agents in your chats, from any of your devices, will be able to use it, but only within what you allow here, and only you can change that, on this computer.\n"
        );
        new_name = ask_line("Name your devices see", &new_name);
        if !confirm("Turn it on?", false) {
            println!("Nothing changed.");
            return Ok(1);
        }
    }
    // What the machine keeps is a login that can only serve as it.
    let (login, new_login) = machine_side(login, saved_node_login(paths)).await;
    if given.is_some() || new_login {
        save_server_login(paths, &login)?;
    }
    config.enabled = true;
    config.revoked = false;
    config.generation += 1;
    config.name = tessera_core::util::one_line(&new_name, 60);
    config.save(paths)?;
    log(
        paths,
        NodeLogEvent::Enabled,
        format!("turned on as {} for {server_name}", config.name),
    );
    // What it allows: as asked, or chosen now.
    let preset = match o.preset {
        Some(p) => Some(p),
        None if interactive => choose_preset(),
        None => None,
    };
    if let Some(p) = preset.filter(|p| *p != PresetArg::Off) {
        let folders = if o.folders.is_empty()
            && interactive
            && matches!(p, PresetArg::Look | PresetArg::Work)
            && config.chosen.is_empty()
        {
            let line = ask_line("Folders agents may use (comma-separated)", "~/code");
            line.split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_string)
                .collect()
        } else {
            o.folders.clone()
        };
        if p == PresetArg::Desktop && interactive && !confirm_desktop() {
            println!(
                "Desktop control stays off; choose another preset with `tessera node preset`."
            );
        } else {
            set_preset(paths, p.preset(), folders, None, None, true, true).map(|_| ())?;
        }
    }
    if o.json {
        print_json(&status(paths));
        return Ok(0);
    }
    let config = NodeConfig::load(paths)?;
    let h = node::home();
    println!("✓ {} is on as a machine for {server_name}.", config.name);
    println!("{}", node::describe_scope(&config.scope, h.as_deref()));
    if config.scope.folders.is_empty() {
        println!(
            "  It allows nothing yet. Choose: tessera node preset work --folder ~/code   (or look, full)"
        );
    }
    let svc = service::node_service_state();
    if interactive && svc.available && !svc.active {
        if confirm(
            "Keep it on in the background (starts when you log in)?",
            true,
        ) {
            let linger = service::wants_linger_hint()
                && confirm(
                    "Also keep it running when you're logged out? (loginctl enable-linger)",
                    true,
                );
            service::node_install(home.as_deref().or(home_of(paths).as_deref()), linger)?;
            println!("✓ Running in the background (tessera-node.service).");
        } else {
            println!(
                "  Bring it online with `tessera node run` (or `tessera node service install`)."
            );
        }
    } else if svc.active {
        println!("  The background service picks this up by itself.");
    } else {
        println!("  Bring it online with `tessera node run` (or `tessera node service install`).");
    }
    Ok(0)
}

/// The desktop control warning, answered in the terminal.
fn confirm_desktop() -> bool {
    println!("\n{DESKTOP_WARNING}\n");
    confirm("Turn on desktop control?", false)
}

/// `TESSERA_HOME` for the service, when this CLI uses one.
fn home_of(paths: &Paths) -> Option<PathBuf> {
    (paths.config_dir == paths.data_dir).then(|| paths.config_dir.clone())
}

/// Turn it off: settings first (a running node notices within a second),
/// then stop the node and its service at once.
/// `tessera node stop` / `resume`: written to `stop.json`, which the running
/// machine applies within a second (the Stop bar writes it the same way).
/// Only Resume is refused inside an agent's command.
fn stop_here(paths: &Paths, stopped: bool, json: bool) -> Result<i32> {
    if !stopped {
        refuse_from_agent()?;
        // With Tessera Guard (Windows) only a Resume approved in Windows'
        // administrator prompt counts: the guard's own program is raised,
        // and the user approves it there.
        if !matches!(
            tessera_core::guard::here(),
            tessera_core::guard::Here::Absent
        ) {
            tessera_core::guard::resume_with_approval()
                .map_err(|why| anyhow::anyhow!("not resumed: {why}"))?;
        }
    }
    node::StopLatch::set(paths, stopped, 0)?;
    if json {
        print_json(&status(paths));
    } else if stopped {
        println!(
            "Stopped: running actions end now, and agents can't use this computer until you resume it here (`tessera node resume`, This computer in the app, or Resume on the Stop bar)."
        );
    } else {
        println!(
            "Resumed: agents can use this computer again as its settings allow. Chats that were allowed to use its screen ask again."
        );
    }
    Ok(0)
}

pub async fn disable(paths: &Paths) -> Result<()> {
    let mut config = NodeConfig::load(paths)?;
    let was = config.enabled;
    config.enabled = false;
    config.save(paths)?;
    if was {
        log(paths, NodeLogEvent::Disabled, "turned off on this computer");
    }
    if let Some(st) = NodeState::load(paths).filter(|s| alive(s.pid)) {
        #[cfg(unix)]
        unsafe {
            libc::kill(st.pid as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        tessera_core::platform::windows::request_stop(st.pid);
        for _ in 0..40 {
            if !alive(st.pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    service::node_uninstall()?;
    Ok(())
}

// ---- scope -------------------------------------------------------------------

struct ScopeChange {
    set: Option<String>,
    add: Vec<String>,
    write: bool,
    remove: Vec<String>,
    commands: Option<OnOff>,
    ask: Option<AskArg>,
    timeout: Option<u64>,
    desktop: Option<OnOff>,
    webviews: Option<OnOff>,
    terminal: Option<TerminalArg>,
}

impl ScopeChange {
    fn is_empty(&self) -> bool {
        self.set.is_none()
            && self.add.is_empty()
            && self.remove.is_empty()
            && self.commands.is_none()
            && self.ask.is_none()
            && self.timeout.is_none()
            && self.desktop.is_none()
            && self.webviews.is_none()
            && self.terminal.is_none()
    }

    fn apply(&self, paths: &Paths, current: &MachineScope) -> Result<MachineScope> {
        let mut scope = match &self.set {
            Some(text) => serde_json::from_str::<MachineScope>(text)
                .context("--set takes a MachineScope as JSON")?,
            None => current.clone(),
        };
        let home = node::home();
        for raw in &self.remove {
            let full = node::expand(raw.trim(), home.as_deref());
            let canon = tessera_core::platform::canonicalize(&full).unwrap_or(full.clone());
            scope.folders.retain(|f| {
                let p = Path::new(&f.path);
                p != full && p != canon && f.path != raw.trim()
            });
        }
        for raw in &self.add {
            scope.folders.push(FolderAccess {
                path: raw.clone(),
                write: self.write,
            });
        }
        if let Some(c) = self.commands {
            scope.commands = matches!(c, OnOff::On);
        }
        if let Some(a) = self.ask {
            scope.ask = a.ask();
        }
        if let Some(t) = self.timeout {
            scope.timeout_secs = t;
        }
        if let Some(d) = self.desktop {
            scope.desktop = matches!(d, OnOff::On);
        }
        if let Some(w) = self.webviews {
            scope.webviews = matches!(w, OnOff::On);
        }
        if let Some(t) = self.terminal {
            scope.terminal = t.access();
        }
        node::check_scope(paths, &scope, home.as_deref()).map_err(|e| anyhow!(e))
    }
}

fn scope(paths: &Paths, change: ScopeChange, yes: bool, json: bool) -> Result<i32> {
    let config = NodeConfig::load(paths)?;
    if change.is_empty() {
        if json {
            print_json(&status(paths));
        } else {
            println!(
                "{}",
                node::describe_scope(&config.scope, node::home().as_deref())
            );
        }
        return Ok(0);
    }
    let new = change.apply(paths, &config.scope)?;
    save_scope(paths, config, new, yes, json)
}

/// Save a new scope: what gives agents more is confirmed first (in the
/// terminal, or `--yes`), and only ever here, never from an agent's command.
fn save_scope(
    paths: &Paths,
    mut config: NodeConfig,
    new: MachineScope,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let home = node::home();
    let wider = node::widening(&config.scope, &new);
    if !wider.is_empty() {
        refuse_root()?;
        refuse_from_agent()?;
        if !yes {
            if !tty() {
                bail!(
                    "this gives agents more ({}); confirm in a terminal, or add --yes",
                    wider.join("; ")
                );
            }
            if new.desktop && !config.scope.desktop && !confirm_desktop() {
                println!("Nothing changed.");
                return Ok(1);
            }
            if new.admin != AdminAccess::Off && config.scope.admin == AdminAccess::Off {
                println!("{ADMIN_WARNING}\n");
            }
            println!("This gives agents more on this computer:");
            for w in &wider {
                println!("  · {w}");
            }
            if !confirm("Go ahead?", false) {
                println!("Nothing changed.");
                return Ok(1);
            }
        }
    }
    if new == config.scope {
        if json {
            print_json(&status(paths));
        } else {
            println!(
                "Nothing to change.\n{}",
                node::describe_scope(&new, home.as_deref())
            );
        }
        return Ok(0);
    }
    config.set_scope(new, home.as_deref());
    config.save(paths)?;
    let rw = config.scope.folders.iter().filter(|f| f.write).count();
    log(
        paths,
        NodeLogEvent::ScopeChanged,
        format!(
            "{}: {} folder(s) ({rw} read-write), commands {}{}, asks first: {}, who: {}, limit {}",
            node::preset_name(node::preset_of(&config.scope, home.as_deref())),
            config.scope.folders.len(),
            if config.scope.commands { "on" } else { "off" },
            match (config.scope.desktop, config.scope.admin) {
                (true, AdminAccess::Off) => ", desktop control on",
                (true, _) => ", desktop control on, administrator commands on",
                (false, AdminAccess::Off) => "",
                (false, _) => ", administrator commands on",
            },
            node::ask_name(config.scope.ask),
            node::who_can_use(&config.scope, |_| None),
            node::human_secs(config.scope.timeout_secs)
        ),
    );
    if json {
        print_json(&status(paths));
    } else {
        println!("{}", node::describe_scope(&config.scope, home.as_deref()));
        if config.scope.commands && !node::commands_run_here(&config.scope) {
            if cfg!(windows) {
                println!(
                    "  note: commands will be refused here: {}",
                    node::commands_refused_why()
                );
            } else {
                println!(
                    "  note: commands will be refused here: they need the sandbox with Landlock ABI 3 (Linux 6.2+), which is {}",
                    tessera_core::sandbox::describe()
                );
            }
        }
    }
    Ok(0)
}

// ---- presets, who can use it, putting startup files back ------------------------

fn set_preset(
    paths: &Paths,
    preset: Option<MachinePreset>,
    folders: Vec<String>,
    ask: Option<AskArg>,
    webviews: Option<OnOff>,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let config = NodeConfig::load(paths)?;
    let home = node::home();
    let Some(preset) = preset else {
        if ask.is_none() && webviews.is_none() {
            if json {
                print_json(&status(paths));
            } else {
                let p = node::preset_of(&config.scope, home.as_deref());
                println!("{}: {}", node::preset_name(p), node::preset_sentence(p));
                println!("{}", node::describe_scope(&config.scope, home.as_deref()));
                println!(
                    "\nChoose with: tessera node preset look|work|full|desktop|off [--folder PATH]… [--ask every|risky|never]"
                );
            }
            return Ok(0);
        }
        // Only "Ask first", or web views.
        let mut new = config.scope.clone();
        new.ask = ask.map(AskArg::ask).unwrap_or(new.ask);
        if let Some(w) = webviews {
            new.webviews = matches!(w, OnOff::On);
        }
        return save_scope(paths, config, new, yes, json);
    };
    let mut new = node::apply_preset(
        paths,
        &config,
        preset,
        (!folders.is_empty()).then_some(folders),
        home.as_deref(),
    )
    .map_err(|e| anyhow!(e))?;
    if let Some(a) = ask {
        new.ask = a.ask();
    }
    if let Some(w) = webviews {
        new.webviews = matches!(w, OnOff::On);
    }
    save_scope(paths, config, new, yes, json)
}

fn access(paths: &Paths, any: bool, bots: Vec<String>, yes: bool, json: bool) -> Result<i32> {
    let config = NodeConfig::load(paths)?;
    if !any && bots.is_empty() {
        if json {
            print_json(&status(paths));
        } else {
            println!(
                "Who can use it: {}",
                node::who_can_use(&config.scope, |_| None)
            );
            println!("Change with: tessera node access --any   or   --bot <handle> (repeatable)");
        }
        return Ok(0);
    }
    let mut new = config.scope.clone();
    new.bots = if any { None } else { Some(bots) };
    let new = node::check_scope(paths, &new, node::home().as_deref()).map_err(|e| anyhow!(e))?;
    save_scope(paths, config, new, yes, json)
}

fn restore(paths: &Paths, id: Option<u64>, limit: usize, json: bool) -> Result<i32> {
    let backups = node::backups::Backups::new(node::backups::dir(paths));
    let Some(id) = id else {
        let list = backups.list(limit.max(1));
        if json {
            print_json(&list);
        } else if list.is_empty() {
            println!("No startup file has been changed by an agent here.");
        } else {
            for c in list.iter().rev() {
                println!(
                    "{:>4}  {}  {:<8} {}{}  · by {}",
                    c.id,
                    tessera_core::jobs::fmt_time(c.ts),
                    c.action,
                    c.path,
                    match (c.existed, c.exists) {
                        (false, true) => " (new)",
                        (true, false) => " (deleted)",
                        _ => "",
                    },
                    if c.by.is_empty() { "?" } else { &c.by }
                );
            }
            println!("\nPut one back as it was before: tessera node restore <number>");
        }
        return Ok(0);
    };
    refuse_from_agent()?;
    let roots = node::startup_paths(node::home().as_deref());
    let c = backups.restore(id, &roots).map_err(|e| anyhow!(e))?;
    log(
        paths,
        NodeLogEvent::Restored,
        format!("{} put back as it was before change {id}", c.path),
    );
    if json {
        print_json(&c);
    } else {
        println!(
            "✓ {} is back as it was before change {id}{}.",
            c.path,
            if c.existed {
                ""
            } else {
                " (removed: it didn't exist then)"
            }
        );
    }
    Ok(0)
}

// ---- service -----------------------------------------------------------------

fn service_cmd(
    paths: &Paths,
    action: ServiceAction,
    linger: bool,
    no_linger: bool,
    home: Option<PathBuf>,
    json: bool,
) -> Result<i32> {
    match action {
        ServiceAction::Install => {
            refuse_root()?;
            refuse_from_agent()?;
            let config = NodeConfig::load(paths)?;
            if !config.enabled {
                bail!("turn this computer on as a machine first: `tessera node enable`");
            }
            let linger = if linger {
                true
            } else if no_linger || json || !tty() {
                false
            } else {
                service::wants_linger_hint()
                    && confirm(
                        "Also keep it running when you're logged out? (loginctl enable-linger)",
                        true,
                    )
            };
            service::node_install(home.as_deref().or(home_of(paths).as_deref()), linger)?;
            if !json {
                println!(
                    "✓ {} stays on in the background (tessera-node.service, starts when you log in).",
                    config.name
                );
            }
        }
        ServiceAction::Uninstall => {
            service::node_uninstall()?;
            if !json {
                println!(
                    "Removed the background service; the machine stays as it is (`tessera node disable` turns it off)."
                );
            }
        }
        ServiceAction::Status => {}
    }
    if json {
        print_json(&status(paths));
    } else if matches!(action, ServiceAction::Status) {
        let s = service::node_service_state();
        println!(
            "{}",
            if !s.available {
                "Background services aren't available here (Linux with systemd, as your own user)."
            } else if !s.installed {
                "Not installed (`tessera node service install`)."
            } else if s.active {
                "Running in the background."
            } else {
                "Installed, not running."
            }
        );
    }
    Ok(0)
}

// ---- other devices: list, pause, resume, revoke --------------------------------

/// `tessera machines`: the server's machines, and the only things another
/// device may do to one: pause, resume, revoke.
pub async fn machines(
    backend: &crate::backend::Backend,
    pause: Option<String>,
    resume: Option<String>,
    revoke: Option<String>,
    json: bool,
) -> Result<i32> {
    let list: Vec<MachineInfo> = backend.call_as(Request::ListMachines).await?;
    let find = |want: &str| {
        list.iter()
            .find(|m| m.id == want)
            .or_else(|| list.iter().find(|m| m.name.eq_ignore_ascii_case(want)))
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow!("no machine `{want}`"))
    };
    if let Some(m) = pause.as_deref().or(resume.as_deref()) {
        let id = find(m)?;
        backend
            .call(Request::SetMachinePaused {
                id,
                paused: pause.is_some(),
            })
            .await?;
    }
    if let Some(m) = revoke.as_deref() {
        let id = find(m)?;
        backend.call(Request::RevokeMachine { id }).await?;
        if !json {
            println!(
                "Revoked: it stopped and turned itself off; only that computer can turn it on again."
            );
        }
    }
    let list: Vec<MachineInfo> = backend.call_as(Request::ListMachines).await?;
    if json {
        print_json(&list);
        return Ok(0);
    }
    if list.is_empty() {
        println!(
            "No machines. Turn a computer on as one there: `tessera node enable` (see docs/MACHINES.md)."
        );
    }
    for m in &list {
        let state = match (m.online, m.paused) {
            (_, true) => "paused".to_string(),
            (true, false) if m.running > 0 => format!("online, {} running", m.running),
            (true, false) => "online".into(),
            (false, false) => format!(
                "offline since {}",
                tessera_core::jobs::fmt_time(m.last_seen)
            ),
        };
        println!("{}  {} ({}) · {state}", m.id, m.name, m.os);
        println!(
            "{}",
            node::describe_scope(&m.scope, m.home.as_deref().map(Path::new))
        );
        for a in m.recent.iter().take(3) {
            println!(
                "    {}  {} {} · {}{}",
                tessera_core::jobs::fmt_time(a.ts),
                a.action,
                tessera_core::util::one_line(&a.target, 60),
                a.outcome,
                a.bot
                    .as_deref()
                    .map(|b| format!(" · {b}"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(0)
}

// ---- run -----------------------------------------------------------------------

/// The paired device's encrypted connection, as the node's link.
struct ClientLink(Client);

impl Link for ClientLink {
    fn call(&self, req: Request) -> BoxFuture<'static, Result<Value, ApiError>> {
        let c = self.0.clone();
        Box::pin(async move { c.call_timeout(req, Duration::from_secs(60)).await })
    }
}

/// Connection events, as the node sees them.
pub fn pump(client: &Client) -> tokio::sync::mpsc::UnboundedReceiver<LinkEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut events = client.subscribe();
    let client = client.clone();
    tokio::spawn(async move {
        let mut up = false;
        if client.status().state == ConnState::Connected {
            up = true;
            let _ = tx.send(LinkEvent::Up(client.status().endpoint));
        }
        loop {
            let ev = match events.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let sent = match ev {
                ClientEvent::Connected => {
                    up = true;
                    tx.send(LinkEvent::Up(client.status().endpoint))
                }
                ClientEvent::Status(s) if s.state != ConnState::Connected => {
                    if up {
                        up = false;
                        tx.send(LinkEvent::Down(s.error))
                    } else {
                        Ok(())
                    }
                }
                ClientEvent::Event(e) => tx.send(LinkEvent::Event(e)),
                ClientEvent::Status(_) => Ok(()),
            };
            if sent.is_err() {
                break;
            }
        }
    });
    rx
}

async fn run(paths: &Paths) -> Result<i32> {
    refuse_root()?;
    // Started by the desktop app: nothing before it has tied this machine
    // to its own lifetime (it ends with the app, however the app ends).
    #[cfg(windows)]
    tessera_core::platform::windows::wait_start_gate().context(
        "this machine wasn't tied to the Tessera app that started it, so it doesn't run",
    )?;
    let config = NodeConfig::load(paths)?;
    if !config.enabled {
        eprintln!(
            "This computer isn't on as a machine{}; turn it on here with `tessera node enable`.",
            if config.revoked {
                " (it was turned off from another device)"
            } else {
                ""
            }
        );
        // Not a failure: a background service stays stopped.
        return Ok(0);
    }
    let login = server_login(paths).ok_or_else(|| {
        anyhow!(
            "this computer isn't paired with a Tessera server (`tessera connect <pairing link>`)"
        )
    })?;
    check_server(&login.server)?;
    // One node per computer (per Tessera home): a second would race the
    // first for the same machine.
    let _lock = single_instance(paths)?;
    // Turned on before machine logins existed: from now on it keeps one.
    let (login, new_login) = machine_side(login, None).await;
    if new_login {
        save_server_login(paths, &login)?;
        tracing::info!("this machine now has a login that can only serve as it");
    }
    let node = node::Node::open(paths.clone())?;
    let client = Client::connect(login.server.clone());
    // Away from home it needs the server's remote address and Access key
    // too: kept up as the server gives them (a machine on the server's own
    // computer has nothing to learn).
    // (Only its own machine login: a device's login it borrows is that
    // device's to keep up.)
    if login.machine {
        let (p, q) = (paths.clone(), paths.clone());
        backend::follow_route(
            &client,
            move || server_login(&p),
            move |f| save_server_login(&q, f),
        );
    }
    let events = pump(&client);
    let stop = CancellationToken::new();
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            stop_signal().await;
            stop.cancel();
        });
    }
    tracing::info!(
        "machine `{}` connecting to {}",
        config.name,
        if login.name.is_empty() {
            "the server"
        } else {
            &login.name
        }
    );
    let exit = node
        .serve(Arc::new(ClientLink(client.clone())), events, stop)
        .await;
    client.close();
    match exit {
        node::Exit::Disabled => eprintln!("Turned off on this computer."),
        node::Exit::Revoked => eprintln!(
            "Turned off from another device; only this computer can turn it on again (`tessera node enable`)."
        ),
        node::Exit::Stopped => {}
    }
    Ok(0)
}

/// Hold `node.lock` for as long as this node runs.
fn single_instance(paths: &Paths) -> Result<std::fs::File> {
    let path = paths.data_dir.join("node.lock");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let pid = NodeState::load(paths)
                .map(|s| format!(" (pid {})", s.pid))
                .unwrap_or_default();
            bail!(
                "this computer's machine is already running{pid}; `tessera node status` shows it"
            );
        }
    }
    #[cfg(windows)]
    if !tessera_core::platform::windows::try_lock(&file).with_context(|| {
        format!(
            "couldn't lock {}; refusing to start the machine",
            path.display()
        )
    })? {
        let pid = NodeState::load(paths)
            .map(|s| format!(" (pid {})", s.pid))
            .unwrap_or_default();
        bail!("this computer's machine is already running{pid}; `tessera node status` shows it");
    }
    Ok(file)
}

async fn stop_signal() {
    #[cfg(windows)]
    tessera_core::platform::windows::stop_signal().await;
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut term) = signal(SignalKind::terminate()) {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                return;
            }
        }
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ---- administrator ---------------------------------------------------------------

/// `tessera node admin [on|off|password|forget]`.
fn admin(
    paths: &Paths,
    action: Option<AdminArg>,
    ask: Option<AdminAskArg>,
    stdin: bool,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let config = NodeConfig::load(paths)?;
    let done = |text: &str| {
        if json {
            print_json(&status(paths));
        } else {
            println!("{text}");
        }
        Ok(0)
    };
    match action {
        None => {
            let st = status(paths);
            if json {
                print_json(&st);
                return Ok(0);
            }
            println!(
                "{}",
                match st.scope.admin {
                    AdminAccess::Off =>
                        "Administrator commands are off here (`tessera node admin on` turns them on).",
                    AdminAccess::Ask =>
                        "Administrator commands are on here: each one asks you first.",
                    AdminAccess::Chat =>
                        "Administrator commands are on here, asking as each chat's mode says.",
                }
            );
            println!(
                "sudo's password: {}",
                if st.admin_password {
                    "saved on this computer"
                } else {
                    "not saved (`tessera node admin password` saves it, if sudo needs one)"
                }
            );
            Ok(0)
        }
        Some(AdminArg::On | AdminArg::Off) => {
            let mut new = config.scope.clone();
            new.admin = match (action, ask) {
                (Some(AdminArg::Off), _) => AdminAccess::Off,
                (_, Some(AdminAskArg::Chat)) => AdminAccess::Chat,
                _ => AdminAccess::Ask,
            };
            if new.admin != AdminAccess::Off && !config.enabled {
                bail!("this computer isn't on as a machine yet (`tessera node enable`)");
            }
            let new =
                node::check_scope(paths, &new, node::home().as_deref()).map_err(|e| anyhow!(e))?;
            let code = save_scope(paths, config, new, yes, json)?;
            if code == 0
                && !json
                && action == Some(AdminArg::On)
                && node::admin::password(paths).is_none()
                && node::admin::sudo_accepts(None).is_err()
            {
                println!(
                    "  note: sudo needs a password here: `tessera node admin password` saves it on this computer (agents never see it)."
                );
            }
            Ok(code)
        }
        Some(AdminArg::Password) => {
            refuse_root()?;
            refuse_from_agent()?;
            let password = if stdin {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim_end_matches(['\n', '\r']).to_string()
            } else if tty() {
                read_hidden("This computer's password (for sudo; it stays on this computer): ")?
            } else {
                bail!("give the password on stdin with --stdin, or run this in a terminal");
            };
            node::admin::check_password_text(&password).map_err(|e| anyhow!(e))?;
            node::admin::sudo_accepts(Some(&password)).map_err(|e| anyhow!(e))?;
            node::admin::save_password(paths, &password)?;
            log(
                paths,
                NodeLogEvent::ScopeChanged,
                "a password for administrator commands was saved on this computer",
            );
            done("Saved. sudo accepted it; it stays on this computer, and agents never see it.")
        }
        Some(AdminArg::Forget) => {
            node::admin::forget_password(paths)?;
            log(
                paths,
                NodeLogEvent::ScopeChanged,
                "the password for administrator commands was removed",
            );
            done("The password for administrator commands is gone from this computer.")
        }
    }
}

/// Read a line from the terminal without showing it.
fn read_hidden(prompt: &str) -> Result<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    #[cfg(unix)]
    let restore = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            let old = t;
            t.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Some(old)
        } else {
            None
        }
    };
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    #[cfg(unix)]
    if let Some(old) = restore {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &old);
        }
    }
    println!();
    read?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_only_work_is_offered_without_a_command_sandbox() {
        for os in ["linux", "macos", "windows"] {
            let choices = preset_choices(os);
            assert!(choices.iter().any(|(p, _)| *p == PresetArg::Work), "{os}");
            assert_eq!(
                choices.iter().any(|(p, _)| *p == PresetArg::Full),
                os != "macos"
            );
            let text = node::preset_sentence_with_sandbox(MachinePreset::WorkInFolders, os, false);
            assert!(
                text.contains("read and change the folders you choose"),
                "{os}: {text}"
            );
            assert!(text.contains("Commands aren't available"), "{os}: {text}");
            assert!(!text.contains("run commands"), "{os}: {text}");
        }
        assert!(
            node::preset_sentence_with_sandbox(MachinePreset::WorkInFolders, "linux", true)
                .contains("run commands there")
        );
    }

    #[tokio::test]
    async fn turning_on_again_with_the_same_login_keeps_the_machines_login() {
        let app = backend::ClientFile {
            name: "Home".into(),
            server: ServerConfig {
                endpoints: vec!["https://new.example.com".into()],
                headers: Default::default(),
                token: "tsr_app".into(),
                server_key: "a2V5".into(),
            },
            machine: false,
            traded_from: String::new(),
            route: None,
        };
        let saved = backend::ClientFile {
            server: ServerConfig {
                endpoints: vec!["https://old.example.com".into()],
                token: "tsr_machine".into(),
                ..app.server.clone()
            },
            machine: true,
            traded_from: tessera_core::util::sha256_hex(b"tsr_app"),
            ..app.clone()
        };
        let (kept, changed) = machine_side(app.clone(), Some(saved.clone())).await;
        assert!(kept.machine && changed);
        assert_eq!(
            kept.server.token, "tsr_machine",
            "the running node's login stays"
        );
        assert_eq!(
            kept.server.endpoints, app.server.endpoints,
            "addresses as given now"
        );
        // Already a machine login: nothing to trade.
        let (same, new) = machine_side(saved.clone(), None).await;
        assert!(!new && same.server.token == "tsr_machine");
        // The file never holds the app's own token, and old files still load.
        let text = serde_json::to_string(&kept).unwrap();
        assert!(!text.contains("tsr_app"));
        let old: backend::ClientFile =
            serde_json::from_str(r#"{"name":"x","server":{"endpoints":["a"],"token":"t"}}"#)
                .unwrap();
        assert!(!old.machine && old.traded_from.is_empty());
    }

    #[test]
    fn a_machine_needs_an_encrypted_login() {
        let mut s = ServerConfig {
            endpoints: vec!["http://10.0.0.2:7878".into()],
            headers: Default::default(),
            token: "tsr_x".into(),
            server_key: String::new(),
        };
        assert!(check_server(&s).is_err());
        s.server_key = "a2V5".into();
        assert!(check_server(&s).is_ok());
    }

    #[test]
    fn scope_changes_apply_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().join("home"));
        paths.ensure().unwrap();
        let code = dir.path().join("code");
        std::fs::create_dir_all(&code).unwrap();
        let change = ScopeChange {
            set: None,
            add: vec![code.display().to_string()],
            write: true,
            remove: vec![],
            commands: Some(OnOff::On),
            ask: Some(AskArg::Chat),
            timeout: Some(5),
            desktop: None,
            webviews: Some(OnOff::On),
            terminal: Some(TerminalArg::Sandboxed),
        };
        let s = change.apply(&paths, &MachineScope::default()).unwrap();
        assert_eq!(s.folders.len(), 1);
        assert!(s.folders[0].write && s.commands && s.ask == MachineAsk::Chat && s.webviews);
        assert_eq!(s.terminal, tessera_proto::TerminalAccess::Sandboxed);
        assert!(
            node::widening(&MachineScope::default(), &s)
                .iter()
                .any(|w| w.contains("terminal")),
            "turning terminals on is asked about"
        );
        assert_eq!(s.timeout_secs, node::MIN_TIMEOUT_SECS);
        let back = ScopeChange {
            set: None,
            add: vec![],
            write: false,
            remove: vec![code.display().to_string()],
            commands: Some(OnOff::Off),
            ask: None,
            timeout: None,
            desktop: Some(OnOff::Off),
            webviews: Some(OnOff::Off),
            terminal: Some(TerminalArg::Off),
        };
        let s2 = back.apply(&paths, &s).unwrap();
        assert!(s2.folders.is_empty() && !s2.commands && !s2.webviews);
        assert_eq!(s2.terminal, tessera_proto::TerminalAccess::Off);
        assert!(node::widening(&s, &s2).is_empty());
    }

    #[test]
    fn preset_and_ask_words_map() {
        assert_eq!(PresetArg::Look.preset(), Some(MachinePreset::LookOnly));
        assert_eq!(
            PresetArg::Desktop.preset(),
            Some(MachinePreset::DesktopControl)
        );
        assert_eq!(PresetArg::Off.preset(), None);
        assert_eq!(AskArg::Risky.ask(), MachineAsk::Risky);
        // `every` and `never` read as the apps say them.
        use clap::ValueEnum;
        assert_eq!(
            AskArg::from_str("every", true).ok().map(AskArg::ask),
            Some(MachineAsk::Always)
        );
        assert_eq!(
            AskArg::from_str("never", true).ok().map(AskArg::ask),
            Some(MachineAsk::Chat)
        );
        assert!(PresetArg::from_str("desktop-control", true).is_ok());
    }
}
