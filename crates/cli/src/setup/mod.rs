//! `tessera setup`: a guided walk through what Tessera needs, in the
//! terminal (over SSH too). Four short steps, each showing what is already
//! set up, each skippable; running it again is safe:
//!
//! 1. Your AI: Claude or ChatGPT plans, API keys, or model servers found on
//!    this computer and the home network
//! 2. Where you'll use it: this computer, your home network, or anywhere
//!    (Tailscale, a Cloudflare tunnel or your own proxy)
//! 3. Keep it running: the background service (systemd)
//! 4. Your phone: a QR code, or a private link to paste on a computer
//!
//! Then a summary, with the safe defaults in plain words. More is one
//! command away: `tessera setup web|where|security` (web search, connecting
//! to another server, the security summary).
//!
//! Secrets typed here (API keys, tokens, header values) are read hidden,
//! sent straight to the server and never printed.

pub mod banner;
pub mod term;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tessera_core::config::{Config, Paths};
use tessera_proto::*;

use crate::backend::{Backend, BackendChoice, BackendEvent};
use crate::service;
use term::{
    Choice, Mark, PAD, Page, Row, S_ACCENT, S_ERR, S_FAINT, S_HINT, S_LINK, S_OK, S_TEXT, S_WARN,
    Sty, Term,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Step {
    /// Where Tessera runs: this machine, or a server you already have.
    Where,
    /// Your AI: plans, API keys, model servers.
    Models,
    /// Web search backend.
    Web,
    /// Where you'll use it: home network, anywhere.
    Devices,
    /// Keep it running in the background.
    Service,
    /// Pair a phone or computer.
    Pair,
    /// Security summary.
    Security,
}

impl Step {
    /// The guided setup: only what a first run needs. The rest is one
    /// command away (`tessera setup <step>`).
    pub const FLOW: [Step; 4] = [Step::Models, Step::Devices, Step::Service, Step::Pair];

    pub fn title(self) -> &'static str {
        match self {
            Step::Where => "Where Tessera runs",
            Step::Models => "Your AI",
            Step::Web => "Web search",
            Step::Devices => "Where you'll use it",
            Step::Service => "Keep it running",
            Step::Pair => "Your phone",
            Step::Security => "Your security",
        }
    }

    /// The line under the title: what the step is for.
    fn blurb(self) -> &'static str {
        match self {
            Step::Where => "This machine as the server, or a Tessera server you already have.",
            Step::Models => {
                "A plan you already pay for, an API key, or a free model on your own computers."
            }
            Step::Web => "How the agent looks things up online.",
            Step::Devices => "This computer, your phone at home, or anywhere.",
            Step::Service => "In the background, from startup, also when you log out.",
            Step::Pair => "Scan a code with the Tessera app, or paste a link on a computer.",
            Step::Security => "Safe defaults, already on.",
        }
    }

    /// The name in the overview and the summary.
    fn short(self) -> &'static str {
        match self {
            Step::Where => "Where it runs",
            Step::Models => "AI",
            Step::Web => "Web search",
            Step::Devices => "Reachable",
            Step::Service => "Background",
            Step::Pair => "Devices",
            Step::Security => "Security",
        }
    }
}

pub struct Options {
    pub plain: bool,
    pub no_anim: bool,
    /// Run just this step.
    pub only: Option<Step>,
    /// Offered on the first `tessera` launch.
    pub first_run: bool,
    pub force_local: bool,
    /// A custom data directory (`--home` / `TESSERA_HOME`): the service
    /// must use it too.
    pub home: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// This machine is (or will be) the server.
    Server,
    /// This terminal uses a server elsewhere.
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Done,
    Skipped,
}

struct Wizard {
    t: Term,
    paths: Paths,
    opts: Options,
    backend: Backend,
    role: Role,
    /// The steps of this run, and how each went (`None`: not reached).
    flow: Vec<Step>,
    done: Vec<Option<bool>>,
    /// `server.bind` chosen in this run (re-applied at the end, since a
    /// running engine may rewrite the config file).
    bind: Option<String>,
    /// Settings changed that the running server only picks up on restart.
    restart_needed: bool,
}

/// `run` finished and the user doesn't want to start chatting now.
pub const QUIT: i32 = -1;

/// Run the wizard. Returns the process exit code (130 when cancelled,
/// [`QUIT`] when a first-run setup should not continue into the chat).
pub async fn run(paths: Paths, backend: Backend, opts: Options) -> Result<i32> {
    term::install_panic_hook();
    let caps = term::Caps::detect(opts.plain, opts.no_anim);
    let role = match &backend {
        Backend::Remote { local: false, .. } => Role::Client,
        _ => Role::Server,
    };
    let mut w = Wizard {
        t: Term::new(caps),
        paths,
        opts,
        backend,
        role,
        flow: Vec::new(),
        done: Vec::new(),
        bind: None,
        restart_needed: false,
    };
    let r = w.run().await;
    w.backend.close();
    match r {
        Ok(code) => Ok(code),
        Err(e) if e.is::<term::Aborted>() => {
            w.apply_bind();
            println!();
            w.t.para(
                "Setup paused; nothing else was changed. Run `tessera setup` to pick up where you left off.",
                S_HINT,
            );
            Ok(130)
        }
        Err(e) => {
            w.apply_bind();
            Err(e)
        }
    }
}

/// What to do on the first launch.
pub enum Offer {
    Run,
    Later,
    Never,
}

/// Ask whether to run the wizard on the first `tessera` launch: the
/// welcome page.
pub async fn offer(plain: bool) -> Result<Offer> {
    let caps = term::Caps::detect(plain, false);
    let mut t = Term::new(caps);
    t.clear_screen();
    banner::play(&mut t, "welcome").await?;
    t.para(
        "Tessera isn't set up yet. Four short steps: your AI, where you'll use it, keeping it running, and your phone.",
        S_HINT,
    );
    t.blank();
    let choices = [
        Choice::new("Set up now")
            .tag("recommended")
            .hint("A few minutes. Skip any step you like."),
        Choice::new("Not now").hint("Start Tessera; you'll be asked again next time."),
        Choice::new("Don't ask again").hint("`tessera setup` is there whenever you want it."),
    ];
    t.esc_word = "not now";
    let r = match t.select("Set up Tessera?", &choices, 0).await {
        Ok(r) => r,
        Err(e) if e.is::<term::Aborted>() => return Ok(Offer::Later),
        Err(e) => return Err(e),
    };
    Ok(match r {
        Some(0) => Offer::Run,
        Some(2) => {
            t.para(
                "Okay. `tessera setup` is there whenever you want it.",
                S_HINT,
            );
            Offer::Never
        }
        _ => Offer::Later,
    })
}

/// `tessera pair`: show a pairing code and wait for a device.
pub async fn pair(backend: Backend, plain: bool, qr: bool) -> Result<i32> {
    term::install_panic_hook();
    let mut t = Term::new(term::Caps::detect(plain, false));
    t.page(&Page {
        title: Step::Pair.title(),
        sub: Step::Pair.blurb(),
        steps: &[],
    });
    let r = show_pairing(&mut t, &backend, qr).await;
    backend.close();
    match r {
        Ok(Some(name)) => {
            t.ok(&format!("Paired “{name}”. It shows the same server key."));
            Ok(0)
        }
        Ok(None) => Ok(0),
        Err(e) if e.is::<term::Aborted>() => Ok(130),
        Err(e) => Err(e),
    }
}

// ---- pure helpers (unit-tested) ------------------------------------------------------------

/// The exact Cloudflare API token recipe.
pub const TOKEN_PERMISSIONS: [(&str, &str, &str); 5] = [
    ("Account", "Cloudflare Tunnel", "Edit"),
    ("Account", "Access: Apps and Policies", "Edit"),
    ("Account", "Access: Service Tokens", "Edit"),
    ("Zone", "DNS", "Edit"),
    ("Zone", "Zone", "Read"),
];

/// How to create the token, as numbered steps; the permission rows (with
/// `·` between their parts) belong to step 3.
pub fn token_recipe() -> Vec<String> {
    let mut v = vec![
        "1  dash.cloudflare.com/profile/api-tokens (My Profile → API Tokens)".to_string(),
        "2  Create Token → Create Custom Token; name it “Tessera setup”".into(),
        "3  Permissions, one row each (“+ Add more”):".into(),
    ];
    for (scope, perm, level) in TOKEN_PERMISSIONS {
        v.push(format!("     {scope:<8}· {perm:<26}· {level}"));
    }
    v.extend([
        "4  Account Resources: Include · your account".to_string(),
        "   Zone Resources: Include · Specific zone · your domain".into(),
        "5  TTL: tomorrow is plenty (it's needed once)".into(),
        "6  Continue to summary → Create Token, and copy it".into(),
    ]);
    v
}

/// Check a hostname for the tunnel: a valid DNS name inside `zone`.
pub fn check_hostname(h: &str, zone: &str) -> Result<String, String> {
    let h = h
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let zone = zone.trim_end_matches('.').to_ascii_lowercase();
    if h != zone && !h.ends_with(&format!(".{zone}")) {
        return Err(format!("the name must end in .{zone}"));
    }
    if h.len() > 253 {
        return Err("that name is too long".into());
    }
    for label in h.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return Err(format!(
                "“{label}” isn't a valid name part (letters, digits and dashes)"
            ));
        }
    }
    Ok(h)
}

pub fn check_url(u: &str) -> Result<(), String> {
    let u = u.trim();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or("start with http:// or https://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) || host.contains('@') {
        return Err("that doesn't look like a web address".into());
    }
    Ok(())
}

/// HTTP header name (RFC 9110 token).
pub fn check_header_name(n: &str) -> Result<(), String> {
    if n.is_empty()
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
    {
        return Err("letters, digits and dashes only, e.g. CF-Access-Client-Id".into());
    }
    Ok(())
}

pub fn check_code(c: &str) -> Result<(), String> {
    if c.chars().filter(char::is_ascii_digit).count() == 8
        && c.chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == ' ')
    {
        Ok(())
    } else {
        Err("the code has 8 digits, like 1234-5678".into())
    }
}

/// Provider presets offered for API keys, in the order shown:
/// (label, id, name, kind, url, where to get a key).
pub const KEY_PRESETS: [(&str, &str, &str, ProviderKind, &str, &str); 3] = [
    (
        "OpenRouter (hundreds of models with one key)",
        "openrouter",
        "OpenRouter",
        ProviderKind::OpenaiChat,
        "https://openrouter.ai/api/v1",
        "Create a key at openrouter.ai → Keys.",
    ),
    (
        "OpenAI",
        "openai",
        "OpenAI",
        ProviderKind::OpenaiResponses,
        "https://api.openai.com/v1",
        "Create a key at platform.openai.com → API keys.",
    ),
    (
        "Anthropic (Claude, paid per use)",
        "anthropic",
        "Anthropic",
        ProviderKind::Anthropic,
        "https://api.anthropic.com",
        "Create a key at console.anthropic.com → API keys.",
    ),
];

/// The provider that runs the user's own Claude Code (their Claude plan).
pub const CLAUDE_CODE: &str = "claude-code";

/// Anthropic's installer for Claude Code.
#[cfg(not(windows))]
pub const CLAUDE_INSTALL: &str = "curl -fsSL https://claude.ai/install.sh | bash";
/// Anthropic's Windows installer, run in PowerShell.
#[cfg(windows)]
pub const CLAUDE_INSTALL: &str = "irm https://claude.ai/install.ps1 | iex";

/// A provider id from a base URL ("http://gpu-box:8000/v1" → "gpu-box").
pub fn provider_id_from_url(url: &str) -> String {
    let host = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    let id: String = host
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches('-').to_string();
    if id.is_empty() || id == "127" || id == "localhost" {
        "custom".into()
    } else {
        id
    }
}

/// `host:port` with the host changed; keeps the port (default 7878).
pub fn with_host(bind: &str, host: &str) -> String {
    format!("{host}:{}", port_of(bind))
}

fn port_of(bind: &str) -> u16 {
    bind.parse::<std::net::SocketAddr>()
        .map(|a| a.port())
        .unwrap_or(7878)
}

pub fn is_loopback(bind: &str) -> bool {
    bind.parse::<std::net::SocketAddr>()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(true)
}

fn usable(p: &ProviderInfo) -> bool {
    (p.authenticated || p.auth == AuthMethod::None) && !p.models.is_empty()
}

/// A model server on this machine or the home network (what looking around
/// finds), as opposed to a hosted API.
pub fn is_found_server(u: &str) -> bool {
    let rest = u
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host = rest.split('/').next().unwrap_or("");
    let host = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "localhost" || host.ends_with(".local") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// This machine on Tailscale.
struct Tailnet {
    /// MagicDNS name, e.g. `my-server.tail1234.ts.net`.
    dns: String,
    ip: Option<String>,
}

/// Parse `tailscale status --json`.
fn parse_tailnet(json: &[u8]) -> Option<Tailnet> {
    let v: Value = serde_json::from_slice(json).ok()?;
    if v["BackendState"].as_str() != Some("Running") {
        return None;
    }
    let dns = v["Self"]["DNSName"]
        .as_str()?
        .trim_end_matches('.')
        .to_string();
    if dns.is_empty() {
        return None;
    }
    let ip = v["Self"]["TailscaleIPs"]
        .as_array()
        .and_then(|a| a.iter().filter_map(Value::as_str).find(|s| s.contains('.')))
        .map(str::to_string);
    Some(Tailnet { dns, ip })
}

async fn tailnet() -> Option<Tailnet> {
    let out = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new("tailscale")
            .args(["status", "--json"])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| parse_tailnet(&out.stdout))
        .flatten()
}

/// A ✓ for "set up", a ○ for "not yet".
fn mark(ok: bool) -> (&'static str, Sty) {
    if ok { ("✓", S_OK) } else { ("○", S_FAINT) }
}

// ---- the wizard ----------------------------------------------------------------------------

impl Wizard {
    fn b(&self) -> Backend {
        self.backend.clone()
    }

    async fn call<T: serde::de::DeserializeOwned>(&self, req: Request) -> Result<T, ApiError> {
        self.backend.call_as(req).await
    }

    async fn status(&self) -> Option<SetupStatus> {
        self.call(Request::SetupStatus).await.ok()
    }

    async fn remote_status(&self) -> Option<RemoteStatus> {
        self.call(Request::RemoteStatus).await.ok()
    }

    async fn reopen(&mut self) -> Result<()> {
        let paths = self.paths.clone();
        let force_local = self.opts.force_local;
        let r = self
            .t
            .spin("Connecting…", async move {
                Backend::open(&paths, BackendChoice { force_local }).await
            })
            .await?;
        let next = r?;
        self.backend.close();
        self.backend = next;
        self.role = match &self.backend {
            Backend::Remote { local: false, .. } => Role::Client,
            _ => Role::Server,
        };
        Ok(())
    }

    /// The background service, when it runs this data directory (one for
    /// another Tessera on this machine isn't ours to report).
    fn own_service(&self) -> Option<service::Summary> {
        (self.role == Role::Server)
            .then(|| service::summary(service::DEFAULT_NAME))
            .flatten()
            .filter(|s| s.home == self.opts.home)
    }

    fn apply_bind(&self) {
        let Some(bind) = &self.bind else { return };
        if let Ok(mut cfg) = Config::load(&self.paths)
            && cfg.server.bind != *bind
        {
            cfg.server.bind = bind.clone();
            let _ = cfg.save(&self.paths);
        }
    }

    /// `server.bind` as it will be once the server restarts.
    fn configured_bind(&self) -> String {
        self.bind.clone().unwrap_or_else(|| {
            Config::load(&self.paths)
                .map(|c| c.server.bind)
                .unwrap_or_else(|_| "127.0.0.1:7878".into())
        })
    }

    async fn run(&mut self) -> Result<i32> {
        if let Some(step) = self.opts.only {
            self.t.page(&Page {
                title: step.title(),
                sub: step.blurb(),
                steps: &[],
            });
            let flow = self.step(step).await?;
            if step == Step::Security && flow == Flow::Done {
                let _ = self.backend.call(Request::CompleteSetup).await;
            }
            self.apply_bind();
            println!();
            return Ok(0);
        }
        // A terminal that uses (or used) a server elsewhere starts with that.
        let elsewhere =
            self.role == Role::Client || crate::backend::load_client_file(&self.paths).is_some();
        self.flow = elsewhere
            .then_some(Step::Where)
            .into_iter()
            .chain(Step::FLOW)
            .collect();
        self.done = vec![None; self.flow.len()];
        let fresh = self.status().await.is_none_or(|s| !s.completed);
        if !fresh && !self.opts.first_run {
            return self.dashboard().await;
        }
        if !self.opts.first_run && !self.welcome().await? {
            return Ok(0);
        }
        for i in 0..self.flow.len() {
            let step = self.flow[i];
            self.header(i);
            self.done[i] = Some(self.step(step).await? == Flow::Done);
            self.t.pause().await?;
        }
        self.finish().await?;
        if self.opts.first_run {
            println!();
            let go = self
                .t
                .confirm("Start chatting now?", true)
                .await?
                .unwrap_or(false);
            if !go {
                return Ok(QUIT);
            }
        }
        Ok(0)
    }

    /// The first page of a guided setup: the banner, the plan and a start.
    /// Returns false when the user leaves.
    async fn welcome(&mut self) -> Result<bool> {
        self.t.clear_screen();
        banner::play(&mut self.t, "setup").await?;
        let n = match self.flow.len() {
            4 => "Four",
            5 => "Five",
            _ => "A few",
        };
        self.t.para(
            &format!(
                "{n} short steps get Tessera ready. Each shows what is set up already; skip any of them."
            ),
            S_HINT,
        );
        println!();
        let rows = self.overview().await;
        self.t.card(&rows);
        println!();
        let choices = [
            Choice::new("Start").tag("recommended"),
            Choice::new("Not now").hint("`tessera setup` comes back here."),
        ];
        Ok(self.t.select("Ready?", &choices, 0).await? == Some(0))
    }

    /// Setup was done before: what is set up, and a step to change.
    async fn dashboard(&mut self) -> Result<i32> {
        loop {
            self.t.page(&Page {
                title: "What's set up",
                sub: "Pick something to change, or leave it as it is.",
                steps: &[],
            });
            let rows = self.overview().await;
            self.t.card(&rows);
            println!();
            let mut steps = self.flow.clone();
            steps.extend([Step::Web, Step::Security]);
            let mut choices: Vec<Choice> = steps
                .iter()
                .map(|s| Choice::new(s.title()).hint(s.blurb()))
                .collect();
            choices.push(Choice::new("Done").tag("leave it as it is"));
            let done = choices.len() - 1;
            self.t.esc_word = "done";
            let picked = self.t.select("Change something?", &choices, done).await;
            self.t.esc_word = "skip";
            let Some(i) = picked?.filter(|i| *i < steps.len()) else {
                break;
            };
            let step = steps[i];
            self.t.page(&Page {
                title: step.title(),
                sub: step.blurb(),
                steps: &[],
            });
            self.step(step).await?;
            self.apply_bind();
            self.t.pause().await?;
        }
        self.apply_bind();
        Ok(0)
    }

    async fn step(&mut self, step: Step) -> Result<Flow> {
        match step {
            Step::Where => self.step_where().await,
            Step::Models => self.step_models().await,
            Step::Web => self.step_web().await,
            Step::Devices => self.step_devices().await,
            Step::Service => self.step_service().await,
            Step::Pair => self.step_pair().await,
            Step::Security => self.step_security().await,
        }
    }

    /// What is set up already, one row per step (✓ set up, ○ not yet).
    async fn overview(&mut self) -> Vec<Row> {
        let s = self.status().await;
        let remote = self.remote_status().await;
        let svc = self.own_service();
        let mut rows = Vec::new();
        for step in self.flow.clone() {
            let (ok, v) = match step {
                Step::Where => match (&self.backend, self.role) {
                    (_, Role::Client) => (true, "a server elsewhere".to_string()),
                    (Backend::Remote { .. }, _) => (true, "this machine · server running".into()),
                    _ => (false, "this machine".into()),
                },
                Step::Models => {
                    let model = s.as_ref().and_then(|s| {
                        s.default_model
                            .as_ref()
                            .filter(|_| s.has_model)
                            .map(|m| m.to_string())
                    });
                    (model.is_some(), model.unwrap_or_else(|| "none yet".into()))
                }
                Step::Devices => {
                    let lan = s.as_ref().is_some_and(|s| s.lan);
                    let tunnel = remote.as_ref().and_then(|r| r.tunnel.as_ref());
                    let anywhere = remote.as_ref().is_some_and(|r| !r.urls.is_empty());
                    let mut v = match (lan, anywhere) {
                        (false, false) => "this computer only".to_string(),
                        (true, false) => "at home".into(),
                        (false, true) => "from anywhere".into(),
                        (true, true) => "at home and anywhere".into(),
                    };
                    if tunnel.is_some_and(|t| !t.connected) {
                        v.push_str(" · tunnel not connected");
                    }
                    (lan || anywhere, v)
                }
                Step::Service => match (self.role, &svc) {
                    (Role::Client, _) => (true, "runs on the server".to_string()),
                    (_, Some(s)) if s.active => (true, "running".into()),
                    (_, Some(_)) => (false, "installed, not running".into()),
                    (_, None) => (false, "not yet".into()),
                },
                Step::Pair => {
                    let n = s.as_ref().map_or(0, |s| s.devices as usize);
                    (
                        n > 0,
                        if n == 0 {
                            "none paired yet".into()
                        } else {
                            format!("{n} paired")
                        },
                    )
                }
                Step::Web | Step::Security => continue,
            };
            let (m, st) = mark(ok);
            let r = Row::new(step.short(), v).mark(m, st);
            rows.push(if ok { r } else { r.missing() });
        }
        rows
    }

    /// The `i`th step's page, with where the run is.
    fn header(&mut self, i: usize) {
        let step = self.flow[i];
        let steps: Vec<(&str, Mark)> = self
            .flow
            .iter()
            .zip(&self.done)
            .enumerate()
            .map(|(k, (s, d))| {
                let m = match d {
                    _ if k == i => Mark::Now,
                    Some(true) => Mark::Done,
                    Some(false) => Mark::Skipped,
                    None => Mark::Next,
                };
                (s.short(), m)
            })
            .collect();
        self.t.page(&Page {
            title: step.title(),
            sub: step.blurb(),
            steps: &steps,
        });
    }

    // ---- 1. where ----

    async fn step_where(&mut self) -> Result<Flow> {
        if self.role == Role::Client {
            let name = crate::backend::load_client_file(&self.paths)
                .map(|c| c.name)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "a Tessera server".into());
            self.t.rows(&[
                Row::new("Server", &name),
                Row::new("Connection", self.backend.describe()),
            ]);
            println!();
            let choices = [
                Choice::new(format!("Keep using {name}")).tag("current"),
                Choice::new("Make this machine the server instead")
                    .hint("Forget the remote server; Tessera runs here."),
                Choice::new("Connect to a different server")
                    .hint("Needs the pairing link from `tessera pair` on that server."),
            ];
            return match self
                .t
                .select("Where should Tessera run?", &choices, 0)
                .await?
            {
                Some(1) => {
                    let _ = std::fs::remove_file(crate::backend::client_file(&self.paths));
                    self.reopen().await?;
                    self.t.ok("This machine is now your Tessera server.");
                    Ok(Flow::Done)
                }
                Some(2) => Ok(if self.connect().await? {
                    Flow::Done
                } else {
                    Flow::Skipped
                }),
                Some(_) => Ok(Flow::Done),
                None => Ok(Flow::Skipped),
            };
        }
        if let Some(saved) = crate::backend::load_client_file(&self.paths)
            && matches!(self.backend, Backend::Local(_))
        {
            let name = if saved.name.is_empty() {
                "the saved server".to_string()
            } else {
                saved.name.clone()
            };
            self.t.warn(&format!(
                "This terminal uses {name} ({}), but it can't be reached right now.",
                saved.server.endpoints.join(", ")
            ));
            println!();
            let choices = [
                Choice::new("Make this machine the server instead")
                    .hint(format!("Forget {name} on this terminal.")),
                Choice::new("Pair with a server again")
                    .hint("Needs the pairing link from `tessera pair` on the server."),
                Choice::new(format!("Keep using {name}"))
                    .hint("Stop here and try again when it's reachable."),
            ];
            return match self
                .t
                .select("What would you like to do?", &choices, 0)
                .await?
            {
                Some(0) => {
                    let _ = std::fs::remove_file(crate::backend::client_file(&self.paths));
                    self.reopen().await?;
                    self.t.ok("This machine is now your Tessera server.");
                    Ok(Flow::Done)
                }
                Some(1) => Ok(if self.connect().await? {
                    Flow::Done
                } else {
                    Flow::Skipped
                }),
                _ => Err(term::Aborted.into()),
            };
        }
        let row = match &self.backend {
            Backend::Remote { label, .. } => Row::new(
                "This machine",
                format!(
                    "server running at {}",
                    label.trim_start_matches("local server ")
                ),
            ),
            Backend::Local(_) => Row::new("This machine", "no server running yet").missing(),
        };
        self.t.rows(&[row]);
        println!();
        let choices = [
            Choice::new("This machine is the server")
                .tag("recommended")
                .hint("A home server, a VM, or this computer; your devices connect to it."),
            Choice::new("Connect to a server I already have")
                .hint("Needs the pairing link from `tessera pair` on that server."),
        ];
        loop {
            match self
                .t
                .select("Where will Tessera run?", &choices, 0)
                .await?
            {
                Some(1) => {
                    if self.connect().await? {
                        return Ok(Flow::Done);
                    }
                }
                Some(_) => return Ok(Flow::Done),
                None => return Ok(Flow::Skipped),
            }
        }
    }

    /// Pair this terminal with a remote server. Returns whether it worked.
    async fn connect(&mut self) -> Result<bool> {
        println!();
        self.t.para(
            "On the server, run `tessera pair` (or open Settings → Devices in the app) and paste the link it shows. It starts with tessera://.",
            S_HINT,
        );
        loop {
            let Some(input) = self
                .t
                .input(
                    "Pairing link or server address",
                    "",
                    "tessera://pair?…  or  http://192.168.1.10:7878",
                    |s| {
                        if s.starts_with("tessera://") {
                            PairLink::parse(s).map(|_| ()).map_err(|e| e.to_string())
                        } else {
                            check_url(s)
                        }
                    },
                )
                .await?
            else {
                return Ok(false);
            };
            let (endpoints, headers, code, name, expected) = if input.starts_with("tessera://") {
                let link = PairLink::parse(&input).map_err(|e| anyhow::anyhow!(e))?;
                (
                    link.urls,
                    link.headers.into_iter().collect::<BTreeMap<_, _>>(),
                    link.code,
                    Some(link.name).filter(|n| !n.is_empty()),
                    link.fingerprint,
                )
            } else {
                let Some(code) = self
                    .t
                    .input("Pairing code", "", "1234-5678", check_code)
                    .await?
                else {
                    return Ok(false);
                };
                (
                    vec![input.trim().to_string()],
                    BTreeMap::new(),
                    code,
                    None,
                    None,
                )
            };
            let paths = self.paths.clone();
            let r = self
                .t
                .spin(
                    "Pairing (end-to-end encrypted)…",
                    crate::connect_server(&paths, endpoints, code, headers, name, expected),
                )
                .await?;
            match r {
                Ok(c) => {
                    self.t.ok(&format!(
                        "Connected to {} ({}) via {}, end-to-end encrypted.",
                        c.name, c.version, c.endpoint
                    ));
                    self.t.hint(&format!(
                        "Server key {}; `tessera pair` on the server shows the same.",
                        c.fingerprint
                    ));
                    self.reopen().await?;
                    return Ok(true);
                }
                Err(e) => {
                    self.t.err(&format!("Pairing failed: {e}"));
                    if !self.retry().await? {
                        return Ok(false);
                    }
                }
            }
        }
    }

    // ---- 2. models ----

    async fn step_models(&mut self) -> Result<Flow> {
        let here = if self.role == Role::Client {
            "on the server and its network"
        } else {
            "on this computer and your network"
        };
        // Found by looking around: model servers, and the user's Claude Code.
        let mut looked = false;
        let mut found: Vec<ProviderInfo> = Vec::new();
        let mut cc: Option<ClaudeCodeInfo> = None;
        let mut show_state = true;
        let fresh = self.role == Role::Server
            && crate::backend::load_client_file(&self.paths).is_none()
            && self.opts.only.is_none();
        loop {
            let status = self.status().await;
            let has_model = status.as_ref().is_some_and(|s| s.has_model);
            let default = status.as_ref().and_then(|s| s.default_model.clone());
            if show_state {
                show_state = false;
                if has_model && let Some(m) = &default {
                    self.t
                        .rows(&[Row::new("Now", format!("new chats use {m}"))]);
                    println!();
                }
            }
            // Before asking anything, see what is there already.
            if !looked && !has_model {
                looked = true;
                (found, cc) = self.look_around(here).await?;
                println!();
            }
            let providers: Vec<ProviderInfo> = self
                .call(Request::ListProviders { refresh: false })
                .await
                .unwrap_or_default();
            let codex = providers
                .iter()
                .any(|p| p.id == tessera_core::auth::CODEX_PROVIDER && p.authenticated);
            let models = providers
                .iter()
                .filter(|p| usable(p))
                .map(|p| p.models.len())
                .sum::<usize>();
            #[derive(PartialEq, Clone, Copy)]
            enum A {
                Continue,
                Claude,
                ChatGpt,
                Local(usize),
                LookAgain,
                Key,
                Connect,
                Default,
                Memory,
                Skip,
            }
            let mut actions: Vec<(A, Choice)> = Vec::new();
            if has_model {
                actions.push((
                    A::Continue,
                    Choice::new(format!(
                        "Keep {}",
                        default.as_ref().map(|m| m.to_string()).unwrap_or_default()
                    ))
                    .tag("current"),
                ));
            }
            let (claude_hint, claude_tag) = match &cc {
                Some(c) if c.signed_in => (
                    match &c.account {
                        Some(a) => format!("Through your own Claude Code, signed in as {a}."),
                        None => "Through your own Claude Code, which is signed in.".into(),
                    },
                    "ready",
                ),
                Some(c) if c.installed => (
                    "Claude Code is installed; sign in to it once with your Claude account.".into(),
                    "sign in",
                ),
                _ if self.role == Role::Server => (
                    "Tessera installs Claude Code for you; you sign in to it once.".into(),
                    "",
                ),
                _ => (
                    "Through Claude Code on the server, signed in with your Claude account.".into(),
                    "",
                ),
            };
            actions.push((
                A::Claude,
                Choice::new("Claude · your Pro or Max plan")
                    .hint(claude_hint)
                    .tag(claude_tag),
            ));
            actions.push((
                A::ChatGpt,
                Choice::new("ChatGPT · your Plus or Pro plan")
                    .hint("Sign in once, in a browser on any device. Works over SSH too.")
                    .tag(if codex { "signed in" } else { "" }),
            ));
            for (k, p) in found.iter().enumerate() {
                let names: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).take(3).collect();
                actions.push((
                    A::Local(k),
                    Choice::new(&p.name)
                        .hint(format!(
                            "{}{} · private and free, on your own hardware",
                            names.join(", "),
                            if p.models.len() > 3 { ", …" } else { "" }
                        ))
                        .tag("found"),
                ));
            }
            if found.is_empty() {
                actions.push((
                    A::LookAgain,
                    Choice::new("AI on your own computer").hint(
                        "llama.cpp, Ollama or LM Studio: private and free. Start one, then retry.",
                    ),
                ));
            }
            actions.push((
                A::Key,
                Choice::new("An API key")
                    .hint("OpenRouter, OpenAI, Anthropic, or any OpenAI-style server."),
            ));
            if fresh && !has_model {
                actions.push((
                    A::Connect,
                    Choice::new("A Tessera server I already have")
                        .hint("Connect with the link `tessera pair` shows there."),
                ));
            }
            if has_model && models > 1 {
                actions.push((
                    A::Default,
                    Choice::new("Choose the default model")
                        .hint("The model new chats use. Each chat can still switch with /model."),
                ));
            }
            if has_model
                && self.opts.only == Some(Step::Models)
                && let Some(r) = self.memory_role().await
            {
                let tag = match (&r.value, &r.resolved) {
                    (Some(_), Some(m)) => m.model.clone(),
                    _ => String::new(),
                };
                actions.push((
                    A::Memory,
                    Choice::new(format!("{} model", r.label))
                        .hint("Reviews chats to save memories and skills; a small, cheap model is plenty.")
                        .tag(&tag),
                ));
            }
            if !has_model {
                actions.push((
                    A::Skip,
                    Choice::new("Skip for now").hint(
                        "Tessera can't answer without one; `tessera setup models` comes back.",
                    ),
                ));
            }
            let ready = cc.as_ref().is_some_and(|c| c.signed_in);
            let default_action = if has_model {
                A::Continue
            } else if ready {
                A::Claude
            } else if !found.is_empty() {
                A::Local(0)
            } else if codex {
                A::ChatGpt
            } else {
                A::Claude
            };
            let def = actions
                .iter()
                .position(|(a, _)| *a == default_action)
                .unwrap_or(0);
            let choices: Vec<Choice> = actions.iter().map(|(_, c)| c.clone()).collect();
            let q = if has_model {
                "Anything to change?"
            } else {
                "How should Tessera get its AI?"
            };
            let Some(i) = self.t.select(q, &choices, def).await? else {
                return Ok(if has_model { Flow::Done } else { Flow::Skipped });
            };
            match actions[i].0 {
                A::Continue => return Ok(Flow::Done),
                A::Skip => return Ok(Flow::Skipped),
                A::Claude => self.claude_code(has_model).await?,
                A::ChatGpt => self.chatgpt(has_model).await?,
                A::Local(k) => {
                    let p = found[k].clone();
                    self.use_server(&p, has_model).await?;
                }
                A::LookAgain => {
                    (found, cc) = self.look_around(here).await?;
                }
                A::Key => self.add_key(None, has_model).await?,
                A::Connect => {
                    if self.connect().await? {
                        // The server's own state from here on.
                        found.clear();
                        cc = None;
                        show_state = true;
                    }
                }
                A::Default => {
                    self.choose_default(None).await?;
                }
                A::Memory => {
                    self.choose_memory_model().await?;
                }
            }
            println!();
            // A first model chosen: that's this step done.
            if !has_model && self.status().await.is_some_and(|s| s.has_model) {
                return Ok(Flow::Done);
            }
        }
    }

    /// Look for model servers (this machine and its network) and the user's
    /// own Claude Code, and say what is there. Nothing changes but the
    /// servers found being added as providers.
    async fn look_around(
        &mut self,
        here: &str,
    ) -> Result<(Vec<ProviderInfo>, Option<ClaudeCodeInfo>)> {
        let b = self.b();
        let (servers, cc) = self
            .t
            .spin(&format!("Looking for AI {here}…"), async move {
                tokio::join!(
                    b.call_as::<Vec<ProviderInfo>>(Request::DetectLocalModels),
                    b.call_as::<ClaudeCodeInfo>(Request::ClaudeCodeStatus {
                        refresh: true,
                        provider: None,
                    })
                )
            })
            .await?;
        let found: Vec<ProviderInfo> = match servers {
            Ok(list) => list
                .into_iter()
                .filter(|p| is_found_server(&p.base_url) && usable(p))
                .collect(),
            Err(e) => {
                self.t
                    .warn(&format!("Couldn't look for AI servers: {}", e.message));
                Vec::new()
            }
        };
        let cc = cc.ok();
        let mut rows = Vec::new();
        if let Some(c) = cc.as_ref().filter(|c| c.installed) {
            let mut v = format!(
                "Claude Code{}",
                c.version
                    .as_ref()
                    .map(|v| format!(" {v}"))
                    .unwrap_or_default()
            );
            v.push_str(if c.signed_in {
                " · signed in"
            } else {
                " · not signed in yet"
            });
            if let Some(plan) = &c.plan {
                v.push_str(&format!(" · {plan}"));
            }
            rows.push(Row::new("Found", v));
        }
        for p in &found {
            rows.push(Row::new(
                if rows.is_empty() { "Found" } else { "" },
                format!("{} · {}", p.name, plural(p.models.len(), "model")),
            ));
        }
        if rows.is_empty() {
            self.t.hint(&format!(
                "No AI servers or Claude Code {here} yet. That's fine: pick a way below."
            ));
        } else {
            self.t.rows(&rows);
        }
        Ok((found, cc))
    }

    /// Make a found model server the default: its only model, or one the
    /// user picks.
    async fn use_server(&mut self, p: &ProviderInfo, has_model: bool) -> Result<()> {
        if let [m] = p.models.as_slice() {
            let r = ModelRef {
                provider: p.id.clone(),
                model: m.id.clone(),
            };
            if has_model
                && !self
                    .t
                    .confirm(&format!("Make {r} the default?"), true)
                    .await?
                    .unwrap_or(false)
            {
                return Ok(());
            }
            match crate::set_default_model(&self.backend, r.clone()).await {
                Ok(()) => self.t.ok(&format!("New chats use {r}.")),
                Err(e) => self.t.err(&e.to_string()),
            }
            return Ok(());
        }
        self.choose_default(Some(&p.id)).await?;
        Ok(())
    }

    /// The `memory` model role, when the engine has it.
    async fn memory_role(&self) -> Option<ModelRole> {
        let roles: ModelRoles = self.call(Request::GetModelRoles).await.ok()?;
        roles.roles.into_iter().find(|r| r.name == "memory")
    }

    /// Pick the memory & learning model (or unset it).
    async fn choose_memory_model(&mut self) -> Result<bool> {
        let Some(role) = self.memory_role().await else {
            self.t
                .todo("This Tessera server has no memory model setting; update it first.");
            return Ok(false);
        };
        let providers: Vec<ProviderInfo> = self
            .call(Request::ListProviders { refresh: false })
            .await
            .unwrap_or_default();
        let fallback = if role.value.is_none() {
            role.resolved.as_ref().map(|m| m.to_string())
        } else {
            None
        };
        let mut values: Vec<Option<String>> = vec![None];
        let mut choices = vec![
            Choice::new("Not set")
                .hint(match &fallback {
                    Some(m) => format!("Uses {m} (the utility model, else the chat's own)"),
                    None => "Uses the utility model, else the chat's own".to_string(),
                })
                .tag(if role.value.is_none() { "current" } else { "" }),
        ];
        for p in providers.iter().filter(|p| usable(p)) {
            for m in &p.models {
                let r = format!("{}/{}", p.id, m.id);
                let mut hint = format!(
                    "{} · {} context",
                    p.name,
                    crate::tui::text::tokens(m.context_window)
                );
                if m.reasoning {
                    hint.push_str(" · reasoning");
                }
                choices.push(Choice::new(r.clone()).hint(hint).tag(
                    if role.value.as_deref() == Some(r.as_str()) {
                        "current"
                    } else {
                        ""
                    },
                ));
                values.push(Some(r));
            }
        }
        let def = values.iter().position(|v| *v == role.value).unwrap_or(0);
        let filter = choices.len() > 9;
        let Some(i) = self
            .t
            .select_opts(&format!("{} model", role.label), &choices, def, filter)
            .await?
        else {
            return Ok(false);
        };
        let model = values[i].clone();
        match self
            .call::<ModelRoles>(Request::SetModelRole {
                role: "memory".into(),
                model: model.clone(),
            })
            .await
        {
            Ok(roles) => {
                let now = roles
                    .roles
                    .iter()
                    .find(|r| r.name == "memory")
                    .and_then(|r| r.resolved.as_ref())
                    .map(|m| m.to_string());
                match (model, now) {
                    (Some(m), _) => self
                        .t
                        .ok(&format!("Memories and skills are reviewed by {m}.")),
                    (None, Some(m)) => self.t.ok(&format!("Not set: the review uses {m}.")),
                    (None, None) => self.t.ok("Not set: the review uses each chat's own model."),
                }
                Ok(true)
            }
            Err(e) => {
                self.t.err(&e.message);
                Ok(false)
            }
        }
    }

    /// Sign in with a ChatGPT plan (a code to approve in any browser), then
    /// pick one of its models.
    async fn chatgpt(&mut self, has_model: bool) -> Result<()> {
        let b = self.b();
        let started = crate::start_chatgpt_login(&b).await;
        let (start, events) = match started {
            Ok(s) => s,
            Err(e) => {
                self.t.err(&format!("Sign-in couldn't start: {e}"));
                return Ok(());
            }
        };
        println!();
        self.t.rows(&[
            Row::new("1  Open", &start.url).st(S_LINK.bold()),
            Row::new("2  Enter", start.user_code.as_deref().unwrap_or("?")).st(S_ACCENT.bold()),
        ]);
        println!();
        let r = self
            .t
            .spin_esc(
                "Waiting for you to approve the sign-in…",
                crate::wait_chatgpt_login(events),
            )
            .await?;
        match r {
            Some(Ok(msg)) => {
                self.t.ok(&msg);
                let make_default = !has_model
                    || self
                        .t
                        .confirm("Make a ChatGPT model the default?", true)
                        .await?
                        .unwrap_or(false);
                if make_default {
                    self.choose_default(Some(tessera_core::auth::CODEX_PROVIDER))
                        .await?;
                }
            }
            Some(Err(e)) => self.t.err(&e.to_string()),
            None => self
                .t
                .todo("Stopped waiting. If you finish signing in later, it still completes."),
        }
        Ok(())
    }

    /// Use the user's own Claude Code: on this machine, install it and let
    /// it sign in if needed (Tessera never handles the Claude login
    /// itself), then pick a model.
    async fn claude_code(&mut self, has_model: bool) -> Result<()> {
        let here = self.role == Role::Server;
        let mut offered_install = false;
        let mut offered_sign_in = false;
        loop {
            let b = self.b();
            let info = self
                .t
                .spin("Looking for Claude Code…", async move {
                    b.call_as::<ClaudeCodeInfo>(Request::ClaudeCodeStatus {
                        refresh: true,
                        provider: None,
                    })
                    .await
                })
                .await?;
            let info = match info {
                Ok(i) => i,
                Err(e) => {
                    self.t.warn(&format!(
                        "This Tessera server can't use Claude Code ({}). Update Tessera, then run `tessera setup models` again.",
                        e.message
                    ));
                    return Ok(());
                }
            };
            if info.installed && info.signed_in {
                let mut rows = vec![Row::new(
                    "Claude Code",
                    match &info.version {
                        Some(v) => format!("{v} · signed in"),
                        None => "signed in".into(),
                    },
                )];
                if let Some(plan) = &info.plan {
                    rows.push(Row::new("Plan", plan));
                }
                if let Some(a) = &info.account {
                    rows.push(Row::new("Account", a));
                }
                // Nothing to configure: every turn brings them along.
                rows.push(Row::new(
                    "Tessera tools",
                    "notes, whiteboards, memory, notifications, the browser, machines — built in",
                ));
                self.t.rows(&rows);
                if info.api_key {
                    self.t
                        .hint("Claude Code uses an API key here: billed per token, not your plan.");
                }
                let b = self.b();
                let providers = self
                    .t
                    .spin("Loading the Claude models…", async move {
                        b.call_as::<Vec<ProviderInfo>>(Request::ListProviders { refresh: true })
                            .await
                    })
                    .await?
                    .unwrap_or_default();
                if !providers.iter().any(|p| p.id == CLAUDE_CODE && usable(p)) {
                    self.t
                        .ok("Claude Code is ready. Pick it for a chat with /model.");
                    return Ok(());
                }
                let make_default = !has_model
                    || self
                        .t
                        .confirm("Make a Claude model the default?", true)
                        .await?
                        .unwrap_or(false);
                if make_default {
                    self.choose_default(Some(CLAUDE_CODE)).await?;
                }
                return Ok(());
            }
            if !here {
                self.t.todo(&format!(
                    "{} Then run `tessera setup models` again.",
                    if info.hint.is_empty() {
                        "Claude Code needs to be installed and signed in on the server."
                    } else {
                        info.hint.as_str()
                    }
                ));
                self.t.rows(&[
                    Row::new("Install", CLAUDE_INSTALL).st(S_ACCENT),
                    Row::new("Sign in", "claude").st(S_ACCENT),
                ]);
                return Ok(());
            }
            if !info.installed {
                if offered_install {
                    self.t.err("Claude Code still isn't found.");
                    if !info.hint.is_empty() {
                        self.t.hint(&info.hint);
                    }
                    return Ok(());
                }
                offered_install = true;
                self.t
                    .todo("Claude Code isn't installed on this machine yet.");
                let choices = [
                    Choice::new("Install it now")
                        .tag("recommended")
                        .hint(format!("Runs Anthropic's installer: {CLAUDE_INSTALL}")),
                    Choice::new("I'll install it myself").hint(
                        "Then sign in once with `claude` and run `tessera setup models` again.",
                    ),
                ];
                if self.t.select("Install Claude Code?", &choices, 0).await? != Some(0) {
                    return Ok(());
                }
                println!();
                #[cfg(not(windows))]
                let ok = std::process::Command::new("sh")
                    .args(["-c", CLAUDE_INSTALL])
                    .status()
                    .is_ok_and(|s| s.success());
                #[cfg(windows)]
                let ok = std::process::Command::new("powershell.exe")
                    .args([
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        CLAUDE_INSTALL,
                    ])
                    .status()
                    .is_ok_and(|s| s.success());
                println!();
                if !ok {
                    self.t
                        .err("The installer didn't finish; see its output above.");
                    return Ok(());
                }
                continue;
            }
            if offered_sign_in {
                self.t.warn(if info.hint.is_empty() {
                    "Claude Code isn't signed in yet. Run `claude` once and sign in with your Claude account."
                } else {
                    info.hint.as_str()
                });
                return Ok(());
            }
            offered_sign_in = true;
            self.t.todo(
                "Claude Code is installed. Sign in once with your Claude account so Tessera can use your plan.",
            );
            if service::running_as_root() {
                self.t.hint(
                    "Sign in as the user the Tessera server runs as (root here), so the server can use it.",
                );
            }
            let choices = [
                Choice::new("Sign in now").tag("recommended").hint(
                    "Opens Claude Code here. Follow its sign-in, then type /exit to come back.",
                ),
                Choice::new("Later").hint("Run `claude` once, then `tessera setup models` again."),
            ];
            if self
                .t
                .select("Sign in to Claude Code?", &choices, 0)
                .await?
                != Some(0)
            {
                return Ok(());
            }
            println!();
            let cmd = info.command.clone().unwrap_or_else(|| "claude".into());
            let _ = std::process::Command::new(cmd).status();
            println!();
        }
    }

    /// Save an API key: `preset` is an index into [`KEY_PRESETS`]; `None`
    /// asks which provider (or another OpenAI-compatible server).
    async fn add_key(&mut self, preset: Option<usize>, has_model: bool) -> Result<()> {
        let i = match preset {
            Some(i) => i,
            None => {
                let mut choices: Vec<Choice> = KEY_PRESETS
                    .iter()
                    .map(|(label, _, _, _, _, hint)| Choice::new(*label).hint(*hint))
                    .collect();
                choices.push(
                    Choice::new("Another server (OpenAI-compatible)")
                        .hint("A model server's address, or any hosted OpenAI-style API."),
                );
                let Some(i) = self.t.select("Which one?", &choices, 0).await? else {
                    return Ok(());
                };
                i
            }
        };
        let (id, name, kind, url, key_required) = if i < KEY_PRESETS.len() {
            let p = KEY_PRESETS[i];
            (p.1.to_string(), p.2.to_string(), p.3, p.4.to_string(), true)
        } else {
            let Some(url) = self
                .t
                .input(
                    "Server address (with /v1)",
                    "",
                    "http://192.168.1.20:8000/v1",
                    check_url,
                )
                .await?
            else {
                return Ok(());
            };
            let suggested = provider_id_from_url(&url);
            let Some(id) = self
                .t
                .input("A short name for it", &suggested, "", |s| {
                    if !s.is_empty()
                        && s.len() <= 32
                        && s.chars().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
                        })
                    {
                        Ok(())
                    } else {
                        Err("lowercase letters, digits and dashes".into())
                    }
                })
                .await?
            else {
                return Ok(());
            };
            (id.clone(), id, ProviderKind::OpenaiChat, url, false)
        };
        if i < KEY_PRESETS.len() {
            self.t.hint(KEY_PRESETS[i].5);
        }
        let prompt = if key_required {
            format!("{name} API key")
        } else {
            "API key (leave empty if it doesn't need one)".to_string()
        };
        let key = self.t.secret(&prompt).await?;
        if key.is_none() && key_required {
            return Ok(());
        }
        let b = self.b();
        let (id2, url2, name2) = (id.clone(), url.clone(), name.clone());
        let key_s = key.as_ref().map(|k| k.to_string());
        let r = self
            .t
            .spin("Checking the key and loading the model list…", async move {
                crate::add_provider(&b, id2, url2, kind, key_s, Some(name2)).await
            })
            .await?;
        drop(key);
        match r {
            Ok(info) if info.models.is_empty() => {
                self.t.warn(&format!(
                    "Saved {}, but it returned no models. Check the key (and the address), then add it again to replace it.",
                    info.name
                ));
            }
            Ok(info) => {
                self.t.ok(&format!(
                    "Saved {} with {}.",
                    info.name,
                    plural(info.models.len(), "model")
                ));
                let ask = if has_model {
                    self.t
                        .confirm(&format!("Make a {} model the default?", info.name), false)
                        .await?
                        .unwrap_or(false)
                } else {
                    true
                };
                if ask {
                    self.choose_default(Some(&info.id)).await?;
                }
            }
            Err(e) => self.t.err(&format!("Couldn't save it: {e}")),
        }
        Ok(())
    }

    async fn choose_default(&mut self, only: Option<&str>) -> Result<bool> {
        let providers: Vec<ProviderInfo> = self
            .call(Request::ListProviders { refresh: false })
            .await
            .unwrap_or_default();
        let current = self.status().await.and_then(|s| s.default_model);
        let mut refs = Vec::new();
        let mut choices = Vec::new();
        for p in providers.iter().filter(|p| usable(p)) {
            if only.is_some_and(|o| o != p.id) {
                continue;
            }
            for m in &p.models {
                let r = ModelRef {
                    provider: p.id.clone(),
                    model: m.id.clone(),
                };
                let mut hint = format!(
                    "{} · {} context",
                    p.name,
                    crate::tui::text::tokens(m.context_window)
                );
                if m.reasoning {
                    hint.push_str(" · reasoning");
                }
                if m.vision {
                    hint.push_str(" · images");
                }
                choices.push(Choice::new(r.to_string()).hint(hint).tag(
                    if current.as_ref() == Some(&r) {
                        "current"
                    } else {
                        ""
                    },
                ));
                refs.push(r);
            }
        }
        if refs.is_empty() {
            self.t.todo("No models to choose from yet.");
            return Ok(false);
        }
        let def = current
            .as_ref()
            .and_then(|c| refs.iter().position(|r| r == c))
            .unwrap_or(0);
        let filter = refs.len() > 9;
        let Some(i) = self
            .t
            .select_opts("Default model", &choices, def, filter)
            .await?
        else {
            return Ok(false);
        };
        let m = refs[i].clone();
        match crate::set_default_model(&self.backend, m.clone()).await {
            Ok(()) => {
                self.t.ok(&format!("New chats use {m}."));
                Ok(true)
            }
            Err(e) => {
                self.t.err(&e.to_string());
                Ok(false)
            }
        }
    }

    // ---- 3. web search ----

    async fn step_web(&mut self) -> Result<Flow> {
        let info: WebSearchInfo = match self.call(Request::GetWebSearch).await {
            Ok(i) => i,
            Err(e) => {
                self.t.err(&format!(
                    "Couldn't read the web search settings: {}",
                    e.message
                ));
                return Ok(Flow::Skipped);
            }
        };
        let searx = info.search == "searxng" && info.searxng_url.is_some();
        if searx {
            let url = info.searxng_url.clone().unwrap_or_default();
            let mut rows = vec![Row::new("Search", format!("SearXNG at {url}"))];
            if !info.header_names.is_empty() {
                rows.push(Row::new("Headers", info.header_names.join(", ")));
            }
            self.t.rows(&rows);
        } else {
            self.t
                .rows(&[Row::new("Search", "DuckDuckGo · works without any setup")]);
        }
        println!();
        let choices = [
            Choice::new("DuckDuckGo")
                .hint("Works out of the box.")
                .tag(if searx { "" } else { "current" }),
            Choice::new("SearXNG (self-hosted)")
                .hint("Your own private metasearch server; DuckDuckGo stays as a fallback.")
                .tag(if searx { "current" } else { "" }),
        ];
        let Some(i) = self
            .t
            .select(
                "How should the agent search the web?",
                &choices,
                usize::from(searx),
            )
            .await?
        else {
            return Ok(Flow::Skipped);
        };
        if i == 0 {
            if !searx {
                return Ok(Flow::Done);
            }
            let r: Result<WebSearchInfo, ApiError> = self
                .call(Request::SaveWebSearch {
                    search: "duckduckgo".into(),
                    searxng_url: None,
                    // The SearXNG credentials aren't needed any more.
                    headers: Some(BTreeMap::new()),
                })
                .await;
            match r {
                Ok(_) => self.t.ok("Web search: DuckDuckGo"),
                Err(e) => self.t.err(&e.message),
            }
            return Ok(Flow::Done);
        }
        let current = info.searxng_url.clone().unwrap_or_default();
        let Some(url) = self
            .t
            .input(
                "SearXNG address",
                &current,
                "https://search.example.com",
                check_url,
            )
            .await?
        else {
            return Ok(Flow::Skipped);
        };
        let url = url.trim().trim_end_matches('/').to_string();
        let same_host = url == current.trim_end_matches('/');
        let headers = if !info.header_names.is_empty() && same_host {
            let choices = [
                Choice::new("Keep them").tag("current"),
                Choice::new("Replace them"),
                Choice::new("Remove them"),
            ];
            self.t.line(&self.t.paint(
                &format!("Stored headers: {}", info.header_names.join(", ")),
                S_HINT,
            ));
            match self.t.select("Extra headers", &choices, 0).await? {
                Some(1) => Some(self.ask_headers().await?),
                Some(2) => Some(BTreeMap::new()),
                _ => None,
            }
        } else {
            let want = self
                .t
                .confirm(
                    "Does it need extra headers (for example Cloudflare Access)?",
                    false,
                )
                .await?
                .unwrap_or(false);
            if want {
                Some(self.ask_headers().await?)
            } else if !info.header_names.is_empty() {
                // Never send the old server's credentials to a new address.
                Some(BTreeMap::new())
            } else {
                None
            }
        };
        let b = self.b();
        let r = self
            .t
            .spin("Saving…", async move {
                b.call_as::<WebSearchInfo>(Request::SaveWebSearch {
                    search: "searxng".into(),
                    searxng_url: Some(url),
                    headers,
                })
                .await
            })
            .await?;
        match r {
            Ok(i) => {
                self.t.ok(&format!(
                    "Web search: SearXNG at {}{}",
                    i.searxng_url.unwrap_or_default(),
                    if i.header_names.is_empty() {
                        String::new()
                    } else {
                        format!(" with {}", i.header_names.join(", "))
                    }
                ));
                Ok(Flow::Done)
            }
            Err(e) => {
                self.t.err(&e.message);
                Ok(Flow::Skipped)
            }
        }
    }

    /// Header names and hidden values.
    async fn ask_headers(&mut self) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        loop {
            let Some(name) = self
                .t
                .input("Header name", "", "CF-Access-Client-Id", check_header_name)
                .await?
            else {
                break;
            };
            let Some(value) = self.t.secret(&format!("Value of {name}")).await? else {
                break;
            };
            out.insert(name, value.to_string());
            if !self
                .t
                .confirm("Add another header?", false)
                .await?
                .unwrap_or(false)
            {
                break;
            }
        }
        Ok(out)
    }

    // ---- 4. other devices ----

    async fn step_devices(&mut self) -> Result<Flow> {
        let configured = self.configured_bind();
        let remote = self.remote_status().await;
        let open = !is_loopback(&configured);
        let mut rows = Vec::new();
        if self.role == Role::Server {
            rows.push(if open {
                let lan = tessera_core::util::lan_urls(&with_host(&configured, "0.0.0.0"))
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| with_host(&configured, "0.0.0.0"));
                Row::new("At home", format!("{lan} · your devices on this network"))
            } else {
                Row::new("At home", "only this computer").missing()
            });
        }
        let urls = remote.as_ref().map(|r| r.urls.clone()).unwrap_or_default();
        let tunnel = remote.as_ref().and_then(|r| r.tunnel.clone());
        for (k, u) in urls.iter().enumerate() {
            let label = if k == 0 { "Anywhere" } else { "" };
            let row = match &tunnel {
                Some(t) if *u == format!("https://{}", t.hostname) => {
                    if t.connected {
                        Row::new(label, format!("{u} · connected")).st(S_OK)
                    } else {
                        Row::new(label, format!("{u} · not connected")).st(S_WARN)
                    }
                }
                _ => Row::new(label, u.as_str()),
            };
            rows.push(row);
        }
        if urls.is_empty() {
            rows.push(Row::new("Anywhere", "not set up").missing());
        }
        self.t.rows(&rows);
        if let Some(t) = &tunnel
            && !t.connected
            && t.error.is_some()
        {
            println!();
            self.tunnel_problem(t);
        }
        println!();
        if self.role != Role::Server {
            self.t.hint(
                "The home network setting belongs to the server (`tessera setup devices` there).",
            );
            println!();
            let any = self.remote_access(urls, tunnel).await?;
            return Ok(if any { Flow::Done } else { Flow::Skipped });
        }
        let anywhere = !urls.is_empty() || tunnel.is_some();
        if anywhere {
            // Set up before: keep it, or change one part of it.
            let choices = [
                Choice::new("Keep it as it is").tag("current"),
                Choice::new("Change how it's reached from anywhere")
                    .hint("Tailscale, a Cloudflare tunnel or your own address."),
                Choice::new(if open {
                    "Stop devices at home from connecting directly"
                } else {
                    "Let devices at home connect directly"
                })
                .hint("Over your home network (Wi-Fi), the fastest way at home."),
            ];
            return Ok(
                match self
                    .t
                    .select("Where will you use Tessera?", &choices, 0)
                    .await?
                {
                    Some(1) => {
                        self.remote_access(urls, tunnel).await?;
                        Flow::Done
                    }
                    Some(2) => {
                        self.set_lan(!open).await?;
                        Flow::Done
                    }
                    Some(_) => Flow::Done,
                    None => Flow::Skipped,
                },
            );
        }
        let choices = [
            Choice::new("At home")
                .tag(if open { "current" } else { "recommended" })
                .hint("This computer and your devices on the home Wi-Fi, end-to-end encrypted."),
            Choice::new("At home and away")
                .hint("Also away from home: Tailscale, a Cloudflare tunnel, or your own address."),
            Choice::new("Only on this computer")
                .tag(if open { "" } else { "current" })
                .hint("Nothing else can connect. You can change this any time."),
        ];
        let Some(i) = self
            .t
            .select("Where will you use Tessera?", &choices, 0)
            .await?
        else {
            return Ok(Flow::Skipped);
        };
        match i {
            0 => {
                self.set_lan(true).await?;
            }
            1 => {
                self.set_lan(true).await?;
                println!();
                self.remote_access(urls, tunnel).await?;
            }
            _ => {
                self.set_lan(false).await?;
            }
        }
        Ok(Flow::Done)
    }

    /// Let devices on the home network connect (listen on every address),
    /// or only this computer. Returns whether it was saved.
    async fn set_lan(&mut self, open: bool) -> Result<bool> {
        let configured = self.configured_bind();
        let want = with_host(&configured, if open { "0.0.0.0" } else { "127.0.0.1" });
        if want == configured && self.bind.is_none() {
            if open {
                self.t
                    .ok("Devices on your network can connect (with a one-time code).");
            } else {
                self.t.ok("Only this computer can connect.");
            }
            return Ok(true);
        }
        // Through the server, so a running one doesn't write its old value
        // back on its next save.
        let r: Result<SetupStatus, ApiError> = self
            .call(Request::SetServerBind { bind: want.clone() })
            .await;
        if let Err(e) = r {
            self.t
                .err(&format!("Couldn't save the setting: {}", e.message));
            return Ok(false);
        }
        self.bind = Some(want.clone());
        self.restart_needed = true;
        let later = if matches!(self.backend, Backend::Remote { .. }) {
            " once Tessera restarts"
        } else {
            ""
        };
        if open {
            self.t.ok(&format!(
                "Saved: devices on your network can connect{later} (listening on {want})."
            ));
        } else {
            self.t
                .ok(&format!("Saved: only this computer can connect{later}."));
        }
        Ok(true)
    }

    async fn remote_access(
        &mut self,
        urls: Vec<String>,
        tunnel: Option<TunnelStatus>,
    ) -> Result<bool> {
        let tunnel_url = tunnel.as_ref().map(|t| format!("https://{}", t.hostname));
        let others: Vec<String> = urls
            .iter()
            .filter(|u| Some(*u) != tunnel_url.as_ref())
            .cloned()
            .collect();
        let tailscale = if self.role == Role::Server {
            tailnet().await
        } else {
            None
        };
        #[derive(Clone, Copy, PartialEq)]
        enum A {
            Keep,
            Recheck,
            Cloudflare,
            Tailscale,
            Own,
            Guide,
            RemoveTunnel,
            Clear,
            NotNow,
        }
        let nothing = tunnel.is_none() && others.is_empty();
        let mut actions: Vec<(A, Choice)> = Vec::new();
        if !nothing {
            actions.push((A::Keep, Choice::new("Keep it as it is").tag("current")));
        }
        if tunnel.as_ref().is_some_and(|t| !t.connected) {
            actions.push((
                A::Recheck,
                Choice::new("Check the tunnel again")
                    .hint("Waits for it to connect, or says what's in the way."),
            ));
        }
        if tunnel.is_none() {
            actions.push((
                A::Cloudflare,
                Choice::new("Cloudflare Tunnel (automatic)")
                    .tag(if nothing && tailscale.is_none() {
                        "recommended"
                    } else {
                        ""
                    })
                    .hint("Free with a domain on Cloudflare; no open ports, works on mobile data."),
            ));
        }
        actions.push((
            A::Tailscale,
            Choice::new("Tailscale")
                .tag(if tailscale.is_some() {
                    "on this computer"
                } else {
                    ""
                })
                .hint(match &tailscale {
                    Some(ts) => format!(
                        "Free, no domain. This machine is {} on your tailnet.",
                        ts.dns
                    ),
                    None => {
                        "Free, no domain. Works on your devices that are signed in to Tailscale."
                            .into()
                    }
                }),
        ));
        actions.push((
            A::Own,
            Choice::new("My own reverse proxy or tunnel")
                .hint("An HTTPS address that already forwards to this server."),
        ));
        if tunnel.is_none() {
            actions.push((
                A::Guide,
                Choice::new("No domain? Compare the options")
                    .hint("A short guide to what each option needs."),
            ));
        }
        if tunnel.is_some() {
            actions.push((
                A::RemoveTunnel,
                Choice::new("Remove the Cloudflare tunnel")
                    .hint("Deletes the tunnel, its DNS record and Access app (needs a token)."),
            ));
        } else if !others.is_empty() {
            actions.push((A::Clear, Choice::new("Remove these addresses")));
        }
        if nothing {
            actions.push((A::NotNow, Choice::new("Not now")));
        }
        // A tunnel that isn't connected: checking it is the likely wish.
        let first = if tailscale.is_some() {
            A::Tailscale
        } else {
            A::Cloudflare
        };
        let def = [A::Recheck, A::Keep, first]
            .iter()
            .find_map(|want| actions.iter().position(|(a, _)| a == want))
            .unwrap_or(0);
        let choices: Vec<Choice> = actions.iter().map(|(_, c)| c.clone()).collect();
        loop {
            let Some(i) = self
                .t
                .select("Reach Tessera from anywhere?", &choices, def)
                .await?
            else {
                return Ok(!nothing);
            };
            return match actions[i].0 {
                A::Keep => Ok(true),
                A::NotNow => Ok(false),
                A::Guide => {
                    self.no_domain_guide();
                    continue;
                }
                A::Recheck => {
                    self.tunnel_check().await?;
                    Ok(true)
                }
                // Cancelled (or refused): back to the choices.
                A::Cloudflare | A::Tailscale | A::Own => {
                    let done = match actions[i].0 {
                        A::Cloudflare => self.cloudflare().await?,
                        A::Tailscale => self.tailscale(&urls, tailscale.as_ref()).await?,
                        _ => self.own_proxy(&urls).await?,
                    };
                    if !done {
                        println!();
                        continue;
                    }
                    Ok(true)
                }
                A::RemoveTunnel => self.cloudflare_remove().await,
                A::Clear => {
                    let r: Result<RemoteAccessInfo, ApiError> = self
                        .call(Request::SetRemoteAccess {
                            urls: Vec::new(),
                            headers: Some(BTreeMap::new()),
                            add_headers: None,
                            remove_headers: Vec::new(),
                        })
                        .await;
                    match r {
                        Ok(_) => self.t.ok("Removed the remote addresses."),
                        Err(e) => self.t.err(&e.message),
                    }
                    Ok(true)
                }
            };
        }
    }

    /// Remote access without a domain, in a few lines.
    fn no_domain_guide(&self) {
        println!();
        self.t.line(
            &self
                .t
                .paint("Remote access without a domain", S_TEXT.bold()),
        );
        self.t.rows(&[
            Row::new(
                "Tailscale",
                "Free, no domain. Install it here and on your phone (tailscale.com/download), sign in to both with one account, then choose Tailscale.",
            ),
            Row::new(
                "A domain",
                "From about $10 a year at Cloudflare: dash.cloudflare.com → Domain Registration → Register Domains. Then choose Cloudflare Tunnel; the rest is automatic.",
            ),
            Row::new(
                "Quick tunnels",
                "Not used: their address changes on every restart, so your phone would lose the server.",
            )
            .st(S_HINT),
        ]);
        self.t
            .hint("The full guide is docs/REMOTE_ACCESS.md in Tessera's source.");
        println!();
    }

    fn print_recipe(&self) {
        for l in token_recipe() {
            let line = if l.starts_with("     ") {
                let parts: Vec<&str> = l.trim().split('·').map(str::trim).collect();
                format!(
                    "       {}{}{}",
                    self.t
                        .paint(&format!("{:<9}", parts.first().unwrap_or(&"")), S_HINT),
                    self.t
                        .paint(&format!("{:<27}", parts.get(1).unwrap_or(&"")), S_TEXT),
                    self.t.paint(parts.get(2).unwrap_or(&""), S_ACCENT)
                )
            } else if let Some((n, rest)) = l.split_once("  ")
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
            {
                format!(
                    "  {}  {}",
                    self.t.paint(n, S_ACCENT),
                    self.t.paint(rest, S_TEXT)
                )
            } else {
                format!("  {}", self.t.paint(&l, S_TEXT))
            };
            self.t.line(&line);
        }
    }

    async fn cloudflare(&mut self) -> Result<bool> {
        println!();
        self.t.para(
            "Tessera creates everything from one API token: a tunnel (an outgoing connection, no open ports), an address like https://tessera.example.com, and a Cloudflare Access rule that only your paired devices pass. Chats stay end-to-end encrypted inside.",
            S_TEXT,
        );
        self.t.para(
            "You need a free Cloudflare account with a domain on it. No domain? Press esc and choose “No domain? Compare the options”.",
            S_HINT,
        );
        println!();
        self.t.line(&self.t.paint(
            "Create the API token (Tessera uses it once and doesn't keep it):",
            Sty::plain().bold(),
        ));
        self.print_recipe();
        println!();
        loop {
            let Some(token) = self.t.secret("Cloudflare API token").await? else {
                return Ok(false);
            };
            let b = self.b();
            let tk = token.to_string();
            let zones = self
                .t
                .spin("Checking the token…", async move {
                    b.call_as::<Vec<CloudflareZone>>(Request::CloudflareZones { token: tk })
                        .await
                })
                .await?;
            let zones = match zones {
                Ok(z) if z.is_empty() => {
                    self.t.err("This token can't see any domain. Check “Zone Resources: Include · Specific zone · your domain”, or add a domain to your Cloudflare account first.");
                    if !self.retry().await? {
                        return Ok(false);
                    }
                    continue;
                }
                Ok(z) => z,
                Err(e) => {
                    self.t
                        .err(&format!("Cloudflare didn't accept it: {}", e.message));
                    if !self.retry().await? {
                        return Ok(false);
                    }
                    continue;
                }
            };
            let zone = if zones.len() == 1 {
                self.t.ok(&format!("Domain: {}", zones[0].name));
                zones[0].clone()
            } else {
                let choices: Vec<Choice> = zones.iter().map(|z| Choice::new(&z.name)).collect();
                let Some(i) = self.t.select("Which domain?", &choices, 0).await? else {
                    return Ok(false);
                };
                zones[i].clone()
            };
            let zname = zone.name.clone();
            let Some(host) = self
                .t
                .input(
                    "Address for Tessera",
                    &format!("tessera.{}", zone.name),
                    "",
                    move |h| check_hostname(h, &zname).map(|_| ()),
                )
                .await?
            else {
                return Ok(false);
            };
            let host = check_hostname(&host, &zone.name).unwrap_or(host);
            let go = self
                .t
                .confirm(&format!("Create https://{host} for this server?"), true)
                .await?
                .unwrap_or(false);
            if !go {
                return Ok(false);
            }
            let req = Request::CloudflareSetup {
                token: token.to_string(),
                zone_id: zone.id.clone(),
                hostname: host.clone(),
            };
            drop(token);
            match self.run_with_progress(req, "cloudflare").await? {
                Ok(_) => {
                    self.tunnel_check().await?;
                    self.t.hint(
                        "Tessera didn't keep the API token, so you can delete it now (My Profile → API Tokens).",
                    );
                    if matches!(self.backend, Backend::Local(_)) {
                        self.t.hint(
                            "The tunnel runs whenever the Tessera server runs; the next step starts it in the background.",
                        );
                    } else if self.opts.only.is_some() {
                        // On its own this step ends by pairing a device
                        // (the full setup has a pairing step later).
                        println!();
                        self.step_pair().await?;
                    }
                    return Ok(true);
                }
                Err(e) => {
                    self.t.err(&format!("Setup stopped: {}", e.message));
                    if !self.retry().await? {
                        return Ok(false);
                    }
                }
            }
        }
    }

    /// Wait for the Cloudflare tunnel to connect and say how it went; while
    /// it isn't connected, offer to check again. Returns whether it is.
    async fn tunnel_check(&mut self) -> Result<bool> {
        loop {
            let b = self.b();
            let start = std::time::Instant::now();
            let seen = self
                .t
                .watch(
                    "Connecting the tunnel to Cloudflare…",
                    Duration::from_secs(1),
                    Duration::from_secs(45),
                    || {
                        let b = b.clone();
                        async move {
                            let s: RemoteStatus = b.call_as(Request::RemoteStatus).await.ok()?;
                            let t = s.tunnel?;
                            // Connected, or running and the server knows why
                            // not (after it has looked again: it checks
                            // every few seconds).
                            let settled = start.elapsed() >= Duration::from_secs(8);
                            (t.connected || (settled && t.running && t.error.is_some()))
                                .then_some(t)
                        }
                    },
                )
                .await?;
            let t = match seen {
                Some(t) => Some(t),
                None => self.remote_status().await.and_then(|s| s.tunnel),
            };
            let Some(t) = t else {
                self.t
                    .err("There is no Cloudflare tunnel any more; set it up again.");
                return Ok(false);
            };
            if t.connected {
                self.t.ok(&format!(
                    "Connected: https://{} works from anywhere.",
                    t.hostname
                ));
                return Ok(true);
            }
            self.tunnel_problem(&t);
            println!();
            let choices = [
                Choice::new("Check again").hint("After fixing it, or to see whether it connected."),
                Choice::new("Continue for now")
                    .hint("The tunnel keeps trying and connects by itself once the way is clear. `tessera setup devices` checks again."),
            ];
            if self.t.select("What now?", &choices, 0).await? != Some(0) {
                return Ok(false);
            }
        }
    }

    /// Why the tunnel isn't connected, and the fix.
    fn tunnel_problem(&self, t: &TunnelStatus) {
        match t.error.as_deref() {
            Some(e) if e.contains("7844") => {
                self.t.warn(&format!(
                    "https://{} can't connect: a firewall blocks Cloudflare's tunnel port.",
                    t.hostname
                ));
                self.t.rows(&[
                    Row::new("Allow out", "TCP and UDP port 7844"),
                    Row::new("To", "region1.v2.argotunnel.com, region2.v2.argotunnel.com"),
                    Row::new("With ufw", "sudo ufw allow out 7844").st(S_ACCENT),
                    Row::new(
                        "Otherwise",
                        "ask the network's admin, or use Tailscale, which finds its own way out",
                    )
                    .st(S_HINT),
                ]);
            }
            Some(e) => {
                self.t
                    .warn(&format!("https://{} isn't connected yet.", t.hostname));
                self.t.rows(&[Row::new("Why", e).st(S_HINT)]);
            }
            None if !t.running => self.t.todo(&format!(
                "The tunnel for https://{} starts with the Tessera server.",
                t.hostname
            )),
            None => self.t.todo(&format!(
                "https://{} is still connecting; it keeps trying in the background.",
                t.hostname
            )),
        }
    }

    async fn retry(&mut self) -> Result<bool> {
        let again = [Choice::new("Try again"), Choice::new("Go back")];
        Ok(self.t.select("What now?", &again, 0).await? == Some(0))
    }

    /// Run a long server task, drawing its `setup_progress` events live.
    async fn run_with_progress(
        &mut self,
        req: Request,
        task: &str,
    ) -> Result<Result<Value, ApiError>> {
        let mut events = self.backend.subscribe();
        let b = self.b();
        let mut call = tokio::spawn(async move { b.call(req).await });
        let mut steps = self.t.steps_start();
        self.t
            .steps_next(&mut steps, Some("Talking to Cloudflare".into()));
        enum Wake {
            Done(Result<Value, ApiError>),
            Ev(Box<Option<BackendEvent>>),
            Tick(Result<()>),
        }
        loop {
            let w = tokio::select! {
                r = &mut call => Wake::Done(r.unwrap_or_else(|e| Err(ApiError::new(ErrorCode::Internal, e.to_string())))),
                ev = events.recv() => Wake::Ev(Box::new(ev)),
                r = self.t.steps_tick(&mut steps) => Wake::Tick(r),
            };
            match w {
                Wake::Done(r) => {
                    match &r {
                        Ok(_) => self.t.steps_next(&mut steps, None),
                        Err(e) => self.t.steps_fail(&mut steps, &e.message),
                    }
                    return Ok(r);
                }
                Wake::Ev(ev) => match *ev {
                    Some(BackendEvent::Event(Event::SetupProgress {
                        task: t,
                        step,
                        done,
                        error,
                    })) if t == task => {
                        if error.is_some() || (done && step == "failed") {
                            // The call's error follows.
                        } else if done {
                            self.t.steps_next(&mut steps, None);
                        } else {
                            self.t.steps_next(&mut steps, Some(step));
                        }
                    }
                    None => {
                        // No events: just wait for the answer.
                        let r = self.t.spin("Working…", &mut call).await?;
                        let r = r.unwrap_or_else(|e| {
                            Err(ApiError::new(ErrorCode::Internal, e.to_string()))
                        });
                        return Ok(r);
                    }
                    _ => {}
                },
                Wake::Tick(r) => {
                    if let Err(e) = r {
                        self.t.steps_fail(
                            &mut steps,
                            "Stopped waiting; the server may still finish the setup.",
                        );
                        return Err(e);
                    }
                }
            }
        }
    }

    async fn cloudflare_remove(&mut self) -> Result<bool> {
        self.t.para(
            "Removing the tunnel needs a Cloudflare API token with the same permissions as when it was created:",
            S_TEXT,
        );
        self.print_recipe();
        let Some(token) = self.t.secret("Cloudflare API token").await? else {
            return Ok(false);
        };
        let b = self.b();
        let tk = token.to_string();
        drop(token);
        let r = self
            .t
            .spin("Removing the tunnel…", async move {
                b.call_as::<RemoteStatus>(Request::CloudflareRemove { token: tk })
                    .await
            })
            .await?;
        match r {
            Ok(_) => {
                self.t
                    .ok("Removed the Cloudflare tunnel. You can delete the API token now.");
                Ok(true)
            }
            Err(e) => {
                self.t.err(&e.message);
                Ok(false)
            }
        }
    }

    /// Add `url` to the remote addresses (with extra headers devices must
    /// send, if any), then check that it answers.
    async fn add_address(
        &mut self,
        current: &[String],
        url: String,
        headers: Option<BTreeMap<String, String>>,
    ) -> Result<bool> {
        let mut urls = current.to_vec();
        if !urls.contains(&url) {
            urls.push(url.clone());
        }
        // Added to the stored headers (one with the same name is updated);
        // the rest, e.g. a tunnel's Access token, stays.
        let r: Result<RemoteAccessInfo, ApiError> = self
            .call(Request::SetRemoteAccess {
                urls,
                headers: None,
                add_headers: headers.clone(),
                remove_headers: Vec::new(),
            })
            .await;
        if let Err(e) = r {
            self.t.err(&e.message);
            return Ok(false);
        }
        let headers = headers.unwrap_or_default();
        let target = url.clone();
        let probe = self
            .t
            .spin(&format!("Checking {url}…"), async move {
                tessera_client::probe(std::slice::from_ref(&target), &headers).await
            })
            .await?;
        match probe {
            Ok(_) => self.t.ok(&format!(
                "Saved: {url} answers, and new pairing codes include it."
            )),
            Err(e) => self.t.warn(&format!(
                "Saved {url}, but it doesn't answer from here yet ({}). New pairing codes include it either way.",
                crate::qr::why_unreachable(&e.to_string(), &url)
            )),
        }
        Ok(true)
    }

    async fn tailscale(&mut self, current: &[String], ts: Option<&Tailnet>) -> Result<bool> {
        let bind = self.configured_bind();
        let port = port_of(&bind);
        let open = !is_loopback(&bind);
        println!();
        match ts {
            Some(ts) => self.t.rows(&[Row::new(
                "This machine",
                match &ts.ip {
                    Some(ip) => format!("{} · {ip}", ts.dns),
                    None => ts.dns.clone(),
                },
            )]),
            None => self.t.para(
                "Install Tailscale on this machine and on your phone (tailscale.com/download) and sign in to both with the same account. `tailscale status` then shows this machine's name.",
                S_HINT,
            ),
        }
        if self.role == Role::Server && !open {
            let name = ts.map_or("<name>.ts.net", |t| t.dns.as_str());
            self.t.hint(&format!(
                "Tessera only listens on this machine. `tailscale serve --bg {port}` gives it https://{name} on your tailnet; or allow devices on your network above."
            ));
        }
        let suggested = match (ts, open) {
            (Some(ts), true) => format!("http://{}:{port}", ts.dns),
            (Some(ts), false) => format!("https://{}", ts.dns),
            (None, _) => String::new(),
        };
        let Some(u) = self
            .t
            .input(
                "Tailscale address",
                &suggested,
                "https://myserver.tail1234.ts.net",
                check_url,
            )
            .await?
        else {
            return Ok(false);
        };
        let u = u.trim().trim_end_matches('/').to_string();
        self.add_address(current, u, None).await
    }

    async fn own_proxy(&mut self, current: &[String]) -> Result<bool> {
        println!();
        self.t.para(
            "The HTTPS address your reverse proxy or tunnel serves Tessera on. Pairing codes offer it next to the home address.",
            S_HINT,
        );
        let Some(u) = self
            .t
            .input("Address", "", "https://tessera.example.com", check_url)
            .await?
        else {
            return Ok(false);
        };
        let u = u.trim().trim_end_matches('/').to_string();
        let headers = if self
            .t
            .confirm(
                "Does it need extra headers (e.g. Cloudflare Access)?",
                false,
            )
            .await?
            .unwrap_or(false)
        {
            Some(self.ask_headers().await?)
        } else {
            None
        };
        self.add_address(current, u, headers).await
    }

    // ---- 5. background service ----

    async fn step_service(&mut self) -> Result<Flow> {
        if self.role != Role::Server {
            self.t
                .hint("The background service runs on the server itself; nothing to do here.");
            return Ok(Flow::Skipped);
        }
        if !cfg!(any(target_os = "linux", windows)) {
            self.t.warn(&format!(
                "Running in the background isn't supported on {} yet. Start the server with `tessera serve` when you need it.",
                std::env::consts::OS
            ));
            return Ok(Flow::Skipped);
        }
        if cfg!(target_os = "linux") && !Path::new("/run/systemd/system").exists() {
            self.t.warn("This machine doesn't use systemd, so Tessera can't install a service. Run `tessera serve` with your init system instead.");
            return Ok(Flow::Skipped);
        }
        let installed = service::summary(service::DEFAULT_NAME);
        if let Some(sum) = &installed
            && sum.home != self.opts.home
        {
            // Another Tessera (a different data directory) owns that unit.
            self.t.warn(&format!(
                "A background service is installed for {}, not this data directory, so it's left alone. `tessera service install --name <name>` adds a second one.",
                sum.home
                    .as_ref()
                    .map(|h| h.display().to_string())
                    .unwrap_or_else(|| "the default data directory".into())
            ));
            return Ok(Flow::Skipped);
        }
        let configured = self.configured_bind();
        // The unit passes its address to `serve`: a changed setting needs
        // the unit rewritten, not just a restart.
        let stale = installed
            .as_ref()
            .and_then(|s| s.bind.clone())
            .filter(|b| *b != configured);
        #[derive(Clone, Copy, PartialEq)]
        enum A {
            Install,
            Update,
            Restart,
            Keep,
            Remove,
            NotNow,
        }
        let mut actions: Vec<(A, Choice)> = Vec::new();
        match &installed {
            Some(sum) => {
                let mut rows = vec![
                    Row::new(
                        "Service",
                        format!(
                            "{} · {}",
                            service::scope_label(sum.scope),
                            if sum.active { "running" } else { "not running" }
                        ),
                    )
                    .st(if sum.active { S_TEXT } else { S_WARN }),
                ];
                if let Some(b) = &sum.bind {
                    rows.push(Row::new("Listens on", b));
                }
                self.t.rows(&rows);
                println!();
                if let Some(old) = &stale {
                    actions.push((
                        A::Update,
                        Choice::new("Update it and restart")
                            .tag("recommended")
                            .hint(format!(
                                "It listens on {old}; your setting is now {configured}."
                            )),
                    ));
                } else if self.restart_needed || !sum.active {
                    actions.push((
                        A::Restart,
                        Choice::new(if sum.active {
                            "Restart it to apply your changes"
                        } else {
                            "Start it"
                        })
                        .tag("recommended"),
                    ));
                }
                actions.push((A::Keep, Choice::new("Keep it as it is")));
                actions.push((
                    A::Install,
                    Choice::new("Reinstall with the current settings")
                        .hint("Rewrites the service for this tessera executable and restarts it."),
                ));
                actions.push((
                    A::Remove,
                    Choice::new("Remove it").hint("Your data and settings stay."),
                ));
            }
            None => {
                self.t
                    .rows(&[Row::new("Background", "not running yet").missing()]);
                println!();
                actions.push((
                    A::Install,
                    Choice::new("Yes, keep it running")
                        .tag("recommended")
                        .hint(if cfg!(windows) {
                            "Starts now and whenever you sign in to Windows, without a window, and keeps this PC awake while it's plugged in. (A startup program; `tessera service status` shows it.)"
                        } else {
                            "Starts now and with this computer, and keeps working when you log out or close SSH. (A systemd service; `tessera service status` shows it.)"
                        }),
                ));
                actions.push((
                    A::NotNow,
                    Choice::new("Not now")
                        .hint("Tessera then only runs while `tessera serve` runs in a terminal."),
                ));
            }
        }
        let choices: Vec<Choice> = actions.iter().map(|(_, c)| c.clone()).collect();
        let Some(i) = self
            .t
            .select("Keep Tessera running in the background?", &choices, 0)
            .await?
        else {
            return Ok(Flow::Skipped);
        };
        match actions[i].0 {
            A::Keep => Ok(Flow::Done),
            A::NotNow => Ok(Flow::Skipped),
            A::Remove => {
                match service::uninstall(service::DEFAULT_NAME, false) {
                    Ok(()) => {}
                    Err(e) => self.t.err(&e.to_string()),
                }
                Ok(Flow::Skipped)
            }
            A::Restart => {
                let scope = installed
                    .as_ref()
                    .map(|s| s.scope)
                    .unwrap_or(service::Scope::User);
                self.apply_bind();
                if let Err(e) = service::restart(scope, service::DEFAULT_NAME) {
                    self.t.err(&e.to_string());
                    return Ok(Flow::Skipped);
                }
                self.restart_needed = false;
                self.after_start().await?;
                Ok(Flow::Done)
            }
            A::Update => {
                // As the same account as before.
                let user = installed
                    .and_then(|s| s.user)
                    .filter(|u| u != "root" && service::running_as_root());
                self.install_service(user).await
            }
            A::Install => {
                let mut user = None;
                if service::running_as_root() {
                    let choices = [
                        Choice::new("As root")
                            .tag("typical for a dedicated container or VM")
                            .hint("The agent can then do anything on this machine that you allow it to."),
                        Choice::new("As another user")
                            .hint("Safer on a shared machine. That user's Tessera settings are used."),
                    ];
                    match self
                        .t
                        .select("Run the server as which user?", &choices, 0)
                        .await?
                    {
                        Some(1) => {
                            let Some(u) = self
                                .t
                                .input("User name", "", "tessera", |u| {
                                    if service::user_exists(u) {
                                        Ok(())
                                    } else {
                                        Err(format!("there is no user `{u}` here"))
                                    }
                                })
                                .await?
                            else {
                                return Ok(Flow::Skipped);
                            };
                            user = Some(u);
                        }
                        Some(_) => {}
                        None => return Ok(Flow::Skipped),
                    }
                }
                self.install_service(user).await
            }
        }
    }

    /// Write the unit (with the current settings) and (re)start it.
    async fn install_service(&mut self, user: Option<String>) -> Result<Flow> {
        self.apply_bind();
        let mut opts = service::InstallOptions {
            bind: self.bind.clone(),
            lan: false,
            user,
            linger: service::Linger::Ask,
            dry_run: false,
            name: service::DEFAULT_NAME.into(),
            home: self.opts.home.clone(),
            replace: false,
        };
        // The in-process engine lets go of nothing we need; the service
        // opens the same data directory.
        let paths = self.paths.clone();
        let mut r = service::install(&paths, &opts, Some(&mut self.t)).await;
        if let Err(e) = &r
            && e.to_string().contains("--replace")
        {
            // A server started by hand holds the port.
            self.t.warn(&e.to_string());
            let stop = self
                .t
                .confirm("Stop that server and let the service take over?", true)
                .await?
                .unwrap_or(false);
            if stop {
                opts.replace = true;
                r = service::install(&paths, &opts, Some(&mut self.t)).await;
            } else {
                return Ok(Flow::Skipped);
            }
        }
        match r {
            Ok(_) => {
                self.restart_needed = false;
                self.after_start().await?;
                Ok(Flow::Done)
            }
            Err(e) if e.is::<term::Aborted>() => Err(e),
            Err(e) => {
                self.t.err(&e.to_string());
                Ok(Flow::Skipped)
            }
        }
    }

    /// Wait for the background server and switch to it.
    async fn after_start(&mut self) -> Result<()> {
        let paths = self.paths.clone();
        let up = self
            .t
            .spin("Waiting for the server…", async move {
                for _ in 0..60 {
                    if let Ok(text) = std::fs::read_to_string(crate::backend::lock_file(&paths))
                        && let Ok(lock) = serde_json::from_str::<crate::backend::ServerLock>(&text)
                        && tessera_client::probe(
                            std::slice::from_ref(&lock.url),
                            &Default::default(),
                        )
                        .await
                        .is_ok()
                    {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                false
            })
            .await?;
        if up {
            self.reopen().await?;
            let at = match &self.backend {
                Backend::Remote { label, .. } => {
                    label.trim_start_matches("local server ").to_string()
                }
                Backend::Local(_) => String::new(),
            };
            self.t
                .ok(&format!("Switched to the background server at {at}."));
        } else {
            self.t
                .warn("The server didn't answer yet; check `tessera service status`.");
        }
        Ok(())
    }

    // ---- 6. pairing ----

    async fn step_pair(&mut self) -> Result<Flow> {
        if matches!(self.backend, Backend::Local(_)) {
            self.t.todo("Pairing needs the Tessera server running. Start it in the background (the previous step) or with `tessera serve`, then run `tessera pair`.");
            return Ok(Flow::Skipped);
        }
        let devices: Vec<DeviceInfo> = self.call(Request::ListDevices).await.unwrap_or_default();
        let paired: Vec<&DeviceInfo> = devices
            .iter()
            .filter(|d| !d.current && !d.name.starts_with("Terminal (this machine)"))
            .collect();
        if paired.is_empty() {
            self.t.rows(&[Row::new("Paired", "none yet").missing()]);
        } else {
            let names: Vec<&str> = paired.iter().map(|d| d.name.as_str()).collect();
            self.t.rows(&[Row::new("Paired", names.join(", "))]);
        }
        println!();
        // Devices at home were just allowed, but the server still listens
        // only here: restart it (it's ours) so they can connect.
        if self.restart_needed
            && self.role == Role::Server
            && !is_loopback(&self.configured_bind())
            && let Some(sum) = service::summary(service::DEFAULT_NAME)
            && sum.active
            && sum.home == self.opts.home
            && self.status().await.is_some_and(|s| !s.lan)
        {
            let go = self
                .t
                .confirm(
                    "Tessera has to restart so devices at home can connect. Restart it now?",
                    true,
                )
                .await?
                .unwrap_or(false);
            if go {
                self.apply_bind();
                match service::restart(sum.scope, service::DEFAULT_NAME) {
                    Ok(()) => {
                        self.restart_needed = false;
                        self.after_start().await?;
                    }
                    Err(e) => self.t.err(&e.to_string()),
                }
                println!();
            }
        }
        if let Some(s) = self.status().await
            && !s.lan
            && !s.remote
        {
            self.t.todo(
                "Other devices can't reach this server yet: it only listens on this machine. Allow devices on your network or set up remote access (`tessera setup devices`), then run `tessera pair`.",
            );
            return Ok(if paired.is_empty() {
                Flow::Skipped
            } else {
                Flow::Done
            });
        }
        let choices = [
            Choice::new("Show the code to scan")
                .tag(if paired.is_empty() { "recommended" } else { "" })
                .hint("A QR code for your phone, or a link to paste on a computer."),
            Choice::new("Not now").hint("Run `tessera pair` whenever you want to add a device."),
        ];
        let def = usize::from(!paired.is_empty());
        let Some(0) = self
            .t
            .select("Connect your phone now?", &choices, def)
            .await?
        else {
            return Ok(if paired.is_empty() {
                Flow::Skipped
            } else {
                Flow::Done
            });
        };
        println!();
        match show_pairing(&mut self.t, &self.backend, true).await? {
            Some(name) => {
                self.t
                    .ok(&format!("Paired “{name}”. It shows the same server key."));
                Ok(Flow::Done)
            }
            None => Ok(if paired.is_empty() {
                Flow::Skipped
            } else {
                Flow::Done
            }),
        }
    }

    // ---- 7. security ----

    async fn step_security(&mut self) -> Result<Flow> {
        let status = self.status().await;
        let cfg = (self.role == Role::Server)
            .then(|| Config::load(&self.paths).ok())
            .flatten();
        let encrypted = match &self.backend {
            Backend::Remote { client, .. } => client.status().encrypted,
            Backend::Local(_) => true,
        };
        let e2ee_required = status.as_ref().is_none_or(|s| s.e2ee_required);
        let sandbox = status
            .as_ref()
            .map(|s| s.sandbox.clone())
            .unwrap_or_else(tessera_core::sandbox::describe);
        let sandbox_off = cfg
            .as_ref()
            .is_some_and(|c| c.security.sandbox == tessera_core::config::SandboxMode::Off);
        let yolo = status
            .as_ref()
            .is_some_and(|s| s.permission == PermissionMode::Auto);
        let redact = cfg
            .as_ref()
            .map(|c| c.security.redact_patterns)
            .unwrap_or(true);
        let sandbox_on = !sandbox_off && sandbox.starts_with("on");
        let mut rows: Vec<(bool, &str, String)> = vec![
            (
                e2ee_required && encrypted,
                "Encryption",
                if e2ee_required && encrypted {
                    "end to end: tunnels and proxies only see noise".into()
                } else {
                    "off for browsers on your network (security.require_e2ee = false)".into()
                },
            ),
            (
                sandbox_on,
                "Sandbox",
                if sandbox_off {
                    "off: the agent's commands could read Tessera's keys".into()
                } else if sandbox_on {
                    "on: commands can't read Tessera's keys, tokens or chats".into()
                } else if cfg.as_ref().is_some_and(|c| {
                    c.security.sandbox == tessera_core::config::SandboxMode::Strict
                }) {
                    format!(
                        "{sandbox}. Strict mode refuses commands until the full sandbox is available."
                    )
                } else {
                    format!(
                        "{sandbox}. Every command asks first. To run without the safe box, choose Off in Settings → Privacy & safety → What commands can reach (security.sandbox = \"off\" in config.toml). Commands would have your account's access, including Tessera's keys and chats."
                    )
                },
            ),
            (
                !yolo,
                "Approvals",
                if yolo {
                    "YOLO by default: nothing asks first".into()
                } else {
                    "risky commands and changes ask you first".into()
                },
            ),
            (
                redact,
                "Secrets",
                "hidden from models; tool output is scrubbed".into(),
            ),
            (
                true,
                "Devices",
                "one-time pairing codes; `tessera devices` lists them".into(),
            ),
        ];
        if let Some(s) = &status
            && !s.has_model
        {
            rows.push((
                false,
                "AI model",
                "none yet: run `tessera setup models`".into(),
            ));
        }
        let rows: Vec<Row> = rows
            .into_iter()
            .map(|(good, label, text)| {
                let mark = match (self.t.caps.depth, good) {
                    (term::Depth::None, true) => ("[+]", Sty::plain()),
                    (term::Depth::None, false) => ("[!]", Sty::plain()),
                    (_, true) => ("▆▆", S_OK),
                    (_, false) => ("▆▆", S_ERR),
                };
                Row::new(label, text)
                    .st(if good { S_TEXT } else { S_WARN })
                    .mark(mark.0, mark.1)
            })
            .collect();
        self.t.rows(&rows);
        println!();
        self.t.hint(if self.role == Role::Server {
            "Change these in the app (Settings → Privacy & safety) or in config.toml (`tessera doctor` shows where)."
        } else {
            "These are set on the server."
        });
        Ok(Flow::Done)
    }

    async fn finish(&mut self) -> Result<()> {
        self.apply_bind();
        if self.t.input_ended() {
            println!();
            self.t.warn("Input ended before the setup finished, so it isn't marked as done. Run `tessera setup` in a terminal to finish it.");
            return Ok(());
        }
        let completed = self.backend.call(Request::CompleteSetup).await.is_ok();
        let s = self.status().await;
        let remote = self.remote_status().await;
        let svc = self.own_service();
        let all = self.done.iter().all(|d| *d == Some(true));
        let steps: Vec<(&str, Mark)> = self
            .flow
            .iter()
            .zip(&self.done)
            .map(|(s, d)| {
                (
                    s.short(),
                    if *d == Some(true) {
                        Mark::Done
                    } else {
                        Mark::Skipped
                    },
                )
            })
            .collect();
        self.t.page(&Page {
            title: if all {
                "Tessera is ready"
            } else {
                "Tessera is set up"
            },
            sub: "Here's how it is set up. Run `tessera setup` again to change any of it.",
            steps: &steps,
        });
        let mut rows = Vec::new();
        let ok = |r: Row| r.mark("✓", S_OK);
        let todo = |r: Row| r.mark("○", S_FAINT).missing();
        let warn = |r: Row| r.mark("!", S_WARN).st(S_WARN);
        if let Some(s) = &s {
            rows.push(match s.default_model.as_ref().filter(|_| s.has_model) {
                Some(m) => ok(Row::new("AI", m.to_string())),
                None => warn(Row::new(
                    "AI",
                    "none yet: Tessera can't answer until it has one",
                )),
            });
        }
        // Where it can be reached from.
        let mut ways = Vec::new();
        if self.role == Role::Server {
            let bind = self.configured_bind();
            if !is_loopback(&bind) {
                ways.push(
                    tessera_core::util::lan_urls(&with_host(&bind, "0.0.0.0"))
                        .into_iter()
                        .next()
                        .map(|u| format!("at home ({u})"))
                        .unwrap_or_else(|| "at home".into()),
                );
            }
        }
        let mut tunnel_down = false;
        if let Some(r) = &remote
            && let Some(u) = r.urls.first()
        {
            match &r.tunnel {
                Some(t) if !t.connected => {
                    tunnel_down = true;
                    ways.push(format!(
                        "anywhere (https://{}, not connected yet)",
                        t.hostname
                    ));
                }
                _ => ways.push(format!("anywhere ({u})")),
            }
        }
        rows.push(if ways.is_empty() {
            todo(Row::new("Reachable", "only on this computer"))
        } else if tunnel_down {
            warn(Row::new("Reachable", ways.join(" · ")))
        } else {
            ok(Row::new("Reachable", ways.join(" · ")))
        });
        if self.role == Role::Server {
            rows.push(match &svc {
                Some(s) if s.active => ok(Row::new("Background", "running, also after a restart")),
                Some(_) => warn(Row::new("Background", "installed, not running")),
                None => todo(Row::new("Background", "not running")),
            });
        }
        if let Some(s) = &s {
            rows.push(if s.devices == 0 {
                todo(Row::new("Devices", "none paired yet"))
            } else {
                ok(Row::new("Devices", format!("{} paired", s.devices)))
            });
            let (safe, all_on) = self.safety(s);
            let r = Row::new("Safety", safe);
            rows.push(if all_on { ok(r) } else { warn(r) });
        }
        self.t.card(&rows);
        if self.restart_needed && matches!(self.backend, Backend::Remote { local: true, .. }) {
            println!();
            self.t
                .warn("Restart the Tessera server so it uses the new network setting.");
        }
        if !completed {
            self.t
                .hint("(This server is older and doesn't remember that setup was done.)");
        }
        println!();
        let cmds = [
            ("tessera", "chat in this terminal"),
            ("tessera pair", "connect another phone or computer"),
            (
                "tessera setup",
                "change any of this; also web search and security",
            ),
        ];
        self.t.commands(&cmds);
        if !self.opts.first_run {
            println!();
        }
        Ok(())
    }

    /// What keeps the user safe, in a few words, and whether it's all on.
    fn safety(&self, s: &SetupStatus) -> (String, bool) {
        let cfg = (self.role == Role::Server)
            .then(|| Config::load(&self.paths).ok())
            .flatten();
        let sandbox_off = cfg
            .as_ref()
            .is_some_and(|c| c.security.sandbox == tessera_core::config::SandboxMode::Off);
        let sandbox_on = !sandbox_off && s.sandbox.starts_with("on");
        let yolo = s.permission == PermissionMode::Auto;
        let mut parts = vec![
            if s.e2ee_required {
                "end-to-end encrypted"
            } else {
                "browsers on your network unencrypted"
            },
            if yolo {
                "YOLO: nothing asks first"
            } else {
                "asks before risky actions"
            },
            "secrets kept from the AI",
        ];
        parts.push(if sandbox_on {
            "commands sandboxed"
        } else if sandbox_off {
            "sandbox off"
        } else {
            "sandbox partial"
        });
        (parts.join(" · "), s.e2ee_required && !yolo && sandbox_on)
    }
}

/// Make a pairing code and show it (QR code, details, a private link to
/// paste), then wait for a device to pair. Returns the device's name.
async fn show_pairing(t: &mut Term, backend: &Backend, qr: bool) -> Result<Option<String>> {
    if matches!(backend, Backend::Local(_)) {
        t.todo("Pairing needs the Tessera server running: start it with `tessera service install` or `tessera serve`, then run `tessera pair`.");
        return Ok(None);
    }
    // A tunnel that is still starting: give it a moment, so the address in
    // the code works away from home.
    let starting = backend
        .call_as::<RemoteStatus>(Request::RemoteStatus)
        .await
        .ok()
        .and_then(|s| s.tunnel)
        .is_some_and(|t| t.running && !t.connected && t.error.is_none());
    if starting {
        let b = backend.clone();
        t.watch(
            "Waiting for the Cloudflare tunnel to connect…",
            Duration::from_secs(1),
            Duration::from_secs(25),
            || {
                let b = b.clone();
                async move {
                    let s: RemoteStatus = b.call_as(Request::RemoteStatus).await.ok()?;
                    s.tunnel
                        .filter(|t| t.connected || t.error.is_some())
                        .map(|_| ())
                }
            },
        )
        .await?;
    }
    let known: std::collections::HashSet<String> = backend
        .call_as::<Vec<DeviceInfo>>(Request::ListDevices)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.id)
        .collect();
    let pc: PairingCode = backend
        .call_as(Request::CreatePairing)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let tunnel = backend
        .call_as::<RemoteStatus>(Request::RemoteStatus)
        .await
        .ok()
        .and_then(|s| s.tunnel);
    let link = pc.link.as_deref().and_then(|l| PairLink::parse(l).ok());
    let Some(raw) = pc.link.clone().filter(|_| link.is_some()) else {
        t.warn("Other devices can't reach this server yet: it only listens on this machine and has no remote address. Allow network or remote access first: `tessera setup devices`.");
        return Ok(None);
    };

    // The details: code, key, addresses (and whether they work), headers.
    let mut rows = vec![Row::new("Code", &pc.code).st(S_ACCENT.bold())];
    if let Some(fp) = &pc.fingerprint {
        rows.push(Row::new("Server key", fp));
    }
    if let Some(link) = &link {
        let tunnel_url = tunnel.as_ref().map(|t| format!("https://{}", t.hostname));
        for (k, u) in link.urls.iter().enumerate() {
            let label = if k == 0 { "Addresses" } else { "" };
            rows.push(match &tunnel {
                Some(tn) if tunnel_url.as_deref() == Some(u.trim_end_matches('/')) => {
                    if tn.connected {
                        Row::new(label, format!("{u} · connected")).st(S_OK)
                    } else {
                        Row::new(label, format!("{u} · not connected")).st(S_WARN)
                    }
                }
                _ => Row::new(label, u.as_str()),
            });
        }
        if link
            .headers
            .iter()
            .any(|(k, _)| k.starts_with("CF-Access-"))
        {
            rows.push(Row::new("Access", "Cloudflare Access token, in the code").st(S_HINT));
        } else if !link.headers.is_empty() {
            let names: Vec<&str> = link.headers.iter().map(|(k, _)| k.as_str()).collect();
            rows.push(Row::new("Headers", format!("{}, in the code", names.join(", "))).st(S_HINT));
        }
    }
    rows.push(Row::new("Valid", "10 minutes, for one device").st(S_HINT));

    let code_rows = match (&pc.link, qr) {
        (Some(l), true) => crate::qr::render(l, t.caps.depth, term::is_light()),
        _ => None,
    };
    let (cols, height) = crossterm::terminal::size()
        .map(|(w, h)| (w as usize, h as usize))
        .unwrap_or((80, 24));
    let qr_w = code_rows
        .as_ref()
        .and_then(|r| r.first())
        .map(|r| term::width(r))
        .unwrap_or(0);
    // Side by side when there's room: the code left, details right.
    let lw = rows
        .iter()
        .map(|r| term::width(&r.label))
        .max()
        .unwrap_or(0)
        + 3;
    let right_w = rows
        .iter()
        .map(|r| lw + term::width(&r.value))
        .max()
        .unwrap_or(0)
        .max(36);
    let side = qr_w > 0 && cols > PAD.len() + qr_w + 3 + right_w;
    match &code_rows {
        Some(code) if side => {
            let mut right: Vec<String> = Vec::new();
            right.push(t.paint("Scan with the Tessera app", S_TEXT.bold()));
            right.push(t.paint("Connect to server → Scan QR code", S_HINT));
            right.push(String::new());
            for r in &rows {
                right.push(format!(
                    "{}{}",
                    t.paint(&format!("{:<lw$}", r.label), S_HINT),
                    t.paint(&r.value, r.st)
                ));
            }
            let top = 1;
            for (i, l) in code.iter().enumerate() {
                let r = i
                    .checked_sub(top)
                    .and_then(|k| right.get(k))
                    .cloned()
                    .unwrap_or_default();
                println!("{PAD}{l}   {r}");
            }
        }
        _ => {
            t.rows(&rows);
        }
    }
    if let Some(link) = &link {
        let notes = crate::qr::reach_notes(link, tunnel.as_ref()).await;
        if !notes.is_empty() {
            println!();
        }
        for n in notes {
            t.warn(&n);
        }
    }
    // The link, unwrapped so it copies in one piece.
    println!();
    t.item(
        "No camera?",
        S_TEXT.bold(),
        "In the app choose Connect to server → Paste link, or run `tessera connect '<link>'` on another computer.",
        S_HINT,
    );
    t.item(
        "Private:",
        S_WARN.bold(),
        "the link holds this server's access keys. Don't share it.",
        S_WARN,
    );
    println!("{PAD}{}", t.paint(&raw, S_LINK));
    if let Some(code) = &code_rows
        && !side
    {
        println!();
        t.line(&t.paint(
            "Scan with the Tessera app: Connect to server → Scan QR code",
            S_TEXT.bold(),
        ));
        for l in code {
            println!("{PAD}{l}");
        }
        if code.len() + 2 > height {
            t.hint("Make the window taller (or zoom out) so the whole code fits, then scan it.");
        }
    }
    println!();
    let b = backend.clone();
    t.wait_enter(
        "Waiting for your device…",
        Duration::from_secs(2),
        Some(raw.as_str()),
        || {
            let b = b.clone();
            let known = known.clone();
            async move {
                let list: Vec<DeviceInfo> = b.call_as(Request::ListDevices).await.ok()?;
                list.into_iter()
                    .find(|d| !known.contains(&d.id))
                    .map(|d| d.name)
            }
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_recipe_is_exact() {
        let r = token_recipe().join("\n");
        for (scope, perm, level) in TOKEN_PERMISSIONS {
            assert!(
                r.lines().any(|l| {
                    let parts: Vec<&str> = l.split('·').map(str::trim).collect();
                    parts == [scope, perm, level]
                }),
                "missing {scope} · {perm} · {level}\n{r}"
            );
        }
        assert!(r.contains("My Profile → API Tokens"));
        assert!(r.contains("Create Custom Token"));
        assert!(r.contains("Specific zone"));
        assert!(r.contains("TTL"));
        assert_eq!(
            TOKEN_PERMISSIONS
                .iter()
                .map(|p| format!("{}·{}·{}", p.0, p.1, p.2))
                .collect::<Vec<_>>(),
            vec![
                "Account·Cloudflare Tunnel·Edit",
                "Account·Access: Apps and Policies·Edit",
                "Account·Access: Service Tokens·Edit",
                "Zone·DNS·Edit",
                "Zone·Zone·Read",
            ]
        );
        // Fits 80 columns with the wizard's indent.
        assert!(token_recipe().iter().all(|l| term::width(l) + 4 <= 80));
    }

    #[test]
    fn hostnames() {
        assert_eq!(
            check_hostname("Tessera.Example.com", "example.com").unwrap(),
            "tessera.example.com"
        );
        assert_eq!(
            check_hostname("https://ai.home.example.com/", "example.com").unwrap(),
            "ai.home.example.com"
        );
        assert!(check_hostname("example.com", "example.com").is_ok());
        assert!(check_hostname("tessera.example.org", "example.com").is_err());
        assert!(check_hostname("evilexample.com", "example.com").is_err());
        assert!(check_hostname("-x.example.com", "example.com").is_err());
        assert!(check_hostname("a_b.example.com", "example.com").is_err());
        assert!(check_hostname("a..example.com", "example.com").is_err());
    }

    #[test]
    fn input_checks() {
        assert!(check_url("https://search.example.com").is_ok());
        assert!(check_url("http://192.168.1.10:7878").is_ok());
        assert!(check_url("search.example.com").is_err());
        assert!(check_url("https://").is_err());
        assert!(check_url("https://user@evil").is_err());
        assert!(check_header_name("CF-Access-Client-Id").is_ok());
        assert!(check_header_name("Bad Header").is_err());
        assert!(check_header_name("").is_err());
        assert!(check_code("1234-5678").is_ok());
        assert!(check_code("12345678").is_ok());
        assert!(check_code("1234-567").is_err());
        assert!(check_code("abcd-efgh").is_err());
    }

    #[test]
    fn provider_ids_and_binds() {
        assert_eq!(provider_id_from_url("http://gpu-box:8000/v1"), "gpu-box");
        assert_eq!(provider_id_from_url("https://api.together.xyz/v1"), "api");
        assert_eq!(provider_id_from_url("http://127.0.0.1:8080/v1"), "custom");
        assert_eq!(with_host("127.0.0.1:7900", "0.0.0.0"), "0.0.0.0:7900");
        assert_eq!(with_host("garbage", "0.0.0.0"), "0.0.0.0:7878");
        assert!(is_loopback("127.0.0.1:7878"));
        assert!(!is_loopback("0.0.0.0:7878"));
        // OpenRouter first (one key, hundreds of models); Anthropic's says where its keys are.
        assert_eq!(KEY_PRESETS[0].1, "openrouter");
        let anthropic = KEY_PRESETS.iter().find(|p| p.1 == "anthropic").unwrap();
        assert_eq!(anthropic.3, ProviderKind::Anthropic);
        assert!(anthropic.5.contains("console.anthropic.com"));
        assert!(KEY_PRESETS.iter().all(|p| p.4.starts_with("https://")));
    }

    #[test]
    fn steps_are_ordered() {
        use clap::ValueEnum;
        assert_eq!(Step::value_variants().len(), 7);
        assert_eq!(Step::Where.title(), "Where Tessera runs");
        // The guided setup: four steps, the AI first and pairing last (it
        // needs the server running, which the step before starts).
        assert_eq!(
            Step::FLOW,
            [Step::Models, Step::Devices, Step::Service, Step::Pair]
        );
        // `tessera setup <arg>` parses back to the same step.
        for &s in Step::value_variants() {
            let arg = s.to_possible_value().unwrap();
            assert_eq!(Step::from_str(arg.get_name(), false), Ok(s));
            // Plain words: titles and blurbs fit a narrow terminal, the
            // short names a segment of the step bar.
            assert!(term::width(s.title()) <= 24, "{}", s.title());
            assert!(term::width(s.blurb()) <= 80, "{}", s.blurb());
            assert!(term::width(s.short()) <= 14, "{}", s.short());
        }
    }

    #[test]
    fn found_servers_are_on_this_computer_or_the_home_network() {
        for u in [
            "http://127.0.0.1:1234/v1",
            "http://localhost:11434/v1",
            "http://192.168.1.42:11434/v1",
            "http://10.0.0.7:8080/v1",
            "http://172.20.1.2:8000/v1",
            "http://gpu-box.local:8080/v1",
            "http://[::1]:8080/v1",
        ] {
            assert!(is_found_server(u), "{u}");
        }
        for u in [
            "https://api.openai.com/v1",
            "https://openrouter.ai/api/v1",
            "http://203.0.113.5:8080/v1",
            "http://100.64.0.20:11434/v1",
            "",
        ] {
            assert!(!is_found_server(u), "{u}");
        }
    }

    #[test]
    fn tailscale_status_is_read() {
        let json = br#"{"BackendState":"Running","Self":{"DNSName":"my-server.tail1234.ts.net.","TailscaleIPs":["fd7a:115c::1","100.64.0.20"]}}"#;
        let t = parse_tailnet(json).unwrap();
        assert_eq!(t.dns, "my-server.tail1234.ts.net");
        assert_eq!(t.ip.as_deref(), Some("100.64.0.20"));
        let stopped = br#"{"BackendState":"NeedsLogin","Self":{"DNSName":""}}"#;
        assert!(parse_tailnet(stopped).is_none());
        assert!(parse_tailnet(b"not json").is_none());
    }
}
