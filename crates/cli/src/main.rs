//! `tessera` — terminal entry point for the Tessera harness.

#[macro_use]
mod output;
mod backend;
mod banner;
mod data_lock;
mod guard;
mod node;
mod qr;
mod run;
// Background services: systemd on Linux, the startup programs on Windows.
#[cfg(not(windows))]
mod service;
#[cfg(windows)]
#[path = "service_windows.rs"]
mod service;
#[cfg(any(windows, test))]
mod service_entry;
mod setup;
mod tui;
mod updates;
mod watchdog;

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::net::SocketAddr;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use tessera_core::config::{Config, Paths};
use tessera_proto::*;

use crate::backend::{Backend, BackendChoice, BackendEvent};

#[derive(Parser)]
#[command(
    name = "tessera",
    version,
    about = "A lightweight, long-horizon AI agent harness",
    long_about = None,
    after_help = "New here? `tessera setup` walks you through everything."
)]
struct Cli {
    /// Start the interactive UI with this prompt.
    prompt: Option<String>,
    /// Use an in-process engine even when a server is configured or running.
    #[arg(long, global = true)]
    local: bool,
    /// Data/config directory (default: XDG locations). Also `TESSERA_HOME`.
    #[arg(long, global = true, env = "TESSERA_HOME")]
    home: Option<String>,
    /// Plain output: no colours or decorations.
    #[arg(long, global = true)]
    plain: bool,
    /// Resume a session in the interactive UI.
    #[arg(short = 'r', long)]
    resume: Option<String>,
    /// Continue the most recent session.
    #[arg(short = 'c', long = "continue")]
    continue_last: bool,
    /// Model (`provider/model`) for new sessions.
    #[arg(short, long, global = true)]
    model: Option<String>,
    /// YOLO: new sessions run without asking for approval (catastrophic
    /// commands are still refused). For `run`, same as `--yes`.
    #[arg(long, global = true)]
    yolo: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Thinking {
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl From<Thinking> for ThinkingLevel {
    fn from(t: Thinking) -> Self {
        match t {
            Thinking::Off => ThinkingLevel::Off,
            Thinking::Low => ThinkingLevel::Low,
            Thinking::Medium => ThinkingLevel::Medium,
            Thinking::High => ThinkingLevel::High,
            Thinking::Max => ThinkingLevel::Max,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Guided setup: your AI, where you'll use it, running in the background, your phone.
    Setup {
        /// Run just one step (any time, as often as you like).
        #[arg(value_enum)]
        step: Option<setup::Step>,
        /// Don't animate.
        #[arg(long)]
        no_anim: bool,
    },
    /// Tessera Guard (Windows): your Stop kept where your own programs can't undo it without your approval.
    Guard {
        #[command(subcommand)]
        action: guard::GuardCmd,
    },
    /// Run the server in the background (systemd): install, uninstall, status.
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    /// Run one prompt non-interactively (reads stdin when piped).
    #[command(
        visible_alias = "p",
        after_help = "Exit codes: 0 finished · 1 error · 2 step limit reached · 3 an action needed approval (re-run with --yes)"
    )]
    Run {
        prompt: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// Continue a session by id.
        #[arg(short, long)]
        session: Option<String>,
        /// Continue the most recent session in this directory.
        #[arg(short = 'c', long = "continue")]
        continue_last: bool,
        #[arg(short, long, value_enum)]
        thinking: Option<Thinking>,
        /// Allow every action without asking (YOLO; trusted environments only).
        #[arg(short = 'y', long)]
        yes: bool,
        /// Read-only: no file changes or commands.
        #[arg(long)]
        readonly: bool,
        /// Emit protocol events as JSON lines, then a final
        /// `{"type":"result",…}` line (status, steps, tokens, text).
        #[arg(long)]
        json: bool,
        /// Print only the final answer.
        #[arg(short, long)]
        quiet: bool,
        /// Show thinking and tool output.
        #[arg(short, long)]
        verbose: bool,
        /// Stop after this many steps (exit code 2). The model is chosen
        /// with --model provider/model or --model @role.
        #[arg(long, value_name = "N")]
        max_steps: Option<u32>,
        /// Give a new chat every tool, as the apps do (default: the
        /// terminal's lean coding set).
        #[arg(long)]
        all_tools: bool,
    },
    /// Run the server for desktop, mobile and remote terminals.
    Serve {
        /// Listen address (default from config, 127.0.0.1:7878). Use 0.0.0.0:7878 for LAN access.
        #[arg(long)]
        bind: Option<String>,
        /// Serve a built web UI from this directory.
        #[arg(long)]
        web: Option<String>,
        /// Don't run scheduled jobs in this process.
        #[arg(long)]
        no_scheduler: bool,
    },
    /// Create a one-time code (and QR code) to pair a phone, desktop app or remote terminal.
    Pair {
        /// Don't draw the QR code.
        #[arg(long)]
        no_qr: bool,
    },
    /// Addresses and proxy headers devices use from elsewhere (put in pairing QR codes).
    Remote {
        /// Public URLs, e.g. https://tessera.example.com (replaces the list).
        urls: Vec<String>,
        /// Header devices must send (repeatable), e.g. CF-Access-Client-Id=xxx.
        #[arg(long = "header", short = 'H')]
        headers: Vec<String>,
        /// Forget all addresses and headers.
        #[arg(long)]
        clear: bool,
    },
    /// Web search backend: `tessera web searxng <url> [-H Name=Value]` or `tessera web duckduckgo`.
    Web {
        /// `duckduckgo` or `searxng` (omit to show the current setting).
        backend: Option<String>,
        /// SearXNG base URL.
        url: Option<String>,
        /// Header sent to the backend (repeatable), e.g. CF-Access-Client-Id=xxx.
        #[arg(long = "header", short = 'H')]
        headers: Vec<String>,
    },
    /// Connect this terminal to a remote Tessera server.
    Connect {
        /// Endpoints in preference order (e.g. http://192.168.1.10:7878 https://tessera.example.com),
        /// or a tessera://pair link from `tessera pair`.
        #[arg(required = true)]
        endpoints: Vec<String>,
        /// Pairing code from `tessera pair` on the server.
        #[arg(long, default_value = "")]
        code: String,
        /// Extra header for every request (repeatable), e.g. CF-Access-Client-Id=xxx
        #[arg(long = "header", short = 'H')]
        headers: Vec<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget the remote server and use this machine again.
    Disconnect,
    /// Let your devices use this computer through your server (a "machine"):
    /// turn it on, choose what it allows, keep it on, read its log.
    Node {
        #[command(subcommand)]
        action: node::NodeCmd,
    },
    /// Enrol a local app with this machine's server (prints JSON). Used by the
    /// desktop app's "run on this computer" setup; needs direct access to the
    /// data directory, so it only works on the server's own machine.
    #[command(hide = true)]
    DeviceToken {
        #[arg(long, default_value = "Desktop app")]
        name: String,
        /// Only print this computer's server key (`{"server_key": …}`),
        /// issuing nothing: the desktop app checks a saved server is this
        /// computer's own. Exit 3 when it has none yet.
        #[arg(long)]
        key_only: bool,
        /// Keep earlier same-name logins until the app has saved this one.
        #[arg(long)]
        keep_old: bool,
    },
    /// Retire earlier same-name logins only after the kept login was saved.
    /// Takes a device id, never a bearer token.
    #[command(hide = true)]
    DeviceTokenRetire {
        #[arg(long)]
        keep: String,
    },
    /// Sign in to a provider (currently: chatgpt). `tessera login claude` says
    /// how your own Claude Code signs in (Tessera never does).
    Login {
        provider: String,
        /// Import an existing Codex CLI login instead of signing in.
        #[arg(long)]
        import: bool,
    },
    /// Sign out of a provider.
    Logout { provider: String },
    /// List providers and their models.
    Providers {
        #[arg(long)]
        refresh: bool,
    },
    /// Add or update an OpenAI-compatible, OpenAI or Anthropic provider, or
    /// your own Claude Code (`--kind claude-code`; `--url` is the `claude`
    /// command, found by itself when left out).
    AddProvider {
        id: String,
        #[arg(long, default_value = "")]
        url: String,
        #[arg(long, default_value = "openai-chat")]
        kind: String,
        /// API key (or set it later).
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a provider.
    RemoveProvider { id: String },
    /// Show or set the default model (`provider/model`).
    Model { model: Option<String> },
    /// List sessions.
    Sessions {
        #[arg(long)]
        archived: bool,
        /// Delete this session (by id).
        #[arg(long)]
        delete: Option<String>,
    },
    /// List paired devices (grouped by computer), rename one, or revoke one.
    Devices {
        #[arg(long, value_name = "ID", conflicts_with = "rename")]
        revoke: Option<String>,
        /// Rename this device (and the others on its computer); needs --name.
        #[arg(long, value_name = "ID", requires = "name")]
        rename: Option<String>,
        /// The new name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
    /// List your machines (other computers running `tessera node`); pause,
    /// resume or revoke one. Turning one on happens on that computer.
    Machines {
        #[arg(long, value_name = "ID|NAME", conflicts_with_all = ["resume", "revoke"])]
        pause: Option<String>,
        #[arg(long, value_name = "ID|NAME", conflicts_with = "revoke")]
        resume: Option<String>,
        #[arg(long, value_name = "ID|NAME")]
        revoke: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List scheduled jobs with how their last run went; with a job's id,
    /// its latest runs (when, how they ended, the chat each ran in).
    Jobs {
        #[arg(value_name = "ID")]
        id: Option<String>,
    },
    /// Saved passwords and keys: names, titles and where they may be used
    /// (never a value). `--titles` suggests clearer titles; with `--apply`
    /// it uses them.
    Secrets {
        #[arg(long)]
        titles: bool,
        #[arg(long, requires = "titles")]
        apply: bool,
    },
    /// Send a notification to your devices (the ones that take them), e.g.
    /// from a script: `tessera notify "Backup done" "All 3 disks checked"`.
    Notify {
        title: String,
        /// The text under the title.
        #[arg(default_value = "")]
        body: String,
    },
    /// App updates your devices get from this server: add a build, list, remove.
    Updates {
        #[command(subcommand)]
        action: Option<updates::UpdatesCmd>,
    },
    /// Show paths, configuration and what the running server's memory holds.
    Doctor,
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Install (or update) the background service and (re)start it.
    Install {
        /// Let devices on your network connect (listen on 0.0.0.0).
        #[arg(long)]
        lan: bool,
        /// Listen address (default: server.bind from the config, this machine only).
        #[arg(long)]
        bind: Option<String>,
        /// Run as this user instead of root (system service; root only).
        #[arg(long)]
        user: Option<String>,
        /// Keep it running without a login (`loginctl enable-linger`), without asking.
        #[arg(long, conflicts_with = "no_linger")]
        linger: bool,
        /// Don't enable lingering and don't ask.
        #[arg(long)]
        no_linger: bool,
        /// Show the unit file and commands without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Stop a Tessera server running outside the service first (it holds the port).
        #[arg(long)]
        replace: bool,
        /// Unit name (for a second server with another --home).
        #[arg(long, default_value = service::DEFAULT_NAME)]
        name: String,
    },
    /// Stop and remove the background service (your data stays).
    Uninstall {
        /// Show what would happen without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Unit name.
        #[arg(long, default_value = service::DEFAULT_NAME)]
        name: String,
    },
    /// Show whether the background service is installed and running.
    Status {
        /// Unit name.
        #[arg(long, default_value = service::DEFAULT_NAME)]
        name: String,
        /// As JSON, for the desktop app: `{ supported, installed, running, system }`.
        #[arg(long)]
        json: bool,
    },
    /// Put back the startup entries an update took out (Windows installer).
    #[command(hide = true)]
    RestoreUpdate {
        /// Only when the update that took them out was cut short (its
        /// installer no longer runs): the desktop app, when it opens.
        #[arg(long)]
        if_interrupted: bool,
    },
}

/// `tessera --help` with the commands grouped, setup first.
fn command() -> clap::Command {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let styles = cmd.get_styles().clone();
    let h = styles.get_header();
    let lit = styles.get_literal();
    const GROUPS: &[(&str, &[&str])] = &[
        ("Get started", &["setup"]),
        ("Chat", &["run", "sessions", "jobs"]),
        (
            "Server and devices",
            &[
                "serve",
                "service",
                "pair",
                "devices",
                "remote",
                "connect",
                "disconnect",
                "machines",
                "node",
                "updates",
            ],
        ),
        (
            "Models and search",
            &[
                "login",
                "logout",
                "providers",
                "add-provider",
                "remove-provider",
                "model",
                "web",
            ],
        ),
        ("Help", &["doctor", "help"]),
    ];
    let visible: Vec<(String, String)> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| {
            (
                c.get_name().to_string(),
                c.get_about().map(|a| a.to_string()).unwrap_or_default(),
            )
        })
        .collect();
    let w = visible
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(8)
        .max(6);
    let mut body = String::new();
    let mut listed: Vec<&str> = Vec::new();
    let help_row = (
        "help".to_string(),
        "Print this message or the help of the given command".to_string(),
    );
    let group = |title: &str, names: Vec<&str>, body: &mut String| {
        let rows: Vec<&(String, String)> = names
            .iter()
            .filter_map(|n| {
                visible
                    .iter()
                    .find(|(v, _)| v == n)
                    .or((*n == "help").then_some(&help_row))
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        body.push_str(&format!("{h}{title}:{h:#}\n"));
        for (n, about) in rows {
            body.push_str(&format!("  {lit}{n:<w$}{lit:#}  {about}\n"));
        }
        body.push('\n');
    };
    for (title, names) in GROUPS {
        listed.extend(names.iter());
        group(title, names.to_vec(), &mut body);
    }
    let rest: Vec<&str> = visible
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| !listed.contains(n))
        .collect();
    group("More", rest, &mut body);
    cmd.help_template(format!(
        "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{body}{h}Arguments:{h:#}\n{{positionals}}\n\n{h}Options:{h:#}\n{{options}}{{after-help}}"
    ))
}

fn main() -> Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(status) = tessera_core::sandbox::directory_launcher_entry() {
        std::process::exit(status);
    }
    // Must run before worker threads exist (see jobs::init_local_offset).
    tessera_core::jobs::init_local_offset();
    tessera_core::memstats::limit_arenas();
    // This process holds provider keys and device tokens in memory.
    tessera_secure::harden_process();
    // Nothing started from here keeps a caller's pipes open (the desktop app
    // reads `tessera service install`'s output and waits for its end).
    #[cfg(windows)]
    tessera_core::platform::windows::keep_std_handles_private();
    // The browser's handle relay runs alone, before any providers or agent
    // commands exist. Its standard handles are private before Chromium starts.
    #[cfg(windows)]
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--tessera-webview-worker"))
    {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        if args.len() != 3 {
            anyhow::bail!("invalid web view worker arguments");
        }
        let scale = args[2]
            .to_str()
            .filter(|s| !s.is_empty())
            .map(str::parse::<f64>)
            .transpose()?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = rt.block_on(tessera_core::node::browser_windows::worker(
            std::path::Path::new(&args[0]),
            std::path::Path::new(&args[1]),
            scale,
        ));
        rt.shutdown_timeout(std::time::Duration::from_millis(300));
        return result.map_err(anyhow::Error::msg);
    }
    // Windows' console shows colours and the setup's layout only with its
    // terminal sequences turned on (Windows Terminal has them on already).
    #[cfg(windows)]
    let _ = crossterm::ansi_support::supports_ansi();
    let cli = {
        use clap::FromArgMatches;
        let matches = command().get_matches();
        Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
    };
    let filter = std::env::var("TESSERA_LOG").unwrap_or_else(|_| {
        if matches!(
            cli.command,
            Some(Cmd::Serve { .. })
                | Some(Cmd::Node {
                    action: node::NodeCmd::Run
                })
        ) {
            "info,tower_http=warn".into()
        } else {
            "warn".into()
        }
    });
    // The TUI and the setup wizard own the terminal: no log lines on it.
    let interactive = matches!(cli.command, None | Some(Cmd::Setup { .. }));
    // The guard service has no console: its log goes to a file of its own.
    let service_log = matches!(
        cli.command,
        Some(Cmd::Guard {
            action: guard::GuardCmd::Service
        })
    )
    .then(guard::service_log)
    .flatten();
    if let Some(log) = service_log {
        tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_writer(std::sync::Mutex::new(log))
            .with_ansi(false)
            .with_target(false)
            .init();
    } else if !interactive {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(false)
            .init();
    }
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    // The work is waiting on the network, models and child processes;
    // blocking work has its own pool. A few workers do: fewer threads,
    // less memory. The terminal UI needs fewer still.
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    builder.worker_threads(if interactive { 2 } else { cores.clamp(2, 4) });
    let rt = builder.enable_all().build()?;
    let code = output::exit_code(rt.block_on(async_main(cli)))?;
    rt.shutdown_timeout(std::time::Duration::from_millis(300));
    std::process::exit(code);
}

async fn async_main(cli: Cli) -> Result<i32> {
    let paths = match &cli.home {
        Some(h) => Paths::at(tessera_core::util::expand_home(h)),
        None => Paths::resolve(),
    };
    paths.ensure()?;
    let choice = || BackendChoice {
        force_local: cli.local,
    };
    // A custom home must reach the background service too.
    let home = cli
        .home
        .as_deref()
        .map(tessera_core::util::expand_home)
        .map(|p| tessera_core::platform::canonicalize(&p).unwrap_or(p));

    match cli.command {
        None => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                // Behave like `run` when piped.
                let backend = Backend::open(&paths, choice()).await?;
                return run::run(
                    backend,
                    run::RunOptions {
                        prompt: cli.prompt,
                        model: cli.model,
                        cwd: None,
                        session: cli.resume,
                        continue_last: cli.continue_last,
                        thinking: None,
                        permission: cli.yolo.then_some(PermissionMode::Auto),
                        json: false,
                        quiet: false,
                        verbose: false,
                        max_steps: None,
                        all_tools: false,
                    },
                )
                .await;
            }
            let mut backend = Backend::open(&paths, choice()).await?;
            if cli.prompt.is_none() && cli.resume.is_none() && !cli.continue_last {
                let status = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    backend.call_as::<SetupStatus>(Request::SetupStatus),
                )
                .await;
                if let Ok(Ok(s)) = status
                    && !s.completed
                {
                    match setup::offer(cli.plain).await? {
                        setup::Offer::Run => {
                            let outcome = setup::run(
                                paths.clone(),
                                backend.clone(),
                                setup::Options {
                                    plain: cli.plain,
                                    no_anim: false,
                                    only: None,
                                    first_run: true,
                                    force_local: cli.local,
                                    home: home.clone(),
                                },
                            )
                            .await?;
                            if outcome != 0 {
                                return Ok(if outcome == setup::QUIT { 0 } else { outcome });
                            }
                            backend.close();
                            backend = Backend::open(&paths, choice()).await?;
                        }
                        setup::Offer::Never => {
                            let _ = backend.call(Request::CompleteSetup).await;
                        }
                        setup::Offer::Later => {}
                    }
                }
            }
            tui::run(
                backend,
                tui::TuiOptions {
                    prompt: cli.prompt,
                    resume: cli.resume,
                    continue_last: cli.continue_last,
                    model: cli.model,
                    plain: cli.plain,
                    yolo: cli.yolo,
                    prefs_file: paths.config_dir.join("tui.json"),
                    log_file: paths.data_dir.join("tui.log"),
                },
            )
            .await?;
            Ok(0)
        }
        Some(Cmd::Run {
            prompt,
            cwd,
            session,
            continue_last,
            thinking,
            yes,
            readonly,
            json,
            quiet,
            verbose,
            max_steps,
            all_tools,
        }) => {
            let backend = Backend::open(&paths, choice()).await?;
            let permission = if readonly {
                Some(PermissionMode::ReadOnly)
            } else if yes || cli.yolo {
                Some(PermissionMode::Auto)
            } else {
                None
            };
            run::run(
                backend,
                run::RunOptions {
                    prompt,
                    model: cli.model,
                    cwd,
                    session,
                    continue_last,
                    thinking: thinking.map(Into::into),
                    permission,
                    json,
                    quiet,
                    verbose,
                    max_steps,
                    all_tools,
                },
            )
            .await
        }
        Some(Cmd::Setup { step, no_anim }) => {
            let backend = match Backend::open(&paths, choice()).await {
                Ok(b) => b,
                Err(e) if backend::load_client_file(&paths).is_some() && !cli.local => {
                    // The saved server is unreachable: set up (or switch
                    // to) this machine instead.
                    eprintln!(
                        "  Couldn't reach the saved server ({e:#}); continuing on this machine."
                    );
                    Backend::open(&paths, BackendChoice { force_local: true }).await?
                }
                Err(e) => return Err(e),
            };
            let code = setup::run(
                paths,
                backend,
                setup::Options {
                    plain: cli.plain,
                    no_anim,
                    only: step,
                    first_run: false,
                    force_local: cli.local,
                    home,
                },
            )
            .await?;
            Ok(if code == setup::QUIT { 0 } else { code })
        }
        Some(Cmd::Guard { action }) => guard::main(&paths, action),
        Some(Cmd::Service { action }) => match action {
            ServiceCmd::Install {
                lan,
                bind,
                user,
                linger,
                no_linger,
                dry_run,
                replace,
                name,
            } => {
                let opts = service::InstallOptions {
                    bind,
                    lan,
                    user,
                    linger: if linger {
                        service::Linger::Yes
                    } else if no_linger {
                        service::Linger::No
                    } else {
                        service::Linger::Ask
                    },
                    dry_run,
                    name,
                    home,
                    replace,
                };
                let mut term = setup::term::Term::new(setup::term::Caps::detect(cli.plain, true));
                service::install(&paths, &opts, Some(&mut term)).await?;
                Ok(0)
            }
            ServiceCmd::Uninstall { dry_run, name } => {
                service::uninstall(&name, dry_run)?;
                Ok(0)
            }
            ServiceCmd::RestoreUpdate { if_interrupted } => {
                // Never longer than this, whatever hangs (the installer and
                // the app wait for it); what isn't back stays noted.
                tokio::time::timeout(
                    std::time::Duration::from_secs(140),
                    service::restore_update(if_interrupted),
                )
                .await
                .map_err(|_| anyhow::anyhow!("putting Tessera's startup entries back took too long; they stay noted and are tried again when the app opens"))??;
                Ok(0)
            }
            ServiceCmd::Status { name, json: false } => service::status(&paths, &name).await,
            ServiceCmd::Status { name, json: true } => {
                // "Keep Tessera running" in the desktop app (Settings → Devices → This computer).
                #[cfg(windows)]
                let supported = true;
                #[cfg(not(windows))]
                let supported = service::available();
                let found = service::summary(&name);
                println!(
                    "{}",
                    serde_json::json!({
                        "supported": supported,
                        "installed": found.is_some(),
                        "running": found.as_ref().is_some_and(|s| s.active),
                        // A Linux system service: only root changes it.
                        "system": found.as_ref().is_some_and(|s| s.scope == service::Scope::System),
                    })
                );
                Ok(0)
            }
        },
        Some(Cmd::Serve {
            bind,
            web,
            no_scheduler,
        }) => serve(paths, bind, web, !no_scheduler).await.map(|_| 0),
        Some(Cmd::Pair { no_qr }) => {
            let backend = Backend::open(&paths, choice()).await?;
            setup::pair(backend, cli.plain, !no_qr).await
        }
        Some(Cmd::Web {
            backend,
            url,
            headers,
        }) => {
            let backend_conn = Backend::open(&paths, choice()).await?;
            let info: WebSearchInfo = match backend {
                None => backend_conn.call_as(Request::GetWebSearch).await,
                Some(b) => {
                    let headers = if headers.is_empty() {
                        None
                    } else {
                        Some(parse_headers(&headers)?)
                    };
                    backend_conn
                        .call_as(Request::SaveWebSearch {
                            search: b,
                            searxng_url: url,
                            headers,
                        })
                        .await
                }
            }
            .map_err(|e| anyhow!(e.message))?;
            match info
                .searxng_url
                .as_deref()
                .filter(|_| info.search == "searxng")
            {
                Some(u) => println!("Web search: SearXNG at {u} (DuckDuckGo as fallback)"),
                None => println!("Web search: DuckDuckGo"),
            }
            if !info.header_names.is_empty() {
                println!(
                    "Headers: {} (values are stored privately)",
                    info.header_names.join(", ")
                );
            }
            Ok(0)
        }
        Some(Cmd::Remote {
            urls,
            headers,
            clear,
        }) => {
            let backend = Backend::open(&paths, choice()).await?;
            let check = (!clear).then(|| (urls.clone(), parse_headers(&headers)));
            let info: RemoteAccessInfo = if clear {
                backend
                    .call_as(Request::SetRemoteAccess {
                        urls: vec![],
                        headers: Some(BTreeMap::new()),
                        add_headers: None,
                        remove_headers: Vec::new(),
                    })
                    .await
            } else if !urls.is_empty() || !headers.is_empty() {
                let current: RemoteAccessInfo = backend
                    .call_as(Request::GetRemoteAccess)
                    .await
                    .map_err(|e| anyhow!(e.message))?;
                let urls = if urls.is_empty() { current.urls } else { urls };
                let headers = if headers.is_empty() {
                    None
                } else {
                    Some(parse_headers(&headers)?)
                };
                backend
                    .call_as(Request::SetRemoteAccess {
                        urls,
                        headers,
                        add_headers: None,
                        remove_headers: Vec::new(),
                    })
                    .await
            } else {
                backend.call_as(Request::GetRemoteAccess).await
            }
            .map_err(|e| anyhow!(e.message))?;
            if info.urls.is_empty() {
                println!(
                    "No remote addresses. Devices can pair on the LAN; add a tunnel with: tessera remote https://tessera.example.com -H Name=Value"
                );
            } else {
                println!("Remote addresses (in pairing QR codes after the LAN address):");
                for u in &info.urls {
                    println!("  {u}");
                }
            }
            if !info.header_names.is_empty() {
                println!(
                    "Headers sent by devices: {} (values are stored privately)",
                    info.header_names.join(", ")
                );
            }
            // A proxy on the server's computer must say who connects, or the
            // server takes everyone who reaches it for itself.
            if let Some((urls, Ok(headers))) = check {
                for u in urls
                    .iter()
                    .filter(|u| !tessera_client::is_loopback_endpoint(u))
                {
                    if tessera_client::seen_as_local(u, &headers).await == Some(true) {
                        println!(
                            "Warning: requests through {u} reach the server as if they were made on the server's own computer, because the proxy in front of it doesn't say who is connecting. Then anyone who reaches {u} gets what only this computer should (plain, unencrypted access). Make the proxy add X-Forwarded-For (nginx: proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;). See docs/REMOTE_ACCESS.md."
                        );
                    }
                }
            }
            Ok(0)
        }
        Some(Cmd::Connect {
            endpoints,
            code,
            headers,
            name,
        }) => {
            let mut headers = parse_headers(&headers)?;
            let (mut endpoints, mut code, mut name, mut expected) = (endpoints, code, name, None);
            if let Some(link) = endpoints.first().filter(|e| e.starts_with("tessera://")) {
                let link = PairLink::parse(link).map_err(|e| anyhow!(e))?;
                endpoints = link.urls;
                headers.extend(link.headers);
                code = link.code;
                name = name.or(Some(link.name).filter(|n| !n.is_empty()));
                expected = link.fingerprint;
            }
            let c = connect_server(&paths, endpoints, code, headers, name, expected).await?;
            println!(
                "Connected to {} ({}) via {}, end-to-end encrypted.",
                c.name, c.version, c.endpoint
            );
            println!(
                "Server key {} (compare with `tessera pair` on the server).",
                c.fingerprint
            );
            println!(
                "`tessera` now uses this server; `tessera --local` or `tessera disconnect` to use this machine."
            );
            Ok(0)
        }
        Some(Cmd::DeviceToken { key_only: true, .. }) => {
            // Only reads it: a computer without a server says so.
            let Some(key) =
                tessera_secure::Identity::public_at(&tessera_server::identity_file(&paths))
            else {
                return Ok(3);
            };
            println!(
                "{}",
                serde_json::json!({ "server_key": tessera_secure::b64(&key) })
            );
            Ok(0)
        }
        Some(Cmd::DeviceToken { name, keep_old, .. }) => {
            let store = tessera_core::store::Store::open(&paths.db_file())?;
            let name = tessera_core::util::one_line(&name, 60);
            let (device_id, token) = store.add_device(&name)?;
            // Preserve the old CLI default. Apps opt out until they have
            // durably saved the new login, then retire with its public id.
            if !keep_old {
                store.retire_older_devices(&device_id)?;
            }
            let identity = tessera_server::load_identity(&paths)?;
            let lock: Option<backend::ServerLock> =
                std::fs::read_to_string(backend::lock_file(&paths))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok());
            let url = lock.map(|l| l.url).unwrap_or_else(|| {
                format!(
                    "http://{}",
                    local_addr(
                        &Config::load(&paths)
                            .map(|c| c.server.bind)
                            .unwrap_or_default()
                            .parse()
                            .unwrap_or_else(|_| "127.0.0.1:7878".parse().unwrap())
                    )
                )
            });
            println!(
                "{}",
                serde_json::json!({ "device_id": device_id, "token": token, "server_key": tessera_secure::b64(identity.public()), "url": url })
            );
            Ok(0)
        }
        Some(Cmd::DeviceTokenRetire { keep }) => {
            let store = tessera_core::store::Store::open(&paths.db_file())?;
            store.retire_older_devices(&keep)?;
            Ok(0)
        }
        Some(Cmd::Node { action }) => node::main(&paths, action, home).await,
        // On the server's own computer: the data folder is where builds live.
        Some(Cmd::Updates { action }) => updates::main(&paths, action),
        Some(Cmd::Machines {
            pause,
            resume,
            revoke,
            json,
        }) => {
            let backend = Backend::open(&paths, choice()).await?;
            node::machines(&backend, pause, resume, revoke, json).await
        }
        Some(Cmd::Disconnect) => {
            let f = backend::client_file(&paths);
            if f.exists() {
                std::fs::remove_file(&f)?;
                println!("Disconnected. `tessera` now runs locally.");
            } else {
                println!("Not connected to a remote server.");
            }
            Ok(0)
        }
        Some(Cmd::Login { provider, import }) => {
            let backend = Backend::open(&paths, choice()).await?;
            login(&backend, &provider, import).await.map(|_| 0)
        }
        Some(Cmd::Logout { provider }) => {
            let backend = Backend::open(&paths, choice()).await?;
            backend
                .call(Request::Logout {
                    provider: provider_alias(&provider),
                })
                .await?;
            println!("Signed out.");
            Ok(0)
        }
        Some(Cmd::Providers { refresh }) => {
            let backend = Backend::open(&paths, choice()).await?;
            if matches!(backend, Backend::Local(_)) && !refresh {
                // Give first-run detection and discovery a moment.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
            let providers: Vec<ProviderInfo> =
                backend.call_as(Request::ListProviders { refresh }).await?;
            let info: ServerInfo = backend
                .call_as(Request::Hello {
                    client: "cli".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                })
                .await?;
            print_providers(&providers, info.default_model.as_ref());
            Ok(0)
        }
        Some(Cmd::AddProvider {
            id,
            url,
            kind,
            key,
            name,
        }) => {
            let backend = Backend::open(&paths, choice()).await?;
            let kind = match kind.as_str() {
                "openai-chat" | "openai_chat" | "chat" | "openai-compatible" => {
                    ProviderKind::OpenaiChat
                }
                "openai" | "responses" | "openai-responses" => ProviderKind::OpenaiResponses,
                "anthropic" | "claude" => ProviderKind::Anthropic,
                "claude-code" | "claude_code" | "claude-subscription" => ProviderKind::ClaudeCode,
                other => {
                    bail!("unknown kind `{other}` (openai-chat, openai, anthropic, claude-code)")
                }
            };
            if kind == ProviderKind::ClaudeCode && key.is_some() {
                bail!(
                    "Claude Code signs in by itself: run `claude` once on the server; Tessera never takes Claude credentials"
                );
            }
            if url.trim().is_empty() && kind != ProviderKind::ClaudeCode {
                bail!("give the provider's address with --url");
            }
            let name = name.or_else(|| {
                (kind == ProviderKind::ClaudeCode).then(|| "Claude (your subscription)".to_string())
            });
            let info = add_provider(&backend, id, url, kind, key, name).await?;
            println!("Saved `{}` with {} models.", info.id, info.models.len());
            if kind == ProviderKind::ClaudeCode {
                let cc: ClaudeCodeInfo = backend
                    .call_as(Request::ClaudeCodeStatus {
                        refresh: false,
                        provider: None,
                    })
                    .await?;
                println!("{}", cc.hint);
            }
            Ok(0)
        }
        Some(Cmd::RemoveProvider { id }) => {
            let backend = Backend::open(&paths, choice()).await?;
            backend.call(Request::RemoveProvider { id }).await?;
            println!("Removed.");
            Ok(0)
        }
        Some(Cmd::Model { model }) => {
            let backend = Backend::open(&paths, choice()).await?;
            match model {
                Some(m) => {
                    let m: ModelRef = m.parse().map_err(|e: String| anyhow!(e))?;
                    set_default_model(&backend, m.clone()).await?;
                    println!("Default model: {m}");
                }
                None => {
                    let info: ServerInfo = backend
                        .call_as(Request::Hello {
                            client: "cli".into(),
                            version: String::new(),
                        })
                        .await?;
                    println!(
                        "{}",
                        info.default_model
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| {
                                "(none — add a provider or `tessera login chatgpt`)".into()
                            })
                    );
                }
            }
            Ok(0)
        }
        Some(Cmd::Sessions { archived, delete }) => {
            let backend = Backend::open(&paths, choice()).await?;
            if let Some(id) = delete {
                backend
                    .call(Request::DeleteSession { session_id: id })
                    .await
                    .map_err(|e| anyhow!(e.message))?;
                println!("Deleted.");
                return Ok(0);
            }
            let list: Vec<SessionInfo> =
                backend.call_as(Request::ListSessions { archived }).await?;
            // Harness helpers (the background memory review) aren't listed.
            for s in list.into_iter().filter(|s| !s.internal) {
                println!(
                    "{}  {:<12} {:<44} {}",
                    s.id,
                    format!("{:?}", s.status).to_lowercase(),
                    tessera_core::util::truncate(&s.title, 42),
                    tessera_core::jobs::fmt_time(s.updated_at)
                );
            }
            Ok(0)
        }
        Some(Cmd::Devices {
            revoke,
            rename,
            name,
        }) => {
            let backend = Backend::open(&paths, choice()).await?;
            if let Some(id) = revoke {
                backend.call(Request::RevokeDevice { id }).await?;
                println!("Revoked.");
                return Ok(0);
            }
            let list: Vec<DeviceInfo> = if let Some(id) = rename {
                let name = name.unwrap_or_default();
                let list: Vec<DeviceInfo> = backend
                    .call_as(Request::RenameDevice {
                        id,
                        name: name.clone(),
                    })
                    .await?;
                println!("Renamed to “{}”.", name.trim());
                list
            } else {
                backend.call_as(Request::ListDevices).await?
            };
            print_devices(&list);
            Ok(0)
        }
        Some(Cmd::Jobs { id }) => {
            let backend = Backend::open(&paths, choice()).await?;
            if let Some(id) = id {
                let runs: Vec<tessera_proto::JobRun> = backend
                    .call_as(Request::ListJobRuns { id: id.clone() })
                    .await?;
                if runs.is_empty() {
                    println!("No runs of {id} recorded.");
                }
                for r in runs {
                    println!(
                        "{}  {:<11} {:<9} chat {}",
                        tessera_core::jobs::fmt_time(r.started_at),
                        job_outcome_word(&r.outcome),
                        r.ended_at
                            .map(|e| human_secs(e.saturating_sub(r.started_at) / 1000))
                            .unwrap_or_else(|| "—".into()),
                        r.session_id
                    );
                }
                return Ok(0);
            }
            let list: Vec<Job> = backend.call_as(Request::ListJobs).await?;
            if list.is_empty() {
                println!(
                    "No scheduled jobs. Ask the agent to schedule something, e.g. \"every weekday at 9 check …\"."
                );
            }
            for j in list {
                let state = if j.enabled {
                    "on "
                } else if j.ended.is_some() {
                    "end"
                } else {
                    "off"
                };
                let last = match (j.last_run, &j.last_status) {
                    (Some(t), Some(s)) => format!(
                        "last {} {}",
                        tessera_core::jobs::fmt_time(t),
                        job_outcome_word(s)
                    ),
                    (Some(t), None) => format!("last {}", tessera_core::jobs::fmt_time(t)),
                    _ => "never ran".into(),
                };
                let mut limit = String::new();
                if let Some(m) = j.max_runs {
                    limit.push_str(&format!("  run {}/{m}", j.runs));
                }
                if let Some(u) = j.until {
                    limit.push_str(&format!("  until {}", tessera_core::jobs::fmt_time(u)));
                }
                println!(
                    "{}  {:<28} {:<16} {}  next {}  {last}{limit}",
                    j.id,
                    tessera_core::util::truncate(&j.name, 26),
                    j.schedule,
                    state,
                    j.next_run
                        .map(tessera_core::jobs::fmt_time)
                        .unwrap_or_else(|| "—".into())
                );
                if let Some(why) = &j.ended {
                    println!("    ended: {why}");
                }
            }
            Ok(0)
        }
        Some(Cmd::Notify { title, body }) => {
            let backend = Backend::open(&paths, choice()).await?;
            backend
                .call(Request::SendNotification {
                    title,
                    body,
                    session_id: None,
                    delay_secs: 0,
                })
                .await
                .map_err(|e| anyhow!(e.message))?;
            println!("Sent.");
            Ok(0)
        }
        Some(Cmd::Secrets { titles, apply }) => {
            let backend = Backend::open(&paths, choice()).await?;
            if titles {
                let list: Vec<tessera_proto::SecretTitle> =
                    backend.call_as(Request::SuggestSecretTitles).await?;
                if list.is_empty() {
                    println!("Every title says what it is for.");
                }
                for t in list {
                    println!("{:<28} “{}” → “{}”", t.name, t.title, t.suggested);
                    if apply {
                        backend
                            .call(Request::SetSecretTitle {
                                name: t.name.clone(),
                                title: t.suggested.clone(),
                            })
                            .await
                            .map_err(|e| anyhow!(e.message))?;
                    }
                }
                if apply {
                    println!("Renamed.");
                }
                return Ok(0);
            }
            let list: Vec<SecretInfo> = backend.call_as(Request::ListSecrets).await?;
            let sec: SecurityInfo = backend.call_as(Request::GetSecurity).await?;
            for s in list {
                let machines = if s.machines_default {
                    format!("machines: like all ({:?})", sec.secret_machines).to_lowercase()
                } else if s.on_machines.is_empty() {
                    "machines: none".into()
                } else {
                    format!("machines: {}", s.on_machines.join(", "))
                };
                println!(
                    "{:<28} {:<36} {} · {}{}",
                    s.name,
                    tessera_core::util::truncate(&s.description, 34),
                    if s.on_server {
                        "this server"
                    } else {
                        "not this server"
                    },
                    machines,
                    s.chat
                        .map(|c| format!(" · kept for chat {c}"))
                        .unwrap_or_default()
                );
            }
            Ok(0)
        }
        Some(Cmd::Doctor) => {
            let cfg = Config::load(&paths)?;
            println!("config:  {}", paths.config_file().display());
            println!("data:    {}", paths.data_dir.display());
            println!("skills:  {}", paths.skills_dir().display());
            println!(
                "remote:  {}",
                backend::load_client_file(&paths)
                    .map(|f| format!("{} {:?}", f.name, f.server.endpoints))
                    .unwrap_or_else(|| "none (local)".into())
            );
            println!(
                "command sandbox: {}",
                if cfg.security.sandbox == tessera_core::config::SandboxMode::Off {
                    "off by your choice".to_string()
                } else {
                    tessera_core::sandbox::describe()
                }
            );
            if cfg.security.sandbox == tessera_core::config::SandboxMode::Auto
                && !tessera_core::sandbox::support().complete()
            {
                println!(
                    "Every command asks first. To run without the safe box, make a one-time choice of Off in Settings → Privacy & safety → What commands can reach (security.sandbox = \"off\" in config.toml). Commands would have your account's access, including Tessera's keys and chats."
                );
            }
            println!(
                "default model: {}",
                cfg.default_model.unwrap_or_else(|| "(auto)".into())
            );
            println!(
                "providers in config: {}",
                cfg.providers
                    .iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!();
            match doctor_memory(&paths).await {
                Ok((from, lines)) => {
                    println!("Server memory ({from}):");
                    for l in lines {
                        println!("  {l}");
                    }
                }
                Err(e) => println!("Server memory: {e:#}"),
            }
            Ok(0)
        }
    }
}

/// The running server's own account of its memory (it can't be read from
/// outside: the server keeps other processes out of its memory maps).
async fn doctor_memory(paths: &Paths) -> Result<(String, Vec<String>)> {
    if backend::load_client_file(paths).is_none() && backend::local_daemon(paths).await.is_none() {
        bail!(
            "no server is running here (`tessera serve`, or `tessera service install` to run one in the background)"
        );
    }
    let backend = Backend::open(paths, BackendChoice { force_local: false }).await?;
    let report: tessera_core::engine::MemoryReport =
        backend.call_as(Request::MemoryReport).await.map_err(|e| {
            if e.message.contains("unknown variant") {
                anyhow!("this server is older and can't report its memory yet; update it")
            } else {
                anyhow!(e.message)
            }
        })?;
    Ok((backend.describe(), report.describe()))
}

async fn serve(
    paths: Paths,
    bind: Option<String>,
    web: Option<String>,
    scheduler: bool,
) -> Result<()> {
    let cfg = Config::load(&paths)?;
    let bind = bind.unwrap_or(cfg.server.bind.clone());
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address `{bind}`"))?;
    // A second server on the same data would mark the running one's chats
    // as interrupted and remove its lock: refuse before opening anything.
    // `server.json` names a server only once it is up; the data lock also
    // covers one that is still starting (two started at the same moment).
    let data_lock = hold_data_lock(&paths).await?;
    let other = std::fs::read_to_string(backend::lock_file(&paths))
        .ok()
        .and_then(|s| serde_json::from_str::<backend::ServerLock>(&s).ok())
        .filter(|l| l.pid != std::process::id() && process_alive(l.pid));
    if data_lock.is_none() || other.is_some() {
        let msg = format!(
            "Tessera is already running here{}. Stop it first ({}).",
            other
                .map(|o| format!(" (process {})", o.pid))
                .unwrap_or_default(),
            if cfg!(windows) {
                "`tessera service uninstall` when it runs in the background, else end tessera.exe in Task Manager"
            } else {
                "`systemctl --user stop tessera` when it runs in the background"
            }
        );
        // Started by `tessera-service.exe` (Windows): a code of its own, so
        // it stops instead of trying again and again beside the running one.
        if std::env::var_os("TESSERA_BACKGROUND").is_some() {
            eprintln!("Error: {msg}");
            std::process::exit(ALREADY_RUNNING);
        }
        anyhow::bail!(msg);
    }
    std::net::TcpListener::bind(addr).with_context(|| {
        format!("can't listen on {bind}: another program (another Tessera?) is using it")
    })?;
    let engine =
        tessera_core::Engine::open(paths.clone(), tessera_core::EngineOptions { scheduler })
            .await?;
    engine.set_listen_addr(&bind);
    // A runtime that stops running tasks is noticed, written down and
    // (under systemd) restarted.
    watchdog::start(&paths.data_dir, addr);
    // A recovered stall is diagnostic history, not another device alert.
    // Unrecovered tasks still surface their own actionable failure.
    if let Some((title, body)) = watchdog::previous_stall(&paths.data_dir) {
        let _ = engine
            .store()
            .record_notification(&tessera_core::store::NewNotification {
                title,
                body,
                from: "Tessera".into(),
                kind: "system".into(),
                source_key: "system_watchdog".into(),
                quiet: true,
                ..Default::default()
            });
    }
    // Windows, started at sign-in: a computer left on to serve doesn't go
    // to sleep by itself while plugged in.
    #[cfg(windows)]
    if service_entry::keeps_awake(
        std::env::var("TESSERA_BACKGROUND").ok().as_deref(),
        std::env::var("TESSERA_KEEP_AWAKE").ok().as_deref(),
        cfg.server.keep_awake,
    ) {
        tessera_core::platform::windows::keep_awake_while_plugged_in();
    }
    let identity = tessera_server::load_identity(&paths)?;
    let lock = backend::ServerLock {
        pid: std::process::id(),
        url: format!("http://{}", local_addr(&addr)),
        key: tessera_secure::b64(identity.public()),
    };
    tessera_core::config::write_atomic(
        &backend::lock_file(&paths),
        serde_json::to_string(&lock)?.as_bytes(),
        false,
    )?;
    banner::print_banner("server");
    println!("  Listening on {bind}");
    for url in server_urls(&bind) {
        println!("    {url}");
    }
    println!(
        "  Server key {} · apps connect end-to-end encrypted",
        identity.fingerprint()
    );
    if !cfg.security.require_e2ee {
        println!(
            "  \x1b[33mrequire_e2ee is off: browsers on the network may connect without end-to-end encryption.\x1b[0m"
        );
    }
    match cfg.security.sandbox {
        tessera_core::config::SandboxMode::Off => println!(
            "  \x1b[33mTool sandbox is off: the agent's commands can read Tessera's credentials.\x1b[0m"
        ),
        _ => println!("  Tool sandbox {}", tessera_core::sandbox::describe()),
    }
    if let Some(hint) = tessera_core::sandbox::hint() {
        println!("  \x1b[33m{hint}\x1b[0m");
    }
    let devices = engine.store().devices(None)?;
    if devices
        .iter()
        .all(|d| d.name.starts_with("Terminal (this machine)"))
    {
        // Only on a terminal: a code in a log file could be read and used by
        // anything that can read the log.
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            let (code, _) = engine.store().create_pairing_code(10 * 60_000)?;
            println!(
                "\n  No devices paired yet. Pairing code: \x1b[1m{code}\x1b[0m (10 minutes; `tessera pair` makes a new one)"
            );
        } else {
            println!(
                "\n  No devices paired yet. Run `tessera pair` in a terminal for a pairing code."
            );
        }
    }
    if addr.ip().is_loopback() {
        println!(
            "\n  Only this machine can connect. Use --bind 0.0.0.0:{} (or set server.bind) for LAN / tunnel access.",
            addr.port()
        );
    }
    println!();
    let web_dir = web.map(|w| tessera_core::util::expand_home(&w));
    let result = tessera_server::serve(
        engine,
        tessera_server::ServerOptions {
            bind: addr,
            web_dir,
        },
    )
    .await;
    // Ours only (a lock another server wrote stays).
    if std::fs::read_to_string(backend::lock_file(&paths))
        .ok()
        .and_then(|s| serde_json::from_str::<backend::ServerLock>(&s).ok())
        .is_some_and(|l| l.pid == std::process::id())
    {
        let _ = std::fs::remove_file(backend::lock_file(&paths));
    }
    result
}

/// The exit code of a server started in the background that found another
/// one already running on the same data (`tessera-service.exe` then stops).
const ALREADY_RUNNING: i32 = 69;

/// Hold `server.lock` in the data folder for as long as this server runs;
/// `None` while another server holds it. One that just stopped can take a
/// moment to let go of it (Windows), so it waits a little first.
async fn hold_data_lock(paths: &Paths) -> Result<Option<std::fs::File>> {
    let path = paths.data_dir.join("server.lock");
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
    for _ in 0..40 {
        match data_lock::lock_now(&file)
            .with_context(|| format!("couldn't lock {}; refusing to start", path.display()))?
        {
            data_lock::Locked::Held => return Ok(Some(file)),
            // Only known missing filesystem support uses this weaker path.
            data_lock::Locked::Unsupported => {
                tracing::warn!(
                    "{}: this file system does not support the server lock; using server.json only (simultaneous starts are not protected)",
                    path.display()
                );
                return Ok(Some(file));
            }
            data_lock::Locked::Busy => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await
            }
        }
    }
    Ok(None)
}

/// Whether process `pid` still runs (and, where that can be seen, is a
/// Tessera: a stale lock's number may belong to something else by now).
fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(name) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        return name.trim().starts_with("tessera");
    }
    #[cfg(unix)]
    {
        // Signal 0 checks without sending anything; EPERM means it exists.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        tessera_core::platform::windows::alive(pid)
            && tessera_core::platform::windows::runs(pid, "tessera")
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

fn local_addr(addr: &SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("127.0.0.1:{}", addr.port())
    } else {
        addr.to_string()
    }
}

fn server_urls(bind: &str) -> Vec<String> {
    let Ok(addr) = bind.parse::<SocketAddr>() else {
        return vec![format!("http://{bind}")];
    };
    let mut urls = vec![format!("http://{}", local_addr(&addr))];
    if addr.ip().is_unspecified()
        && let Some(ip) = lan_ip()
    {
        urls.push(format!("http://{ip}:{}", addr.port()));
    }
    urls
}

/// The device list: one block per computer, each login on a line with
/// what it is.
/// How a scheduled run went, in a word (`Job::last_status`).
fn job_outcome_word(status: &str) -> &str {
    match status {
        "ok" => "finished",
        "error" => "failed",
        other => other,
    }
}

/// `3m 20s`, `1h 05m`.
fn human_secs(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn print_devices(list: &[DeviceInfo]) {
    let mut groups: Vec<(&str, Vec<&DeviceInfo>)> = Vec::new();
    for d in list {
        let key = d.computer.as_deref().unwrap_or(&d.id);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(d),
            None => groups.push((key, vec![d])),
        }
    }
    for (_, g) in groups {
        let several = g.len() > 1;
        if several {
            let host = g.iter().find_map(|d| d.host.as_deref());
            println!(
                "One computer{}:",
                host.map(|h| format!(" ({h})")).unwrap_or_default()
            );
        }
        for d in g {
            let what = match d.client {
                DeviceClient::App => "app",
                DeviceClient::Phone => "phone app",
                DeviceClient::Browser => "browser",
                DeviceClient::Terminal => "terminal",
                DeviceClient::Machine => "machine",
                DeviceClient::Unknown => "device",
            };
            println!(
                "{}{}  {:<32} {:<9} paired {}  last seen {}{}",
                if several { "  " } else { "" },
                d.id,
                d.name,
                what,
                tessera_core::jobs::fmt_time(d.created_at),
                d.last_seen
                    .map(tessera_core::jobs::fmt_time)
                    .unwrap_or_else(|| "never".into()),
                if d.current { "  (this one)" } else { "" }
            );
        }
    }
}

/// Primary LAN address (no packets are sent).
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr()
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_loopback())
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "this computer".into())
}

fn parse_headers(raw: &[String]) -> Result<BTreeMap<String, String>> {
    raw.iter()
        .map(|h| {
            let (k, v) = h
                .split_once('=')
                .or_else(|| h.split_once(':'))
                .ok_or_else(|| anyhow!("header `{h}` must be Name=Value"))?;
            Ok((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn provider_alias(p: &str) -> String {
    match p {
        "chatgpt" | "codex" | "openai-codex" => tessera_core::auth::CODEX_PROVIDER.into(),
        other => other.into(),
    }
}

async fn login(backend: &Backend, provider: &str, import: bool) -> Result<()> {
    let provider = provider_alias(provider);
    if matches!(
        provider.as_str(),
        "claude" | "claude-code" | "anthropic-subscription"
    ) {
        // Claude Code signs in by itself; Tessera only says what to do.
        let cc: ClaudeCodeInfo = backend
            .call_as(Request::ClaudeCodeStatus {
                refresh: true,
                provider: None,
            })
            .await?;
        println!("{}", cc.hint);
        return Ok(());
    }
    if provider != tessera_core::auth::CODEX_PROVIDER {
        bail!("`{provider}` uses an API key: tessera add-provider {provider} --url … --key …");
    }
    if import {
        backend
            .call(Request::CompleteOauth {
                provider: provider.clone(),
                input: "import-codex-cli".into(),
            })
            .await?;
        println!("Imported your Codex CLI ChatGPT login.");
        return Ok(());
    }
    let (start, events) = start_chatgpt_login(backend).await?;
    banner::print_banner("sign in with ChatGPT");
    println!("  1. Open  \x1b[1m{}\x1b[0m  on any device", start.url);
    println!(
        "  2. Sign in and enter the code  \x1b[1;36m{}\x1b[0m\n",
        start.user_code.as_deref().unwrap_or("?")
    );
    println!("  Waiting for approval… (Ctrl+C to cancel)");
    let message = wait_chatgpt_login(events).await?;
    println!("  ✓ {message}");
    let providers: Vec<ProviderInfo> = backend
        .call_as(Request::ListProviders { refresh: false })
        .await?;
    if let Some(p) = providers.iter().find(|p| p.id == provider) {
        println!(
            "  Models: {}",
            p.models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

/// What `connect_server` paired with.
pub struct Connected {
    pub name: String,
    pub version: String,
    pub endpoint: String,
    pub fingerprint: String,
}

/// Pair this terminal with a remote server and remember it (`tessera
/// connect`, and the setup wizard).
pub async fn connect_server(
    paths: &Paths,
    endpoints: Vec<String>,
    code: String,
    headers: BTreeMap<String, String>,
    name: Option<String>,
    expected: Option<String>,
) -> Result<Connected> {
    if code.is_empty() {
        bail!("give the pairing code with --code, or pass the tessera://pair link");
    }
    let device = format!("Terminal on {}", hostname());
    let (endpoint, grant) = tessera_client::pair(&endpoints, &headers, &code, &device).await?;
    let fingerprint = tessera_client::key_fingerprint(&grant.server_key).unwrap_or_default();
    if expected.as_ref().is_some_and(|e| *e != fingerprint) {
        bail!(
            "the server's key ({fingerprint}) doesn't match the pairing link; something may be intercepting the connection. Nothing was saved."
        );
    }
    // Paired with this same server before: that login goes once this one
    // works (Settings → Devices then lists this terminal once).
    let before = backend::load_client_file(paths)
        .filter(|f| !f.machine && f.server.server_key == grant.server_key)
        .map(|f| f.server.token);
    let file = backend::ClientFile {
        name: name.unwrap_or_else(|| grant.server.name.clone()),
        server: tessera_client::ServerConfig {
            endpoints,
            headers,
            token: grant.token,
            server_key: grant.server_key,
        },
        machine: false,
        traded_from: String::new(),
        route: None,
    };
    backend::save_client_file(paths, &file)?;
    if let Some(old) = before.filter(|t| *t != file.server.token) {
        let client = tessera_client::Client::connect(file.server.clone());
        let _ = client
            .call_timeout(
                tessera_proto::Request::RetireLogin { token: old },
                std::time::Duration::from_secs(15),
            )
            .await;
        client.close();
    }
    Ok(Connected {
        name: file.name,
        version: grant.server.version,
        endpoint,
        fingerprint,
    })
}

/// Add or update a provider and discover its models (`tessera
/// add-provider`, and the setup wizard).
pub async fn add_provider(
    backend: &Backend,
    id: String,
    url: String,
    kind: ProviderKind,
    key: Option<String>,
    name: Option<String>,
) -> Result<ProviderInfo> {
    let info = backend
        .call_as(Request::SaveProvider {
            spec: ProviderSpec {
                id: id.clone(),
                name: name.unwrap_or_else(|| id.clone()),
                kind,
                base_url: url,
                api_key: key,
                discover: true,
                models: vec![],
            },
        })
        .await?;
    Ok(info)
}

pub async fn set_default_model(backend: &Backend, model: ModelRef) -> Result<()> {
    backend.call(Request::SetDefaultModel { model }).await?;
    Ok(())
}

/// Start the ChatGPT device sign-in; the receiver sees its outcome.
pub async fn start_chatgpt_login(
    backend: &Backend,
) -> Result<(
    OAuthStart,
    tokio::sync::mpsc::UnboundedReceiver<BackendEvent>,
)> {
    let events = backend.subscribe();
    let start: OAuthStart = backend
        .call_as(Request::StartOauth {
            provider: tessera_core::auth::CODEX_PROVIDER.into(),
        })
        .await?;
    Ok((start, events))
}

/// Wait (up to 15 minutes) for the sign-in to finish; returns the server's
/// message.
pub async fn wait_chatgpt_login(
    mut events: tokio::sync::mpsc::UnboundedReceiver<BackendEvent>,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .map_err(|_| anyhow!("timed out waiting for sign-in"))?;
        match ev {
            Some(BackendEvent::Event(Event::Notice { message, level, .. }))
                if message.contains("ChatGPT") =>
            {
                if level == NoticeLevel::Error {
                    bail!("{message}");
                }
                return Ok(message);
            }
            None => bail!("connection closed"),
            _ => {}
        }
    }
}

fn print_providers(providers: &[ProviderInfo], default: Option<&ModelRef>) {
    for p in providers {
        let auth = if p.authenticated {
            "\x1b[32m●\x1b[0m"
        } else {
            "\x1b[2m○\x1b[0m"
        };
        let account = p
            .account
            .as_deref()
            .map(|a| format!(" ({a})"))
            .unwrap_or_default();
        println!("{auth} {}  {}{account}  {}", p.id, p.name, p.base_url);
        for m in &p.models {
            let is_default = default.is_some_and(|d| d.provider == p.id && d.model == m.id);
            println!(
                "    {} {:<40} {:>5}k ctx{}{}",
                if is_default { "★" } else { " " },
                m.id,
                m.context_window / 1000,
                if m.reasoning { " · reasoning" } else { "" },
                if m.vision { " · vision" } else { "" }
            );
        }
        if !p.authenticated && p.id == tessera_core::auth::CODEX_PROVIDER {
            println!("      sign in: tessera login chatgpt");
        }
        if !p.authenticated && p.kind == ProviderKind::ClaudeCode {
            println!(
                "      sign in: run `claude` once on the server (Tessera runs your Claude Code)"
            );
        }
    }
}
