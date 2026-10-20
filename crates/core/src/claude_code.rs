//! The user's own Claude Code as a chat's engine (provider `claude-code`).
//!
//! Other apps may not sign in with a Claude.ai account, but a user may run
//! Claude Code headless (`claude -p`) under their own sign-in, and that uses
//! their Pro or Max plan. So Tessera never signs in to Claude and never
//! reads Claude Code's credentials or settings: it runs the `claude` command
//! the user installed, in the chat's folder and inside the command sandbox,
//! and talks to it with the stream-json protocol of the official Agent SDK
//! (`--input-format stream-json`, permission prompts over stdio as
//! `can_use_tool` control requests, answered by Tessera's approvals).
//!
//! Tessera's own tools reach Claude Code through the Agent SDK's in-process
//! MCP server kind (`"type": "sdk"` in `--mcp-config`): Claude Code sends
//! each MCP message as an `mcp_message` control request on the turn's own
//! stdin/stdout, and the engine answers it with the same tool dispatch as
//! its own agent. No port, token or file carries that access: it is the
//! turn's pipe, held only by the `claude` process Tessera started, and it
//! ends with that process. Tessera's context (memory, a bot's persona,
//! a few lines on its tools) goes in with `--append-system-prompt-file`,
//! a file Tessera writes for the run and removes after it.
//!
//! This module finds and checks `claude`, and knows the command line and
//! the protocol's messages; the turn itself runs in `engine/claude_code.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tessera_proto::{
    AssistantPart, ClaudeCodeInfo, ContentPart, ModelInfo, PermissionMode, ProviderKind,
    ThinkingLevel, ThinkingOption, Usage,
};

use crate::config::{ModelConfig, ProviderConfig};
pub use crate::pricing::CLAUDE_CODE as PROVIDER;

/// Aliases Claude Code resolves to the latest model of each family; its own
/// list (with exact versions) replaces them after the first turn.
const ALIASES: [&str; 3] = ["opus", "sonnet", "haiku"];

/// The built-in provider entry (like the ChatGPT one). `base_url` is the
/// `claude` command: empty means "find it".
pub fn preset() -> ProviderConfig {
    ProviderConfig {
        id: PROVIDER.into(),
        name: "Claude (your subscription)".into(),
        kind: ProviderKind::ClaudeCode,
        base_url: String::new(),
        discover: true,
        models: ALIASES
            .iter()
            .map(|id| ModelConfig {
                id: (*id).into(),
                name: None,
                context_window: Some(context_window(id)),
                max_output: None,
                reasoning: None,
                vision: None,
                price: None,
                fast: None,
            })
            .collect(),
        headers: Default::default(),
        reasoning_replay: Default::default(),
        parallel: None,
    }
}

/// The context window Claude Code runs a model with (Anthropic's model
/// configuration docs): Fable, Sonnet 5+ and Opus 4.7+ have 1M natively,
/// and so do `[1m]` variants of older ones; the rest have 200K. The aliases
/// (`opus`, `sonnet`) follow the current models.
pub fn context_window(id: &str) -> u64 {
    const LONG: u64 = 1_000_000;
    const SHORT: u64 = 200_000;
    let id = id.trim().to_ascii_lowercase();
    if id.ends_with("[1m]") {
        return LONG;
    }
    let base = id.trim_start_matches("claude-");
    match base {
        "default" | "opus" | "sonnet" => return LONG,
        "haiku" => return SHORT,
        _ => {}
    }
    if base.starts_with("fable") {
        return LONG;
    }
    // family-major[-minor][-date]
    let mut parts = base.split('-');
    let family = parts.next().unwrap_or("");
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts
        .next()
        .filter(|p| p.len() <= 2)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    match family {
        "sonnet" if major >= 5 => LONG,
        "opus" if (major, minor) >= (4, 7) => LONG,
        _ => SHORT,
    }
}

/// The context window a result's `modelUsage` gives for `model` (its id, or an alias such as
/// `opus`): that model's own entry, never another model's.
pub fn reported_window(model_usage: &Value, model: &str) -> Option<u64> {
    let norm = |s: &str| {
        s.trim()
            .to_ascii_lowercase()
            .trim_end_matches("[1m]")
            .to_string()
    };
    let want = norm(model);
    let models = model_usage.as_object()?;
    let window = |u: &Value| u.get("contextWindow").and_then(Value::as_u64);
    models
        .iter()
        .find(|(k, _)| norm(k) == want)
        // An alias: the one entry of that family.
        .or_else(|| {
            let alias = want.trim_start_matches("claude-");
            let mut of = models.iter().filter(|(k, _)| norm(k).contains(alias));
            match (of.next(), of.next()) {
                (Some(one), None) if !alias.is_empty() => Some(one),
                _ => None,
            }
        })
        .and_then(|(_, u)| window(u))
}

/// A readable name for a model id before Claude Code has named it.
pub fn model_name(id: &str) -> String {
    match id {
        "opus" => "Opus (latest)".into(),
        "sonnet" => "Sonnet (latest)".into(),
        "haiku" => "Haiku (latest)".into(),
        "fable" => "Fable (latest)".into(),
        _ => id.into(),
    }
}

/// Reasoning levels: Claude Code's `--effort`, or its own setting (`Auto`).
pub fn thinking(model: &str) -> (Vec<ThinkingOption>, ThinkingLevel) {
    if model.contains("haiku") {
        return (Vec::new(), ThinkingLevel::Auto);
    }
    let levels = ["low", "medium", "high", "xhigh", "max"];
    (effort_options(&levels), ThinkingLevel::Auto)
}

fn effort_options(levels: &[&str]) -> Vec<ThinkingOption> {
    let mut out = vec![ThinkingOption {
        level: ThinkingLevel::Auto,
        label: "Default".into(),
        hint: "Claude Code's own effort setting".into(),
    }];
    for l in levels {
        let (level, label, hint) = match *l {
            "low" => (ThinkingLevel::Low, "Low", "Quick, light reasoning"),
            "medium" => (ThinkingLevel::Medium, "Medium", "Balanced speed and depth"),
            "high" => (ThinkingLevel::High, "High", "Thorough reasoning"),
            "xhigh" => (ThinkingLevel::XHigh, "Extra high", "Very thorough"),
            "max" => (ThinkingLevel::Max, "Max", "Maximum depth; slowest"),
            _ => continue,
        };
        out.push(ThinkingOption {
            level,
            label: label.into(),
            hint: hint.into(),
        });
    }
    out
}

/// `--effort` for a reasoning level (`None`: Claude Code decides).
pub fn effort(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Auto | ThinkingLevel::Off => None,
        ThinkingLevel::Minimal | ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium => Some("medium"),
        ThinkingLevel::High => Some("high"),
        ThinkingLevel::XHigh => Some("xhigh"),
        ThinkingLevel::Max => Some("max"),
    }
}

/// The models Claude Code offers to the account `provider`, from its
/// `initialize` reply (`default`, which follows the user's own Claude Code
/// setting, is left out).
pub fn models_from_init(reply: &Value, provider: &str) -> Vec<ModelInfo> {
    let Some(list) = reply.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|m| {
            let id = m.get("value").and_then(Value::as_str)?;
            if id.is_empty() || id == "default" {
                return None;
            }
            let name = m
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| model_name(id));
            let levels: Vec<&str> = m
                .get("supportedEffortLevels")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let thinking = if levels.is_empty() {
                Vec::new()
            } else {
                effort_options(&levels)
            };
            // Claude Code's own fast mode (Opus 5.5, 5, 4.8 as of 2.1.286).
            let fast = m.get("supportsFastMode").and_then(Value::as_bool) == Some(true);
            // `opus` resolves to the model it stands for today.
            let resolved = m
                .get("resolvedModel")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .unwrap_or(id);
            Some(ModelInfo {
                provider: provider.into(),
                id: id.into(),
                name,
                context_window: context_window(resolved),
                max_output: 64_000,
                reasoning: true,
                vision: true,
                thinking,
                default_thinking: ThinkingLevel::Auto,
                last_thinking: None,
                fast,
                cyber_access_programs: Vec::new(),
                small_mode: Default::default(),
                small: false,
                context_warning: None,
            })
        })
        .collect()
}

/// Fast mode for a `claude -p` run: Claude Code takes it only from the
/// run's `--settings` (its fast mode docs). It is billed to the account's
/// usage credits, not the plan.
pub fn fast_settings() -> String {
    json!({"fastMode": true}).to_string()
}

/// Why Claude Code says fast mode is off (`fast_mode_disabled_reason`), in
/// plain words; `None` when there is nothing the user can act on.
pub fn fast_off_reason(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "extra_usage_disabled" | "free" => {
            "Claude's fast mode is paid from usage credits, which aren't on for your account (claude.ai → Settings → Models → Usage and cost)."
        }
        "model_not_allowed" => "Claude's fast mode isn't allowed for this model on your account.",
        "not_first_party" => {
            "Claude's fast mode works only with a Claude account or an Anthropic API key, not this provider."
        }
        "disabled_by_env" => "Claude's fast mode is switched off on this computer.",
        "network_error" => "Claude Code couldn't check whether fast mode is available here.",
        _ => return None,
    })
}

// ---- accounts ------------------------------------------------------------------------

/// Which Claude account a `claude` run is for: the `claude` command, and for
/// any account but the usual one (`claude-code`, Claude Code's own `~/.claude`)
/// its own configuration folder, which Claude Code takes from the documented
/// `CLAUDE_CONFIG_DIR` ("useful for running multiple accounts side by side").
/// Each folder holds that account's sign-in, made by the user with Claude
/// Code's own login; Tessera only names the folder, and never reads or writes
/// anything in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub command: String,
    pub dir: Option<PathBuf>,
}

impl Account {
    /// The account `id` runs `command` as, with other accounts' folders in
    /// `root`. Only the accounts Tessera adds (`claude-code-<name>`) have a
    /// folder of their own; any other `claude_code` provider (the usual one,
    /// or one set up with another `claude` command) uses Claude Code's own.
    pub fn of(id: &str, command: &str, root: Option<&Path>) -> Self {
        let own = id
            .strip_prefix(PROVIDER)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|name| !name.is_empty());
        Self {
            command: command.trim().to_string(),
            dir: own.then(|| root.map(|r| r.join(id))).flatten(),
        }
    }

    /// What is known about it is kept under this name (the usual account's
    /// is its command alone, as before there were several).
    pub fn key(&self) -> String {
        match &self.dir {
            None => self.command.clone(),
            Some(d) => format!("{}\u{1f}{}", self.command, d.display()),
        }
    }

    /// Run `cmd` (`claude`) as this account. Tessera makes the account's
    /// folder (empty, only for this user) the first time; Claude Code fills
    /// it when the user signs in.
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        #[cfg(windows)]
        if let Some(bash) = crate::platform::shell::claude_git_bash(
            &|k| std::env::var(k).ok(),
            &|p| Path::new(p).is_file(),
            &|name| crate::platform::find_program(name).map(|p| p.display().to_string()),
        ) {
            cmd.env("CLAUDE_CODE_GIT_BASH_PATH", bash);
        }
        if let Some(d) = &self.dir {
            if !d.is_dir() {
                let _ = std::fs::create_dir_all(d);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
                }
            }
            cmd.env("CLAUDE_CONFIG_DIR", d);
        }
    }

    /// What the user runs in a terminal on the server to sign this account
    /// in (`None`: the usual account, plain `claude`).
    pub fn sign_in(&self) -> Option<String> {
        let d = self.dir.as_ref()?;
        let program = match self.command.as_str() {
            "" => "claude".to_string(),
            c => c.to_string(),
        };
        Some(if cfg!(windows) {
            format!("$env:CLAUDE_CONFIG_DIR=\"{}\"; {program}", d.display())
        } else {
            format!("CLAUDE_CONFIG_DIR='{}' {program}", d.display())
        })
    }
}

// ---- finding and checking `claude` ----------------------------------------------

/// What `claude` said about itself. Asked only when something needs it
/// (the status request, a refreshed model list, a turn), never at start-up:
/// the engine keeps the answer, also across restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub signed_in: bool,
    pub plan: Option<String>,
    pub account: Option<String>,
    /// Signed in, but billed per token (an API key, or a cloud provider).
    pub api_key: bool,
}

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd"]
    } else {
        &[""]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|p| executable(p))
    })
}

/// Where `command` is: a path as given, a bare name on `PATH`, and for the
/// default (`""` or `claude`) also where Claude Code's installers put it
/// (a service's `PATH` often lacks `~/.local/bin`).
pub fn locate(command: &str) -> Option<PathBuf> {
    let c = command.trim();
    if !c.is_empty() && c != "claude" {
        let p = crate::util::expand_home(c);
        return if p.components().count() > 1 {
            executable(&p).then_some(p)
        } else {
            on_path(c)
        };
    }
    on_path("claude").or_else(|| {
        let home = dirs::home_dir()?;
        // Windows: the native installer's `claude.exe`, npm's `claude.cmd`.
        let places: &[&str] = if cfg!(windows) {
            &[r".local\bin\claude.exe", r"AppData\Roaming\npm\claude.cmd"]
        } else {
            &[".local/bin/claude", ".claude/local/claude"]
        };
        places.iter().map(|r| home.join(r)).find(|p| executable(p))
    })
}

async fn run_quiet(path: &Path, args: &[&str], account: &Account) -> Option<(bool, String)> {
    let mut cmd = tokio::process::Command::new(path);
    crate::platform::no_window(&mut cmd);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::tools::bash::withhold_env(&mut cmd);
    account.apply(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .ok()?
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Ask `claude` for its version and the account's sign-in (`claude auth
/// status`; its credential files are never read).
pub async fn detect(account: &Account) -> Detection {
    let mut d = Detection {
        path: locate(&account.command),
        ..Default::default()
    };
    if let Some(path) = d.path.clone() {
        d.version = run_quiet(&path, &["--version"], account)
            .await
            .and_then(|(_, out)| out.split_whitespace().next().map(str::to_string));
        if let Some((ok, out)) = run_quiet(&path, &["auth", "status"], account).await {
            apply_auth_status(&mut d, ok, &out);
        }
    }
    d
}

/// Read `claude auth status` (JSON; older versions print text).
fn apply_auth_status(d: &mut Detection, ok: bool, out: &str) {
    let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
        let l = out.to_ascii_lowercase();
        d.signed_in = ok && !l.contains("not logged in") && !l.contains("not signed in");
        return;
    };
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    d.signed_in = v.get("loggedIn").and_then(Value::as_bool).unwrap_or(ok);
    d.account = s("email");
    d.plan = s("subscriptionType");
    let method = s("authMethod").unwrap_or_default();
    let first_party = s("apiProvider").is_none_or(|p| p == "firstParty");
    d.api_key = d.signed_in && !(method == "claude.ai" && first_party);
}

/// What the apps show about an account (`provider`); `host` names the
/// computer Tessera runs on (where the user has to sign in).
pub fn info(d: &Detection, host: &str, provider: &str, account: &Account) -> ClaudeCodeInfo {
    let sign_in = account.sign_in();
    let hint = if d.path.is_none() {
        format!(
            "Install Claude Code on {host}, the computer running Tessera (https://code.claude.com/docs/en/setup), then sign in by running `claude` once there."
        )
    } else if let (false, Some(run)) = (d.signed_in, &sign_in) {
        format!(
            "Sign this account in by running `{run}` once in a terminal on {host} (as the user Tessera runs as) and logging in with that Claude Pro or Max account. Then press Check again."
        )
    } else if !d.signed_in {
        format!(
            "Sign in by running `claude` once in a terminal on {host} (as the user Tessera runs as) and logging in with your Claude Pro or Max account. Then press Check again."
        )
    } else if let (true, Some(run)) = (d.api_key, &sign_in) {
        format!(
            "This account is signed in to Claude Code with an API key, so its use is billed per token; run `{run}`, then `/login`, to use a Pro or Max plan instead."
        )
    } else if d.api_key {
        "Claude Code here is signed in with an API key, so its use is billed per token; run `claude`, then `/login`, to use your Pro or Max plan instead.".into()
    } else if sign_in.is_some() {
        "Ready: chats can use this account's models, with Tessera's notes, whiteboards, memory and other tools built in.".into()
    } else {
        "Ready: chats can use a “Claude (your subscription)” model, with Tessera's notes, whiteboards, memory and other tools built in.".into()
    };
    ClaudeCodeInfo {
        installed: d.path.is_some(),
        command: d.path.as_ref().map(|p| p.display().to_string()),
        version: d.version.clone(),
        signed_in: d.signed_in,
        plan: d.plan.clone(),
        account: d.account.clone(),
        api_key: d.api_key,
        hint,
        provider: Some(provider.to_string()),
        sign_in,
    }
}

// ---- the command line -----------------------------------------------------------

/// Which Claude Code conversation a Tessera chat continues.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Link {
    /// Claude Code's session id (`--resume`).
    pub session: String,
    /// The Tessera session it belongs to (a fork starts its own).
    pub owner: String,
    /// The Claude account it is on (each keeps its own conversations).
    /// Empty: from before there were several, the usual account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// The folder it ran in (Claude Code keeps conversations by folder).
    #[serde(default)]
    pub dir: String,
    /// The last transcript entry it has seen and that entry's timestamp: if
    /// a rewind removed it, Claude Code's copy is ahead and isn't resumed.
    pub seq: u64,
    pub ts: u64,
    /// What it has reported so far, by model (see [`usage_since`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reported: BTreeMap<String, Reported>,
    /// Tessera's context as this conversation was first given it, sent
    /// unchanged on every resume: it is part of Claude Code's system prompt,
    /// so a memory saved mid-conversation would otherwise make each turn a
    /// full prompt-cache miss. Renewed when Claude Code compacts or a
    /// setting the context reflects changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<FrozenContext>,
    /// Ledger version confirmed by a correlated replay-user acceptance.
    /// Old `ledger_version` values were only sent, not confirmed: ignore them
    /// on reload so uncertain pre-fix delivery gets one full resend.
    #[serde(default, rename = "confirmed_ledger_version")]
    pub ledger_version: String,
}

impl Link {
    /// Whether it is a conversation of the account `provider`.
    pub fn on_account(&self, provider: &str) -> bool {
        match self.provider.as_str() {
            "" => provider == PROVIDER,
            p => p == provider,
        }
    }
}

/// See [`Link::context`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrozenContext {
    /// Tessera's tools offered with it (their names are in the text).
    pub tools: Vec<String>,
    pub text: String,
}

/// A model's running totals in a Claude Code conversation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
}

/// What one model used in a turn, and its API-equivalent cost.
pub struct Used {
    pub model: String,
    pub usage: Usage,
    pub cost: f64,
}

/// A result's `modelUsage` holds running totals for the conversation (they
/// carry on across `--resume`): what each model used since `before`, with
/// the API-equivalent cost Claude Code worked out, and the new totals. A
/// total smaller than before means it started counting again.
pub fn usage_since(
    model_usage: &serde_json::Map<String, Value>,
    before: &BTreeMap<String, Reported>,
) -> (Vec<Used>, BTreeMap<String, Reported>) {
    let mut totals = before.clone();
    let mut out = Vec::new();
    for (model, v) in model_usage {
        let u = usage(v);
        let now = Reported {
            input: u.input,
            output: u.output,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            cost: v.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
        };
        let prev = before
            .get(model)
            .filter(|p| {
                p.input <= now.input
                    && p.output <= now.output
                    && p.cache_read <= now.cache_read
                    && p.cache_write <= now.cache_write
            })
            .cloned()
            .unwrap_or_default();
        let delta = Usage {
            input: now.input - prev.input,
            output: now.output - prev.output,
            cache_read: now.cache_read - prev.cache_read,
            cache_write: now.cache_write - prev.cache_write,
            cost_usd: None,
        };
        if delta.input + delta.output + delta.cache_read + delta.cache_write > 0 {
            out.push(Used {
                model: model.clone(),
                usage: delta,
                cost: (now.cost - prev.cost).max(0.0),
            });
        }
        totals.insert(model.clone(), now);
    }
    (out, totals)
}

/// Claude Code's permission mode for a chat's: edits in its folder run
/// without asking in Tessera's `default` (and YOLO), everything asks in
/// `ask`, read-only chats plan. What it still asks about is answered by
/// Tessera (engine `claude_policy`), so its own checks are never bypassed.
pub fn set_permission_mode_line(request_id: &str, mode: PermissionMode) -> String {
    json!({"type": "control_request", "request_id": request_id,
        "request": {"subtype": "set_permission_mode", "mode": permission_mode(mode)}})
    .to_string()
}

pub fn permission_mode(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default | PermissionMode::Auto => "acceptEdits",
        PermissionMode::Ask => "default",
        PermissionMode::ReadOnly => "plan",
    }
}

pub struct Launch<'a> {
    pub model: &'a str,
    pub effort: Option<&'static str>,
    /// Claude Code's fast mode (only for models that offer it).
    pub fast: bool,
    pub mode: PermissionMode,
    pub resume: Option<&'a str>,
    /// Private chats: no history on disk, web only; optional chosen search bridge.
    pub private: bool,
    /// More folders it may read (the chat's uploads).
    pub add_dirs: Vec<PathBuf>,
    /// Tessera's tools, over the turn's own pipe ([`SERVER`]).
    pub bridge: bool,
    /// Tessera's context for the chat, appended to Claude Code's prompt.
    pub context: Option<&'a Path>,
    /// What Claude Code may use of its own (a bot's tool preset).
    pub limits: ToolLimits,
    /// A helper run by Tessera (the memory review): only Tessera's tools,
    /// nothing kept.
    pub helper: bool,
}

/// The Agent SDK's in-process server entry for `--mcp-config`.
pub fn mcp_config() -> String {
    json!({"mcpServers": {SERVER: {"type": "sdk", "name": SERVER, "timeout": BRIDGE_TIMEOUT_MS}}})
        .to_string()
}

pub fn args(l: &Launch) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--verbose",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--include-partial-messages",
        // Each message is echoed back when Claude Code takes it up: until it
        // has taken up everything sent, its input must stay open.
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        permission_mode(l.mode),
        "--model",
        l.model,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(e) = l.effort {
        a.extend(["--effort".into(), e.into()]);
    }
    if l.fast && !l.helper {
        a.extend(["--settings".into(), fast_settings()]);
    }
    if let Some(r) = l.resume {
        a.extend(["--resume".into(), r.into()]);
    }
    if l.private {
        // Only an explicitly selected provider enables a private bridge.
        let protected = l.limits.deny.contains(&"WebFetch");
        let search_bridge = l.bridge && l.limits.deny.contains(&"WebSearch");
        a.extend(private_controls());
        a.extend([
            "--no-session-persistence".into(),
            "--tools".into(),
            if protected {
                ""
            } else if search_bridge {
                "WebFetch"
            } else {
                "WebSearch,WebFetch"
            }
            .into(),
            "--strict-mcp-config".into(),
        ]);
        if search_bridge {
            a.extend([
                "--mcp-config".into(),
                mcp_config(),
                "--allowedTools".into(),
                format!("mcp__{SERVER}"),
                "--disallowedTools".into(),
                if protected {
                    "WebSearch,WebFetch"
                } else {
                    "WebSearch"
                }
                .into(),
            ]);
        }
    } else {
        let mut allowed: Vec<String> = Vec::new();
        if l.bridge {
            // Asked once: Tessera's own approvals decide, when the call runs.
            a.extend(["--mcp-config".into(), mcp_config()]);
            allowed.push(format!("mcp__{SERVER}"));
        }
        if !l.helper {
            allowed.extend(l.limits.restore.iter().map(|t| t.to_string()));
        }
        if !allowed.is_empty() {
            a.extend(["--allowedTools".into(), allowed.join(",")]);
        }
        if let Some(f) = l.context {
            a.extend([
                "--append-system-prompt-file".into(),
                f.display().to_string(),
            ]);
        }
        if l.helper {
            a.extend([
                "--tools".into(),
                String::new(),
                "--no-session-persistence".into(),
            ]);
        } else {
            a.extend(["--tools".into(), l.limits.only.join(",")]);
            if !l.limits.deny.is_empty() {
                a.extend(["--disallowedTools".into(), l.limits.deny.join(",")]);
            }
        }
        // Ordinary chats keep the user's MCP servers/connectors. Limited
        // actors keep the old isolated configuration; the host hook also
        // checks every external call rather than trusting this launch flag.
        if l.helper || l.limits.own_mcp_off {
            a.push("--strict-mcp-config".into());
        }
    }
    for d in &l.add_dirs {
        a.extend(["--add-dir".into(), d.display().to_string()]);
    }
    a
}

/// What a private chat's `claude` leaves out (documented flags): the user's
/// and the project's customizations, i.e. CLAUDE.md, hooks, plugins, skills,
/// MCP servers and auto memory (`--safe-mode`, which unlike `--bare` keeps the
/// CLI's own sign-in), every settings file, and slash commands. Admin-managed
/// policy still applies (Anthropic's rule). A CLI too old for one of these
/// exits at start, before Tessera sends it any of the chat.
pub fn private_controls() -> [String; 4] {
    [
        "--safe-mode",
        "--setting-sources",
        "",
        "--disable-slash-commands",
    ]
    .map(String::from)
}

/// The same for a private chat's environment (documented variables).
pub const PRIVATE_ENV: [(&str, &str); 3] = [
    ("CLAUDE_CODE_SAFE_MODE", "1"),
    ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
];

/// Of the variables the user lets agent programs see (`agent.pass_env`), the
/// ones a private chat's `claude` gets: none that load code or settings into
/// it (Node/library preloads, Python paths, Claude Code's own switches). The
/// account's folder (`CLAUDE_CONFIG_DIR`) stays: it is which sign-in to use,
/// the same as in the user's other chats; so do the certificates Node trusts
/// (`NODE_EXTRA_CA_CERTS`, a file of certificates, behind a TLS proxy).
pub fn private_pass_env(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| {
            let up = n.to_ascii_uppercase();
            up == "NODE_EXTRA_CA_CERTS"
                || !(up.starts_with("CLAUDE_CODE_")
                    || up.starts_with("NODE_")
                    || up.starts_with("LD_")
                    || up.starts_with("DYLD_")
                    || up.starts_with("PYTHON")
                    || up.starts_with("NPM_CONFIG_")
                    || matches!(up.as_str(), "BASH_ENV" | "ENV"))
        })
        .cloned()
        .collect()
}

// ---- Tessera's tools in Claude Code (the bridge) -------------------------------

/// The MCP server name Claude Code knows Tessera's tools by.
pub const SERVER: &str = "tessera";
/// How Claude Code names a bridged tool (`mcp__tessera__note`).
pub const PREFIX: &str = "mcp__tessera__";
/// Longest one bridged call may take: an approval, or a question on a
/// whiteboard, waits for the user (Claude Code's own default is shorter).
const BRIDGE_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

/// The bridge also supplies redacted file reads and searches. Claude Code's
/// native tools cannot redact their results before its model reads them.
/// Tessera's bash/job pair additionally supplies explicitly requested vault values.
pub fn bridged(name: &str) -> bool {
    match name {
        "note" | "whiteboard" | "browser" | "machines" | "desktop" | "memory" | "notify"
        | "share_file" | "docs" | "skill" | "skill_manage" | "recall" | "schedule"
        | "message_bot" | "settings" | "agent" | "agents" | "save_secret" | "lsp" | "task"
        | "read" | "write" | "edit" | "grep" | "glob" | "bash" | "job" | "web_fetch" => true,
        _ => name.starts_with("mcp__"),
    }
}

/// Obvious credential targets are refused before the unmodified CLI runs a
/// native tool. This is a text/path guard, not proof about arbitrary programs.
pub fn credential_target(tool: &str, input: &Value, cwd: &Path) -> bool {
    if !matches!(
        tool,
        "Read" | "Grep" | "Glob" | "Bash" | "PowerShell" | "NotebookEdit"
    ) {
        return false;
    }
    let looks_private = |text: &str| {
        let normalized = format!("/{}", text.replace('\\', "/").to_ascii_lowercase());
        let path = Path::new(&normalized);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        crate::tools::credential_file(path)
            || crate::tools::share::secret_file(path)
            || name == ".env"
            || name.starts_with(".env.")
            || name.starts_with(".env*")
            || name == "kubeconfig"
            || (!name.ends_with(".pub")
                && ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
                    .iter()
                    .any(|p| name.starts_with(p)))
            || name == "identity"
            || name.ends_with(".kubeconfig")
            || name == "application_default_credentials.json"
            || matches!(name, "auth.json" | "secrets.json" | ".ssh" | ".kube")
            || normalized.contains("/.aws/")
            || normalized.ends_with("/.aws")
            || normalized.contains("/.azure/")
            || normalized.ends_with("/.azure")
            || normalized.contains("/.config/gcloud/")
            || normalized.ends_with("/.config/gcloud")
            || normalized.contains("/.config/gh/")
    };
    if matches!(tool, "Bash" | "PowerShell") {
        return input["command"].as_str().is_some_and(|command| {
            command
                .split(|c: char| c.is_whitespace() || "\"';|&()<>=`".contains(c))
                .filter(|s| !s.is_empty())
                .any(|token| {
                    looks_private(token)
                        || looks_private(
                            &crate::tools::canonical(&cwd.join(crate::util::expand_home(token)))
                                .to_string_lossy(),
                        )
                })
        });
    }
    for key in ["file_path", "notebook_path", "path", "glob"] {
        if let Some(p) = input[key].as_str() {
            let path = cwd.join(crate::util::expand_home(p));
            if looks_private(p) || looks_private(&crate::tools::canonical(&path).to_string_lossy())
            {
                return true;
            }
        }
    }
    if tool == "Glob" && input["pattern"].as_str().is_some_and(looks_private) {
        return true;
    }
    // Recursive CLI grep cannot exclude all credential filenames reliably.
    // Tessera's search supplies redacted results through the bridge instead.
    tool == "Grep"
        && !input["path"]
            .as_str()
            .is_some_and(|p| cwd.join(p).is_file())
}
/// The plan a `TodoWrite` call's list stands for (`None`: not a list): each
/// to-do's text and where it stands, in its order.
pub fn todo_plan(args: &Value) -> Option<Vec<tessera_proto::PlanItem>> {
    use tessera_proto::{PlanItem, PlanStatus};
    let todos = args.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|t| {
                let text = t.get("content").and_then(Value::as_str)?.trim();
                (!text.is_empty()).then(|| PlanItem {
                    text: crate::util::one_line(text, 200),
                    status: match t.get("status").and_then(Value::as_str) {
                        Some("completed") => PlanStatus::Done,
                        Some("in_progress") => PlanStatus::InProgress,
                        _ => PlanStatus::Pending,
                    },
                })
            })
            .take(50)
            .collect(),
    )
}

/// One of the questions Claude Code asks with `AskUserQuestion`.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    /// The question, as shown.
    pub text: String,
    /// The question exactly as Claude Code wrote it: its answer goes under it.
    pub key: String,
    /// A short label for it ("Auth method").
    pub header: String,
    /// (label, what it means) of each option.
    pub options: Vec<(String, String)>,
    /// Several options may be picked.
    pub multi: bool,
}

/// The extra option every question has: the user's own answer, typed.
pub const OTHER: &str = "Something else…";

/// The questions of an `AskUserQuestion` call (at most 4, as Claude Code
/// asks; at most 11 options each, leaving room for `OTHER`).
pub fn questions(input: &Value) -> Vec<Question> {
    let text = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    input
        .get("questions")
        .and_then(Value::as_array)
        .map(|qs| {
            qs.iter()
                .filter(|q| !text(q, "question").is_empty())
                .take(4)
                .map(|q| Question {
                    text: crate::util::one_line(&text(q, "question"), 1500),
                    key: q
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    header: crate::util::one_line(&text(q, "header"), 60),
                    options: q
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|os| {
                            os.iter()
                                .filter(|o| !text(o, "label").is_empty())
                                .take(11)
                                .map(|o| {
                                    (
                                        crate::util::one_line(&text(o, "label"), 180),
                                        crate::util::one_line(&text(o, "description"), 400),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    multi: q.get("multiSelect").and_then(Value::as_bool) == Some(true),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The whiteboard Claude Code's questions show on: each question, its
/// options and what they mean, and the answers given so far.
pub fn questions_markdown(qs: &[Question], answers: &[String]) -> String {
    let mut md = String::new();
    for (i, q) in qs.iter().enumerate() {
        if i > 0 {
            md.push('\n');
        }
        if !q.header.is_empty() {
            md.push_str(&format!("**{}**\n\n", q.header));
        }
        md.push_str(&q.text);
        md.push_str(if q.multi { " *(pick any)*\n\n" } else { "\n\n" });
        for (label, about) in &q.options {
            if about.is_empty() {
                md.push_str(&format!("- {label}\n"));
            } else {
                md.push_str(&format!("- **{label}**: {about}\n"));
            }
        }
        if let Some(a) = answers.get(i) {
            md.push_str(&format!("\n→ {a}\n"));
        }
    }
    md
}

/// What Claude Code gets back for its questions: its input with `answers`
/// (each question's text → the label picked, several joined with ", ", or
/// what the user typed).
pub fn answered_input(input: &Value, qs: &[Question], answers: &[String]) -> Value {
    let mut out = input.clone();
    let map: serde_json::Map<String, Value> = qs
        .iter()
        .zip(answers)
        .map(|(q, a)| (q.key.clone(), Value::String(a.clone())))
        .collect();
    if let Some(o) = out.as_object_mut() {
        o.insert("answers".into(), Value::Object(map));
    }
    out
}

/// Claude Code's task tools (newer versions; `TodoWrite` before them).
pub fn is_task_tool(name: &str) -> bool {
    matches!(name, "TaskCreate" | "TaskUpdate" | "TaskGet" | "TaskList")
}

/// A successful CLI task call updates the authoritative plan. Empty ids are
/// steps made through Tessera's ledger; a matching new CLI task adopts one
/// instead of duplicating it. Returns its step until the result supplies its id.
pub fn task_call(
    plan: &mut Vec<tessera_proto::PlanItem>,
    ids: &mut Vec<String>,
    name: &str,
    args: &Value,
) -> (Option<usize>, bool) {
    use tessera_proto::{PlanItem, PlanStatus};
    let text = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| crate::util::one_line(t, 200))
    };
    let status = |s: &str| match s {
        "completed" => Some(PlanStatus::Done),
        "in_progress" => Some(PlanStatus::InProgress),
        "pending" => Some(PlanStatus::Pending),
        _ => None,
    };
    match name {
        "TaskCreate" => {
            // Models sometimes say `name` or `title`, and Claude Code takes
            // the task all the same (seen live with haiku, 2.1.286).
            let Some(subject) = text("subject")
                .or_else(|| text("name"))
                .or_else(|| text("title"))
            else {
                return (None, false);
            };
            ids.resize(plan.len(), String::new());
            if let Some(i) = plan
                .iter()
                .enumerate()
                .position(|(i, p)| ids[i].is_empty() && p.text == subject)
            {
                return (Some(i), false);
            }
            if plan.len() >= 50 {
                return (None, false);
            }
            plan.push(PlanItem {
                text: subject,
                status: PlanStatus::Pending,
            });
            ids.push(String::new());
            (Some(plan.len() - 1), true)
        }
        "TaskUpdate" => {
            let id = match args.get("taskId") {
                Some(Value::String(s)) => s.trim().to_string(),
                Some(Value::Number(n)) => n.to_string(),
                _ => return (None, false),
            };
            let Some(i) = ids
                .iter()
                .position(|x| !id.is_empty() && *x == id)
                .filter(|i| *i < plan.len())
            else {
                return (None, false);
            };
            let s = args
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if s == "deleted" {
                plan.remove(i);
                ids.remove(i);
                return (None, true);
            }
            let mut changed = false;
            if let Some(st) = status(s)
                && plan[i].status != st
            {
                plan[i].status = st;
                changed = true;
            }
            if let Some(subject) = text("subject")
                && plan[i].text != subject
            {
                plan[i].text = subject;
                changed = true;
            }
            (None, changed)
        }
        _ => (None, false),
    }
}

/// A `TaskCreate` result names the task of step `step`: its id (a task of
/// that id from before, an older list, gives way).
pub fn task_created(ids: &mut [String], plan_len: usize, step: usize, result: &Value) -> bool {
    let id = match result.pointer("/task/id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return false,
    };
    if step >= ids.len() || step >= plan_len {
        return false;
    }
    for (k, x) in ids.iter_mut().enumerate() {
        if k != step && *x == id {
            x.clear();
        }
    }
    ids[step] = id;
    true
}

/// Keep CLI ids attached to the same steps when `task` replaces/reorders a
/// plan. Status-only updates preserve bindings; removed steps lose theirs.
pub fn reconcile_tasks(
    old: &[tessera_proto::PlanItem],
    ids: &mut Vec<String>,
    new: &[tessera_proto::PlanItem],
) {
    let mut used = vec![false; old.len()];
    *ids = new
        .iter()
        .map(|p| {
            old.iter()
                .enumerate()
                .position(|(i, before)| !used[i] && before.text.trim() == p.text.trim())
                .map(|i| {
                    used[i] = true;
                    ids.get(i).cloned().unwrap_or_default()
                })
                .unwrap_or_default()
        })
        .collect();
}

/// Reconcile only successful CLI results, using the actual TaskCreate id,
/// never its ordinal. Replayed creates with a known id update the same step.
pub fn task_result(
    plan: &mut Vec<tessera_proto::PlanItem>,
    ids: &mut Vec<String>,
    name: &str,
    args: &Value,
    result: &Value,
) -> bool {
    if result.get("success").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if name == "TodoWrite" {
        let Some(new) = todo_plan(args) else {
            return false;
        };
        reconcile_tasks(plan, ids, &new);
        let changed = *plan != new;
        *plan = new;
        return changed;
    }
    if name == "TaskCreate" {
        let id = match result.pointer("/task/id") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return false,
        };
        if ids.contains(&id) {
            let mut update = args.clone();
            update["taskId"] = json!(id);
            return task_call(plan, ids, "TaskUpdate", &update).1;
        }
        let (step, changed) = task_call(plan, ids, name, args);
        return step.is_some_and(|step| task_created(ids, plan.len(), step, result)) || changed;
    }
    task_call(plan, ids, name, args).1
}

/// Claude Code's own subagent tool (`Task` in older versions).
pub fn is_agent(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

/// The Tessera tool a Claude Code tool name stands for
/// (`mcp__tessera__note` → `note`).
pub fn native_name(name: &str) -> Option<&str> {
    name.strip_prefix(PREFIX).filter(|n| !n.is_empty())
}

/// Only reported result facts become shared display details. Progress
/// messages and missing fields are not stdout, exit codes or durations.
pub fn shell_details(name: &str, result: &Value) -> Option<Value> {
    if !matches!(name, "Bash" | "PowerShell" | "BashOutput" | "TaskOutput") {
        return None;
    }
    let mut d = serde_json::Map::new();
    for (target, keys) in [
        ("exit_code", &["exit_code", "exitCode"][..]),
        ("duration_ms", &["duration_ms", "durationMs"][..]),
    ] {
        if let Some(v) = keys
            .iter()
            .find_map(|k| result.get(*k))
            .filter(|v| v.is_number())
        {
            d.insert(target.into(), v.clone());
        }
    }
    if result.get("interrupted").and_then(Value::as_bool) == Some(true) {
        d.insert("interrupted".into(), Value::Bool(true));
    }
    (!d.is_empty()).then_some(Value::Object(d))
}

/// A bridged tool is shown by its native execution name.
pub fn shown_name(name: &str) -> &str {
    native_name(name).unwrap_or(name)
}

/// What a bridged tool tells Claude Code about itself: Tessera's own
/// description, except where Claude Code has a tool of the same kind.
pub fn bridge_description(name: &str, native: &str) -> String {
    match name {
        "bash" => format!("{native} Prefer this to built-in Bash/PowerShell: Tessera permissions, kept jobs and redacted $VARIABLE credentials."),
        "agents" => "Manage Tessera helpers: status, stop, report, message, apply/discard, approve/decline. Start with `agent`. Same permissions, slots and worktrees. docs: subagents.".into(),
        "job" => "Background jobs started by Tessera's `bash` tool: `list`, `output` (new since last read), `wait` \
                  (up to `timeout` s), `kill`."
            .into(),
        _ => native.to_string(),
    }
}

/// What Claude Code may use of its own in a chat: everything, or what a
/// bot's tool preset allows (its Tessera tools are already filtered).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolLimits {
    /// Explicit built-in ceiling, including for ordinary chats. Unknown tools stay off.
    pub only: Vec<&'static str>,
    /// Claude Code tools it may not use (`--disallowedTools`).
    pub deny: Vec<&'static str>,
    /// Tools Claude Code leaves out by default that the preset needs back
    /// (`--allowedTools`): on Linux and macOS it searches with `find` and
    /// `grep` through Bash, so a preset without commands but with search
    /// gets its Glob and Grep tools again (naming one restores both).
    pub restore: Vec<&'static str>,
    /// None of the user's own MCP servers (`--strict-mcp-config`).
    pub own_mcp_off: bool,
}

impl ToolLimits {
    /// Native content tools return to the CLI before Tessera can scrub them.
    /// Keep its loop, helpers, planning and task UI, but use the MCP equivalents
    /// for file, command, network and integration results when secrets exist.
    pub fn protect_secrets(&mut self) {
        self.only.retain(|tool| !unredacted_tool(tool));
        self.restore.retain(|tool| !unredacted_tool(tool));
        for (tool, _) in BUILTINS {
            if unredacted_tool(tool) && !self.deny.contains(tool) {
                self.deny.push(tool);
            }
        }
        self.own_mcp_off = true;
    }
}

pub fn unredacted_tool(name: &str) -> bool {
    // Fail closed for tools introduced by later CLI releases. Helpers inherit
    // this control hook, so they cannot use a native content tool either.
    !matches!(
        name,
        "Agent"
            | "Task"
            | "TaskCreate"
            | "TaskGet"
            | "TaskList"
            | "TaskUpdate"
            | "TaskStop"
            | "StopTask"
            | "KillShell"
            | "TodoWrite"
            | "ToolSearch"
            | "AskUserQuestion"
            | "EnterPlanMode"
            | "ExitPlanMode"
            | "EnterWorktree"
            | "ExitWorktree"
            | "EnterWorktreeMode"
            | "Config"
            | "CronCreate"
            | "CronDelete"
            | "CronList"
    )
}

/// The full effective Tessera tool ceiling in Claude Code's terms.
/// `None` is used only by callers requesting the complete supported set.
pub fn limits_for(allowed: Option<&[String]>) -> ToolLimits {
    let has = |n: &str| allowed.is_none_or(|a| a.iter().any(|a| a == n));
    let mut only: Vec<_> = BUILTINS
        .iter()
        .filter(|(tool, native)| has(native) && (*tool != "Workflow" || has("bash")))
        .map(|(tool, _)| *tool)
        .collect();
    if !only.is_empty() {
        only.push("ToolSearch");
        only.push("AskUserQuestion");
        only.push("EnterPlanMode");
        only.push("ExitPlanMode");
    }
    ToolLimits {
        deny: BUILTINS
            .iter()
            .map(|(tool, _)| *tool)
            .filter(|tool| !only.contains(tool))
            .collect(),
        restore: only
            .iter()
            .copied()
            .filter(|t| matches!(*t, "Glob" | "Grep"))
            .collect(),
        only,
        own_mcp_off: allowed.is_some(),
    }
}

/// Local capabilities and hosted storage/delivery integrations from the
/// public tools-reference and SDK input reference. ToolSearch only discovers
/// tools: each discovered action must still pass the host hook. RemoteTrigger
/// and remote Agent launches are intentionally not capabilities of this host.
pub const BUILTINS: &[(&str, &str)] = &[
    ("Bash", "bash"),
    ("PowerShell", "bash"),
    ("Monitor", "bash"),
    ("Read", "read"),
    ("Write", "write"),
    ("Edit", "edit"),
    ("MultiEdit", "edit"),
    ("NotebookEdit", "edit"),
    ("Grep", "grep"),
    ("Glob", "glob"),
    ("WebFetch", "web_fetch"),
    ("WebSearch", "web_search"),
    ("Agent", "agent"),
    ("Task", "agent"),
    ("Workflow", "agents"),
    ("SendMessage", "agents"),
    ("ListAgents", "agents"),
    ("Skill", "read"),
    ("LSP", "read"),
    ("ScheduleWakeup", "schedule"),
    ("CronCreate", "schedule"),
    ("CronList", "schedule"),
    ("CronDelete", "schedule"),
    ("EnterWorktree", "write"),
    ("ExitWorktree", "write"),
    ("PushNotification", "notify"),
    ("ReportFindings", "task"),
    ("SendFeedback", "task"),
    ("SubagentHandback", "task"),
    ("EndConversation", "task"),
    ("ListMcpResourcesTool", "read"),
    ("ReadMcpResourceTool", "read"),
    ("ReadMcpResourceDirTool", "read"),
    ("RefreshMcpTools", "read"),
    ("WaitForMcpServers", "read"),
    ("Artifact", "share_file"),
    ("SendUserFile", "share_file"),
    ("ShareOnboardingGuide", "share_file"),
    ("Projects", "note"),
    ("ShowOnboardingRolePicker", "skill_manage"),
    ("TaskOutput", "job"),
    ("TaskStop", "job"),
    ("BashOutput", "job"),
    ("KillShell", "job"),
    ("TodoWrite", "task"),
    ("TaskCreate", "task"),
    ("TaskUpdate", "task"),
    ("TaskGet", "task"),
    ("TaskList", "task"),
];
/// What Claude Code asked of the bridge.
#[derive(Debug, PartialEq)]
pub enum Rpc {
    /// Answered at once (the handshake, the tool list…).
    Reply(Value),
    /// Run a tool; `call` is the id of the tool call it belongs to.
    Call {
        id: Value,
        name: String,
        arguments: Value,
        call: Option<String>,
    },
}

/// The answer to an MCP JSON-RPC message for the bridge offering `tools`.
pub fn mcp_request(msg: &Value, tools: &[crate::llm::ToolSpec]) -> Rpc {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Notifications get an empty result (the SDK's convention).
    if msg.get("id").is_none() {
        return Rpc::Reply(json!({"jsonrpc": "2.0", "result": {}}));
    }
    let result = match method {
        "initialize" => json!({
            "protocolVersion": msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": t.parameters,
            "annotations": {"readOnlyHint": matches!(t.name.as_str(), "docs" | "recall" | "skill")},
            // The everyday ones come loaded; the rest wait for Claude Code's
            // tool search, so they cost nothing until used.
            "_meta": {"anthropic/alwaysLoad": matches!(t.name.as_str(), "note" | "whiteboard" | "memory" | "notify")},
        })).collect::<Vec<_>>()}),
        "tools/call" => {
            let p = msg.get("params").unwrap_or(&Value::Null);
            return Rpc::Call {
                id,
                name: p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arguments: p
                    .get("arguments")
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
                call: p
                    .pointer("/_meta/claudecode~1toolUseId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
        }
        _ => {
            return Rpc::Reply(json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("`{method}` is not supported")},
            }));
        }
    };
    Rpc::Reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// A tool's result as an MCP `tools/call` reply.
pub fn mcp_result(id: &Value, content: &[ContentPart], is_error: bool) -> Value {
    let mut blocks: Vec<Value> = content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => json!({"type": "text", "text": text}),
            ContentPart::Image { mime, data } => {
                json!({"type": "image", "data": data, "mimeType": mime})
            }
        })
        .collect();
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": ""}));
    }
    json!({"jsonrpc": "2.0", "id": id, "result": {"content": blocks, "isError": is_error}})
}

/// The control response carrying the bridge's answer to `request_id`.
pub fn mcp_line(request_id: &str, message: Value) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": {"mcp_response": message}},
    })
    .to_string()
}

/// Tessera's answer to a control request (`v`, the whole line) it won't
/// serve now, in its own words and never as a Stop: a permission request is
/// declined with `why` and `interrupt: false` (an empty message or an error
/// would let Claude Code fall back to its own "STOP what you are doing"), a
/// `PreToolUse` hook denies with `why`, other hooks carry on, a bridged tool
/// call gets `why` as its error result, anything else an error.
pub fn decline_request(v: &Value, why: &str) -> String {
    let id = v
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match v.pointer("/request/subtype").and_then(Value::as_str) {
        Some("can_use_tool") => permission_line(id, false, &Value::Null, why),
        Some("hook_callback")
            if v.pointer("/request/input/hook_event_name")
                .and_then(Value::as_str)
                == Some("PreToolUse") =>
        {
            hook_line(
                id,
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": why}}),
            )
        }
        Some("hook_callback") => hook_line(id, json!({})),
        Some("mcp_message")
            if v.pointer("/request/message/method").and_then(Value::as_str)
                == Some("tools/call") =>
        {
            let rpc = v
                .pointer("/request/message/id")
                .cloned()
                .unwrap_or(Value::Null);
            mcp_line(id, mcp_result(&rpc, &[ContentPart::text(why)], true))
        }
        _ => refusal_line(id, why),
    }
}

/// A control request Tessera doesn't serve.
pub fn refusal_line(request_id: &str, why: &str) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": why},
    })
    .to_string()
}

/// What Tessera adds to Claude Code's system prompt for a chat: a few lines
/// on the tools it brings, then memory, a bot's persona and the like. Kept
/// short: it is sent with every request (cached).
pub fn context(tools: &[String], persona: Option<&str>, blocks: &[String]) -> String {
    let mut s = String::from(
        "# Tessera\nClaude Code is the engine of this Tessera chat. Bridged tools share its permissions and workspace.\n\
         Do what the user asks; tool approvals happen in Tessera, not prose.\n\
         Use task for the durable goal/constraints/plan/evidence. Versioned ledger updates replace older work state, including cleared goals; after compaction read task then recall for exact details.\n",
    );
    if !tools.is_empty() {
        s.push_str(&format!(
            "Bridged names start with {PREFIX}; descriptions give actions.\n"
        ));
        let mut hints = Vec::new();
        if tools.iter().any(|n| n == "bash") {
            hints.push(
                "Prefer bridged read/write/edit/grep/glob/bash for tagged files, paging and jobs.",
            );
        }
        if tools.iter().any(|n| n == "web_fetch") {
            hints.push("Use web_fetch for Tessera's reader.");
        }
        if tools.iter().any(|n| n == "memory") {
            hints.push("Only memory persists here; Claude auto-memory is off.");
        }
        if tools.iter().any(|n| n == "save_secret") {
            hints.push(
                "Use save_secret for secure entry; never put credentials in chat or arguments.",
            );
        }
        if tools.iter().any(|n| n == "agents") {
            hints.push(
                "Agents tools are independent Tessera helpers; CLI background agents are not.",
            );
        }
        if tools.iter().any(|n| n == "web_search") {
            hints
                .push("Use web_search instead of WebSearch for the user's chosen search provider.");
        }
        s.push_str(&hints.join("\n"));
    }
    if let Some(p) = persona {
        s.push_str("\n\n");
        s.push_str(p.trim());
    }
    for b in blocks.iter().filter(|b| !b.trim().is_empty()) {
        s.push_str("\n\n");
        s.push_str(b.trim());
    }
    s.push('\n');
    s
}

// ---- one-shot requests (titles, summaries) ---------------------------------------

/// The model Tessera's own small requests use when Claude Code is all there
/// is: the smallest, they are short.
pub const UTILITY_MODEL: &str = "haiku";

/// `claude -p` for one answer: no tools, no MCP servers, nothing kept; the
/// system prompt from `system` (a file Tessera wrote), the question on stdin.
pub fn one_shot_args(model: &str, system: &Path) -> Vec<String> {
    [
        "-p",
        "--output-format",
        "json",
        "--model",
        model,
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--system-prompt-file",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([system.display().to_string()])
    .collect()
}

/// A one-shot answer: its text, tokens, the API-equivalent cost Claude Code
/// reported and the concrete model; or what went wrong.
pub struct OneShot {
    pub text: String,
    pub usage: Usage,
    pub cost: Option<f64>,
    pub model: Option<String>,
}

pub fn one_shot_result(stdout: &str) -> Result<OneShot, String> {
    let v: Value = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("result"))
        .ok_or_else(|| "Claude Code gave no answer".to_string())?;
    let text = v
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(failure(&text).0);
    }
    Ok(OneShot {
        text,
        usage: usage(v.get("usage").unwrap_or(&Value::Null)),
        cost: v.get("total_cost_usd").and_then(Value::as_f64),
        model: v
            .get("modelUsage")
            .and_then(Value::as_object)
            .and_then(|m| m.keys().next().cloned()),
    })
}

// ---- protocol messages ----------------------------------------------------------

/// A user message line (stdin).
pub fn user_line(content: &[ContentPart]) -> String {
    let blocks: Vec<Value> = content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => json!({"type": "text", "text": text}),
            ContentPart::Image { mime, data } => json!({
                "type": "image",
                "source": {"type": "base64", "media_type": mime, "data": data},
            }),
        })
        .collect();
    json!({
        "type": "user",
        "session_id": "",
        "message": {"role": "user", "content": blocks},
        "parent_tool_use_id": null,
    })
    .to_string()
}

/// A control request line (`initialize`, `interrupt`).
pub fn control_line(request_id: &str, subtype: &str) -> String {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": subtype}})
        .to_string()
}

/// Supplemental user context, held until the next query without starting a
/// second turn. It is identified so replay does not count as user work.
pub fn context_line(text: &str) -> String {
    let mut v: Value =
        serde_json::from_str(&user_line(&[ContentPart::text(text)])).expect("user line");
    v["shouldQuery"] = json!(false);
    v["isSynthetic"] = json!(true);

    v.to_string()
}

pub fn context_usage_line(request_id: &str) -> String {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "get_context_usage"}}).to_string()
}

/// Usage reported by the documented full get_context_usage control. Only
/// counts and window are retained; no memory filenames or account data.
pub fn context_usage(reply: &Value) -> Option<(u64, u64)> {
    let tokens = reply.get("totalTokens")?.as_u64()?;
    let window = reply.get("maxTokens")?.as_u64()?;
    (window > 0).then_some((tokens, window))
}

/// Stop one of its tasks (a subagent or a background command) alone: the
/// Agent SDK's `stop_task` control request. Claude Code answers, reports the
/// task `killed` and then `stopped`, and carries on without it.
pub fn stop_task_line(request_id: &str, task_id: &str) -> String {
    json!({"type": "control_request", "request_id": request_id,
           "request": {"subtype": "stop_task", "task_id": task_id}})
    .to_string()
}

// ---- hooks: file snapshots for rewind, the verification note ---------------------

/// Claude Code's tools that change files. Tessera keeps a copy of a file
/// before one of them changes it, so rewinding a chat puts it back.
pub const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
/// Tessera's hook callbacks (ids it registers with `initialize`).
pub const HOOK_POLICY: &str = "tessera_policy";
pub const HOOK_STOP: &str = "tessera_stop";
pub const HOOK_CHECK: &str = "tessera_post_write";

pub fn is_edit_tool(name: &str) -> bool {
    EDIT_TOOLS.contains(&name)
}

/// The `initialize` control request with Tessera's hooks, as the Agent SDK
/// registers its hook functions: Claude Code calls them back over the same
/// pipe (`hook_callback`) and waits for the answer. A `PreToolUse` hook runs
/// before every file change, in every permission mode (also where no
/// permission prompt comes, as in `acceptEdits`), and `Stop` before Claude
/// ends a turn. Nothing goes to settings files: the hooks belong to this
/// one process.
pub fn initialize_line(request_id: &str, snapshots: bool, verify: bool) -> String {
    let mut hooks = serde_json::Map::new();
    // Authorization and post-approval snapshots share one ordered callback.
    // Different CLI hook callbacks may execute concurrently.
    let _ = verify;
    if snapshots {
        hooks.insert(
            "PostToolUse".into(),
            json!([{"matcher": EDIT_TOOLS.join("|"), "hookCallbackIds": [HOOK_CHECK]}]),
        );
    }
    hooks.insert(
        "PreToolUse".into(),
        json!([{"hookCallbackIds": [HOOK_POLICY], "timeout": 86400}]),
    );
    // Lifecycle observation is independent of the verification preference.
    hooks.insert("Stop".into(), json!([{"hookCallbackIds": [HOOK_STOP]}]));
    let mut request = json!({"subtype": "initialize"});
    if !hooks.is_empty() {
        request["hooks"] = Value::Object(hooks);
    }
    json!({"type": "control_request", "request_id": request_id, "request": request}).to_string()
}

/// The answer to a `hook_callback`: the hook's output (`{}`: carry on as
/// usual, nothing decided; Tessera's hooks never approve anything).
pub fn hook_line(request_id: &str, output: Value) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": output},
    })
    .to_string()
}

/// The file an edit tool's input names.
pub fn edit_path(input: &Value) -> Option<&str> {
    ["file_path", "notebook_path"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .filter(|p| !p.trim().is_empty())
}

/// The `Stop` hook's output that lets Claude go on with `note` (feedback,
/// not an error; Claude Code's hooks reference, "Stop decision control").
pub fn stop_output(note: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": note}})
}

/// The answer to a `can_use_tool` request.
pub fn permission_line(request_id: &str, allow: bool, input: &Value, message: &str) -> String {
    let response = if allow {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        // A refusal is not a Stop. Explicitly opt out of the CLI's default
        // interruption, and never leave it to invent a denial message.
        let message = if message.trim().is_empty() {
            "Not approved yet: this action has no approval. Carry on with other work, or ask in the chat."
        } else {
            message
        };
        json!({"behavior": "deny", "message": message, "interrupt": false})
    };
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
    .to_string()
}

/// An assistant message's content (empty, hidden reasoning is dropped).
pub fn assistant_parts(content: &Value) -> Vec<AssistantPart> {
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str)? {
            "text" => {
                let text = b.get("text").and_then(Value::as_str)?;
                (!text.is_empty()).then(|| AssistantPart::Text { text: text.into() })
            }
            "thinking" => {
                let text = b.get("thinking").and_then(Value::as_str)?;
                (!text.trim().is_empty()).then(|| AssistantPart::Thinking {
                    text: text.into(),
                    opaque: None,
                })
            }
            "tool_use" => Some(AssistantPart::ToolCall {
                id: b.get("id").and_then(Value::as_str)?.into(),
                name: b.get("name").and_then(Value::as_str)?.into(),
                arguments: b.get("input").cloned().unwrap_or_else(|| json!({})),
            }),
            _ => None,
        })
        .collect()
}

/// A tool result's content (a string or text and image blocks).
pub fn tool_result_parts(content: &Value) -> Vec<ContentPart> {
    match content {
        Value::String(s) => vec![ContentPart::text(s.clone())],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str)? {
                "text" => Some(ContentPart::text(b.get("text")?.as_str()?)),
                "image" => {
                    let src = b.get("source")?;
                    Some(ContentPart::Image {
                        mime: src.get("media_type")?.as_str()?.into(),
                        data: src.get("data")?.as_str()?.into(),
                    })
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A tool card's details for a file edit (`Edit`, `Write`…): its path and a
/// unified diff, from the structured result Claude Code reports.
pub fn edit_details(result: &Value, cwd: &Path) -> Option<Value> {
    let path = result.get("filePath").and_then(Value::as_str)?;
    let name = Path::new(path)
        .strip_prefix(cwd)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string());
    let hunks = result
        .get("structuredPatch")
        .and_then(Value::as_array)
        .filter(|h| !h.is_empty());
    let diff = match hunks {
        Some(hunks) => {
            let mut d = format!("--- a/{name}\n+++ b/{name}\n");
            for h in hunks {
                let n = |k: &str| h.get(k).and_then(Value::as_u64).unwrap_or(0);
                d.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    n("oldStart"),
                    n("oldLines"),
                    n("newStart"),
                    n("newLines")
                ));
                for l in h
                    .get("lines")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    d.push_str(l.as_str().unwrap_or_default());
                    d.push('\n');
                }
            }
            d
        }
        None if result.get("type").and_then(Value::as_str) == Some("create") => {
            let content = result.get("content").and_then(Value::as_str)?;
            crate::tools::files::diff_stats("", content, &name).2
        }
        None => return None,
    };
    Some(json!({"path": name, "diff": crate::util::truncate(&diff, 200_000)}))
}

/// Token counts from an API `usage` object (`input_tokens`…) or a
/// `modelUsage` entry (`inputTokens`…).
pub fn usage(v: &Value) -> Usage {
    let n = |a: &str, b: &str| {
        v.get(a)
            .or_else(|| v.get(b))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Usage {
        input: n("input_tokens", "inputTokens"),
        output: n("output_tokens", "outputTokens"),
        cache_read: n("cache_read_input_tokens", "cacheReadInputTokens"),
        cache_write: n("cache_creation_input_tokens", "cacheCreationInputTokens"),
        cost_usd: None,
    }
}

/// One line saying what a tool call does, for approvals and progress.
pub fn summary(input: &Value, cwd: &Path) -> String {
    for k in [
        "command",
        "file_path",
        "notebook_path",
        "url",
        "query",
        "pattern",
        "path",
        "description",
        "prompt",
    ] {
        if let Some(s) = input
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if k.ends_with("path") {
                let p = Path::new(s);
                if let Ok(rel) = p.strip_prefix(cwd) {
                    return rel.display().to_string();
                }
            }
            return s.to_string();
        }
    }
    let compact = input.to_string();
    if compact == "{}" {
        String::new()
    } else {
        crate::util::truncate(&compact, 300)
    }
}

/// A short plain description of a failed turn: Claude Code's own words,
/// with what to do when it isn't signed in or the plan's limit is reached.
pub fn failure(text: &str) -> (String, bool) {
    // `[ede_diagnostic] result_type=… stop_reason=…`: Claude Code's internal
    // note when its turn was interrupted (a Ctrl+C it got from outside, a
    // stopped service); nothing a person can read.
    let text = text
        .split("; ")
        .filter(|p| !p.trim_start().starts_with("[ede_diagnostic]"))
        .collect::<Vec<_>>()
        .join("; ");
    if text.trim().is_empty() {
        return (
            "Claude Code was interrupted before it finished this turn (stopped from outside Tessera). Send a message to continue.".into(),
            false,
        );
    }
    let text = text.as_str();
    let l = text.to_ascii_lowercase();
    let signed_out = !refreshing(text)
        && [
            "/login",
            "not logged in",
            "invalid api key",
            "oauth token",
            "authentication",
        ]
        .iter()
        .any(|k| l.contains(k));
    let mut msg = format!("Claude Code: {}", crate::util::truncate(text.trim(), 600));
    if signed_out {
        msg.push_str(" — sign in by running `claude` once in a terminal on the server.");
    } else if l.contains("limit") && !refreshing(text) {
        msg.push_str(" — your Claude plan's usage limit; it resets later (or pick another model).");
    }
    (msg, signed_out)
}

/// Why a run ended before its turn did, in plain words: how it ended, its own
/// last words (stderr, or plain text it printed instead of its JSON) and what
/// to do. `version` is the Claude Code Tessera found, if known.
pub fn unfinished(stderr: &str, stdout: &str, how: Option<&str>, version: Option<&str>) -> String {
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let l = detail.to_ascii_lowercase();
    // How its command line parser names a flag it doesn't know: a version
    // from before Tessera started using that flag.
    const UNKNOWN: [&str; 3] = ["unknown option", "unknown argument", "unrecognized option"];
    if let Some(line) = detail
        .lines()
        .find(|x| UNKNOWN.iter().any(|k| x.to_ascii_lowercase().contains(k)))
    {
        return format!(
            "This Claude Code{} is too old for Tessera: it doesn't know an option Tessera starts it with ({}). Update it by running `claude update` on the computer running Tessera, then send your message again.",
            version.map(|v| format!(" ({v})")).unwrap_or_default(),
            crate::util::truncate(line.trim(), 160)
        );
    }
    // Signed out, or a plan's limit: its words with what to do about it.
    if !detail.is_empty() {
        let (msg, signed_out) = failure(&detail);
        if signed_out || (l.contains("limit") && !refreshing(&detail)) {
            return msg;
        }
    }
    let why = match how {
        Some("signal 9") => {
            " (it was killed, signal 9: often a sign the computer ran out of memory)".to_string()
        }
        Some(h) if h.starts_with("signal ") => format!(" (it was stopped from outside, {h})"),
        Some(h) => format!(" ({h})"),
        None => String::new(),
    };
    let mut msg = format!("Claude Code stopped before finishing its turn{why}");
    if detail.is_empty() {
        msg.push('.');
    } else {
        msg.push_str(": ");
        msg.push_str(crate::util::truncate(&detail, 600).trim_end());
        if !msg.ends_with(['.', '!', '?']) {
            msg.push('.');
        }
    }
    msg.push_str(" Send a message to try again.");
    msg
}

/// A `system` `api_retry` message in plain words: Claude's API didn't answer
/// and Claude Code tries again (it says so only when it gives up otherwise).
pub fn api_retry(v: &Value) -> String {
    let n = |k: &str| v.get(k).and_then(Value::as_u64);
    let error = v.get("error").and_then(Value::as_str).unwrap_or_default();
    let why = match (error, n("error_status")) {
        ("rate_limit", _) | (_, Some(429)) => "too many requests right now".to_string(),
        ("overloaded", _) | (_, Some(529)) => "they are overloaded".to_string(),
        (_, Some(s)) if s >= 500 => format!("a server error, {s}"),
        ("", None) if v.get("no_response").is_some() => "no answer in time".to_string(),
        ("", None) => "no answer".to_string(),
        (e, Some(s)) => format!("{}, {s}", e.replace('_', " ")),
        (e, None) => e.replace('_', " "),
    };
    let attempt = match (n("attempt"), n("max_retries")) {
        (Some(a), Some(m)) => format!(" (attempt {a} of {m})"),
        _ => String::new(),
    };
    format!("Claude's servers didn't answer ({why}); Claude Code is trying again{attempt}.")
}

/// Claude Code couldn't renew its sign-in because another `claude` on the
/// computer was doing it at the same moment: it passes by itself.
pub fn refreshing(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    l.contains("another claude code process is refreshing")
        || l.contains("because another claude code process is refreshing")
}

/// Only the CLI's explicit success result confirms a finished turn.
/// Unknown/missing metadata is a notice, not a successful outcome.
pub fn result_confirmed(v: &Value) -> bool {
    v.get("subtype").and_then(Value::as_str) == Some("success")
        && v.get("is_error").and_then(Value::as_bool) == Some(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn result_requires_a_known_success_subtype_and_false_error_flag() {
        use serde_json::json;
        for result in [
            json!({}),
            json!({"subtype":"unknown", "is_error":false}),
            json!({"subtype":"success"}),
            json!({"subtype":"success", "is_error":"false"}),
            json!({"subtype":"error_during_execution", "is_error":false}),
            json!({"subtype":"success", "is_error":true}),
        ] {
            assert!(!super::result_confirmed(&result), "{result}");
        }
        assert!(super::result_confirmed(
            &json!({"subtype":"success", "is_error":false})
        ));
    }
    #[test]
    fn shell_presentation_uses_only_reported_facts() {
        use serde_json::json;
        assert!(super::shell_details("Bash", &json!({"stdout":"hello"})).is_none());
        assert!(super::shell_details("Read", &json!({"exitCode":0})).is_none());
        assert_eq!(
            super::shell_details(
                "Bash",
                &json!({"exitCode":1,"durationMs":42,"interrupted":true})
            ),
            Some(json!({"exit_code":1,"duration_ms":42,"interrupted":true}))
        );
        assert!(
            super::shell_details("PowerShell", &json!({"exitCode":"0","durationMs":"42"}))
                .is_none()
        );
    }

    #[test]
    fn a_run_that_ended_early_says_why() {
        use super::unfinished;
        let old = unfinished(
            "error: unknown option '--replay-user-messages'\n",
            "",
            Some("exit code 1"),
            Some("1.0.3"),
        );
        assert!(
            old.contains("too old") && old.contains("(1.0.3)") && old.contains("claude update"),
            "{old}"
        );
        assert!(old.contains("--replay-user-messages"), "{old}");
        let killed = unfinished("", "", Some("signal 9"), None);
        assert!(
            killed.contains("killed")
                && killed.contains("memory")
                && killed.ends_with("Send a message to try again."),
            "{killed}"
        );
        let plain = unfinished(
            "",
            "Error: the folder is read-only",
            Some("exit code 2"),
            None,
        );
        assert_eq!(
            plain,
            "Claude Code stopped before finishing its turn (exit code 2): Error: the folder is read-only. Send a message to try again."
        );
        let out = unfinished(
            "Not logged in · Please run /login",
            "",
            Some("exit code 1"),
            None,
        );
        assert!(out.contains("sign in"), "{out}");
        assert_eq!(
            unfinished("", "", None, None),
            "Claude Code stopped before finishing its turn. Send a message to try again."
        );
    }

    #[test]
    fn retries_read_plainly() {
        let r = super::api_retry(
            &serde_json::json!({"type": "system", "subtype": "api_retry", "attempt": 2,
            "max_retries": 10, "retry_delay_ms": 4000, "error_status": 529, "error": "overloaded"}),
        );
        assert_eq!(
            r,
            "Claude's servers didn't answer (they are overloaded); Claude Code is trying again (attempt 2 of 10)."
        );
        let r = super::api_retry(
            &serde_json::json!({"attempt": 1, "max_retries": 10, "error_status": null, "error": "unknown"}),
        );
        assert!(r.contains("(unknown)"), "{r}");
    }

    #[test]
    fn an_interrupted_turn_reads_plainly() {
        let (msg, signed_out) = super::failure(
            "[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use",
        );
        assert!(!signed_out);
        assert!(msg.starts_with("Claude Code was interrupted"), "{msg}");
        assert!(!msg.contains("ede_diagnostic"), "{msg}");
        // Real words stay, the diagnostic goes.
        let (msg, _) = super::failure("API Error: 500; [ede_diagnostic] result_type=user");
        assert_eq!(msg, "Claude Code: API Error: 500");
    }

    #[test]
    fn task_tools_keep_a_plan() {
        use tessera_proto::{PlanItem, PlanStatus};
        // A matching native step is adopted, not duplicated or discarded.
        let mut plan = vec![PlanItem {
            text: "Buy bread".into(),
            status: PlanStatus::Pending,
        }];
        let mut ids = Vec::new();
        let (step, changed) = task_call(
            &mut plan,
            &mut ids,
            "TaskCreate",
            &json!({"subject": "Buy bread"}),
        );
        assert_eq!((step, changed), (Some(0), false));
        assert_eq!(plan.len(), 1);
        assert!(task_created(
            &mut ids,
            plan.len(),
            0,
            &json!({"task": {"id": "1"}})
        ));
        // `name` instead of `subject` (a model's slip Claude Code accepts).
        let (step, _) = task_call(
            &mut plan,
            &mut ids,
            "TaskCreate",
            &json!({"name": "Pack plates"}),
        );
        assert_eq!(plan[1].text, "Pack plates");
        assert!(task_created(
            &mut ids,
            plan.len(),
            step.unwrap(),
            &json!({"task": {"id": 2}})
        ));
        assert_eq!(ids, ["1", "2"]);
        // Updates by id: status, subject, deleted; unknown ids and statuses change nothing.
        assert!(
            task_call(
                &mut plan,
                &mut ids,
                "TaskUpdate",
                &json!({"taskId": "2", "status": "in_progress"})
            )
            .1
        );
        assert_eq!(plan[1].status, PlanStatus::InProgress);
        assert!(
            !task_call(
                &mut plan,
                &mut ids,
                "TaskUpdate",
                &json!({"taskId": "2", "status": "in_progress"})
            )
            .1
        );
        assert!(
            !task_call(
                &mut plan,
                &mut ids,
                "TaskUpdate",
                &json!({"taskId": "9", "status": "completed"})
            )
            .1
        );
        assert!(
            !task_call(
                &mut plan,
                &mut ids,
                "TaskUpdate",
                &json!({"taskId": "1", "status": "someday"})
            )
            .1
        );
        assert!(
            task_call(
                &mut plan,
                &mut ids,
                "TaskUpdate",
                &json!({"taskId": 1, "subject": "Buy bread and cheese"})
            )
            .1
        );
        assert_eq!(plan[0].text, "Buy bread and cheese");
        assert!(
            task_call(
                &mut plan,
                &mut ids,
                "TaskUpdate",
                &json!({"taskId": "1", "status": "deleted"})
            )
            .1
        );
        assert_eq!(
            (plan.len(), ids.as_slice()),
            (1, ["2".to_string()].as_slice())
        );
        // An id seen again (a new task list) takes over from the old step.
        let (step, _) = task_call(
            &mut plan,
            &mut ids,
            "TaskCreate",
            &json!({"subject": "Pick a park"}),
        );
        assert!(task_created(
            &mut ids,
            plan.len(),
            step.unwrap(),
            &json!({"task": {"id": "2"}})
        ));
        assert_eq!(ids, ["", "2"]);
        assert!(!task_call(&mut plan, &mut ids, "TaskCreate", &json!({"subject": "  "})).1);
    }

    #[test]
    fn questions_and_their_answers() {
        let input = json!({"questions": [
            {"question": "Which  database?\n", "header": "Database", "multiSelect": false,
             "options": [{"label": "Postgres", "description": "The usual"}, {"label": "SQLite"}]},
            {"question": "", "options": []},
            {"question": "Which extras?", "multiSelect": true, "options": [{"label": "Backups", "description": ""}]}]});
        let qs = questions(&input);
        assert_eq!(qs.len(), 2, "an empty question is left out");
        assert_eq!(qs[0].text, "Which  database?");
        assert_eq!(
            qs[0].key, "Which  database?\n",
            "answers go under Claude Code's own text"
        );
        assert!(!qs[0].multi && qs[1].multi);
        let md = questions_markdown(&qs, &["SQLite".into()]);
        assert!(
            md.contains("**Database**")
                && md.contains("- **Postgres**: The usual")
                && md.contains("- SQLite"),
            "{md}"
        );
        assert!(
            md.contains("→ SQLite") && md.contains("Which extras? *(pick any)*"),
            "{md}"
        );
        let out = answered_input(&input, &qs, &["SQLite".into(), "Backups, Alerts".into()]);
        assert_eq!(
            out["answers"],
            json!({"Which  database?\n": "SQLite", "Which extras?": "Backups, Alerts"})
        );
        assert_eq!(
            out["questions"], input["questions"],
            "its questions go back as they were"
        );
    }

    use super::*;

    #[test]
    fn auth_status_reads_the_plan_and_billing() {
        let mut d = Detection::default();
        apply_auth_status(
            &mut d,
            true,
            r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
                "email": "a@b.c", "subscriptionType": "max"}"#,
        );
        assert!(d.signed_in && !d.api_key);
        assert_eq!(d.plan.as_deref(), Some("max"));
        assert_eq!(d.account.as_deref(), Some("a@b.c"));
        let mut d = Detection::default();
        apply_auth_status(
            &mut d,
            true,
            r#"{"loggedIn": true, "authMethod": "api_key"}"#,
        );
        assert!(d.signed_in && d.api_key);
        let mut d = Detection::default();
        apply_auth_status(&mut d, false, r#"{"loggedIn": false}"#);
        assert!(!d.signed_in && !d.api_key);
        let mut d = Detection::default();
        apply_auth_status(&mut d, false, "Not logged in");
        assert!(!d.signed_in);
    }

    #[test]
    fn hooks_are_registered_with_initialize() {
        let v: Value = serde_json::from_str(&initialize_line("init", true, true)).unwrap();
        assert_eq!(v["request"]["subtype"], "initialize");
        assert_eq!(
            v["request"]["hooks"]["PreToolUse"][0]["hookCallbackIds"][0],
            HOOK_POLICY
        );
        assert!(
            v["request"]["hooks"]["PreToolUse"][0]
                .get("matcher")
                .is_none()
        );
        assert_eq!(
            v["request"]["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            v["request"]["hooks"]["Stop"][0]["hookCallbackIds"][0],
            HOOK_STOP
        );
        // Even private/report-only runs retain policy and lifecycle callbacks.
        let bare: Value = serde_json::from_str(&initialize_line("init", false, false)).unwrap();
        assert_eq!(
            bare["request"]["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(bare["request"]["hooks"].get("Stop").is_some());
        let mode: Value =
            serde_json::from_str(&set_permission_mode_line("p", PermissionMode::ReadOnly)).unwrap();
        assert_eq!(
            mode["request"],
            json!({"subtype": "set_permission_mode", "mode": "plan"})
        );
        let r: Value = serde_json::from_str(&hook_line("h1", json!({}))).unwrap();
        assert_eq!(r["response"]["request_id"], "h1");
        assert_eq!(r["response"]["response"], json!({}));
        assert_eq!(
            stop_output("x")["hookSpecificOutput"]["additionalContext"],
            "x"
        );
        assert_eq!(
            edit_path(&json!({"notebook_path": "/w/n.ipynb"})),
            Some("/w/n.ipynb")
        );
        assert_eq!(edit_path(&json!({"file_path": " "})), None);
        let s: Value = serde_json::from_str(&stop_task_line("s1", "task9")).unwrap();
        assert_eq!(
            s["request"],
            json!({"subtype": "stop_task", "task_id": "task9"})
        );
    }

    #[test]
    fn fast_mode_and_search_tools_follow_the_chat() {
        let a = args(&Launch {
            fast: true,
            ..launch("opus")
        });
        let at = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
        assert_eq!(at("--settings").as_deref(), Some(r#"{"fastMode":true}"#));
        assert!(!args(&launch("opus")).iter().any(|x| x == "--settings"));
        let m = models_from_init(
            &json!({"models": [
            {"value": "opus", "displayName": "Opus 5.5", "supportsFastMode": true},
            {"value": "sonnet", "displayName": "Sonnet 5.5"}]}),
            PROVIDER,
        );
        assert!(m[0].fast && !m[1].fast);
        // A preset without commands but with search gets Glob and Grep back.
        let research = limits_for(Some(&["read".into(), "grep".into(), "glob".into()]));
        assert_eq!(research.restore, ["Grep", "Glob"]);
        assert!(!research.only.contains(&"Bash") && !research.only.contains(&"Workflow"));
        let a = args(&Launch {
            bridge: true,
            limits: research,
            ..launch("opus")
        });
        let at = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
        assert_eq!(
            at("--allowedTools").as_deref(),
            Some("mcp__tessera,Grep,Glob")
        );
        assert_eq!(
            limits_for(Some(&["bash".into(), "grep".into()])).restore,
            ["Grep"]
        );
    }

    fn launch(model: &str) -> Launch<'_> {
        Launch {
            model,
            effort: None,
            fast: false,
            mode: PermissionMode::Default,
            resume: None,
            private: false,
            add_dirs: vec![],
            bridge: false,
            context: None,
            limits: ToolLimits::default(),
            helper: false,
        }
    }

    #[test]
    fn command_line_follows_the_chat() {
        let a = args(&Launch {
            effort: effort(ThinkingLevel::High),
            mode: PermissionMode::Ask,
            resume: Some("abc"),
            add_dirs: vec![PathBuf::from("/up")],
            ..launch("sonnet")
        });
        let s = a.join(" ");
        assert!(s.contains("--permission-prompt-tool stdio"));
        assert!(s.contains("--permission-mode default"));
        assert!(s.contains("--model sonnet --effort high --resume abc"));
        assert!(s.ends_with("--add-dir /up"));
        assert!(!s.contains("dangerously") && !s.contains("bypass"));
        let yolo = args(&Launch {
            mode: PermissionMode::Auto,
            private: true,
            // A private chat never gets Tessera's tools or context.
            bridge: true,
            context: Some(Path::new("/tmp/ctx.md")),
            ..launch("opus")
        })
        .join(" ");
        assert!(yolo.contains("--permission-mode acceptEdits"));
        assert!(yolo.contains("--no-session-persistence --tools WebSearch,WebFetch"));
        // No user or project customizations; the CLI's own sign-in stays (no --bare).
        assert!(yolo.contains("--safe-mode --setting-sources  --disable-slash-commands"));
        assert!(!yolo.contains("--bare"));
        assert!(!s.contains("--safe-mode") && !s.contains("--setting-sources"));
        assert!(!yolo.contains("bypass"));
        assert!(!yolo.contains("--mcp-config") && !yolo.contains("--append-system-prompt"));
        assert_eq!(permission_mode(PermissionMode::ReadOnly), "plan");
    }

    #[test]
    fn a_private_run_gets_no_variable_that_loads_code_or_settings() {
        let names: Vec<String> = [
            "HTTPS_PROXY",
            "CLAUDE_CONFIG_DIR",
            "NODE_EXTRA_CA_CERTS",
            "NODE_OPTIONS",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "PYTHONPATH",
            "CLAUDE_CODE_PLUGIN_DIRS",
            "claude_code_process_wrapper",
            "BASH_ENV",
            "npm_config_prefix",
            "MY_TOOL_HOME",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            private_pass_env(&names),
            [
                "HTTPS_PROXY",
                "CLAUDE_CONFIG_DIR",
                "NODE_EXTRA_CA_CERTS",
                "MY_TOOL_HOME"
            ]
        );
        assert!(PRIVATE_ENV.contains(&("CLAUDE_CODE_SAFE_MODE", "1")));
        assert!(PRIVATE_ENV.contains(&("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")));
    }

    /// Every request Tessera turns down gets Tessera's own words in the form
    /// Claude Code shows its model, and never an interruption.
    #[test]
    fn a_declined_request_says_why_and_never_interrupts() {
        let why = "This chat's tools changed; continuing with the updated list.";
        let req = |request: Value| json!({"type": "control_request", "request_id": "r1", "request": request});
        let answer = |request: Value| -> Value {
            serde_json::from_str(&decline_request(&req(request), why)).unwrap()
        };
        let perm = answer(json!({"subtype": "can_use_tool", "tool_name": "Bash", "input": {}}));
        assert_eq!(perm["response"]["subtype"], "success");
        assert_eq!(perm["response"]["request_id"], "r1");
        assert_eq!(perm["response"]["response"]["behavior"], "deny");
        assert_eq!(perm["response"]["response"]["message"], why);
        assert_eq!(perm["response"]["response"]["interrupt"], false);
        let hook = answer(
            json!({"subtype": "hook_callback", "callback_id": HOOK_POLICY,
            "input": {"hook_event_name": "PreToolUse", "tool_name": "Read"}}),
        );
        let out = &hook["response"]["response"]["hookSpecificOutput"];
        assert_eq!(out["permissionDecision"], "deny");
        assert_eq!(out["permissionDecisionReason"], why);
        let stop = answer(json!({"subtype": "hook_callback", "callback_id": HOOK_STOP,
            "input": {"hook_event_name": "Stop"}}));
        assert_eq!(stop["response"]["response"], json!({}));
        let call = answer(json!({"subtype": "mcp_message", "server_name": SERVER,
            "message": {"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "note"}}}));
        let result = &call["response"]["response"]["mcp_response"];
        assert_eq!(result["id"], 7);
        assert_eq!(result["result"]["isError"], true);
        assert_eq!(result["result"]["content"][0]["text"], why);
        let other = answer(json!({"subtype": "mcp_message", "server_name": SERVER,
            "message": {"jsonrpc": "2.0", "id": 8, "method": "tools/list"}}));
        assert_eq!(other["response"]["subtype"], "error");
        assert_eq!(other["response"]["error"], why);
    }

    #[test]
    fn tessera_tools_and_context_are_flags_of_the_run() {
        let a = args(&Launch {
            bridge: true,
            context: Some(Path::new("/tmp/t/ctx.md")),
            ..launch("opus")
        });
        let at = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
        let cfg: Value = serde_json::from_str(&at("--mcp-config").unwrap()).unwrap();
        // The Agent SDK's in-process kind: no command, URL or token in it.
        assert_eq!(
            cfg,
            json!({"mcpServers": {"tessera": {"type": "sdk", "name": "tessera", "timeout": 86_400_000}}})
        );
        assert_eq!(at("--allowedTools").as_deref(), Some("mcp__tessera"));
        assert_eq!(
            at("--append-system-prompt-file").as_deref(),
            Some("/tmp/t/ctx.md")
        );
        // Ordinary chats retain user integrations alongside the explicit ceiling.
        assert!(!a.iter().any(|x| x == "--strict-mcp-config"));
        assert!(a.iter().any(|x| x == "--tools"));
        // A bot's preset limits Claude Code's own tools and servers too.
        let bot = args(&Launch {
            bridge: true,
            limits: limits_for(Some(&["read".into(), "web_search".into(), "note".into()])),
            ..launch("opus")
        })
        .join(" ");
        assert!(bot.contains("--disallowedTools Bash,"), "{bot}");
        assert!(bot.contains("Edit") && bot.contains("WebFetch") && bot.contains("Agent"));
        let limits = limits_for(Some(&["read".into(), "web_search".into(), "note".into()]));
        assert!(limits.only.contains(&"Read") && limits.only.contains(&"WebSearch"));
        assert!(!limits.only.contains(&"Write"));
        assert!(bot.ends_with("--strict-mcp-config"), "{bot}");
        assert!(limits_for(None).only.contains(&"Bash"));
        // A helper: no tools of its own, nothing kept, only Tessera's server.
        let helper = args(&Launch {
            bridge: true,
            helper: true,
            ..launch("haiku")
        })
        .join(" ");
        assert!(
            helper.contains("--tools  --no-session-persistence --strict-mcp-config"),
            "{helper}"
        );
    }

    #[test]
    fn the_bridge_speaks_mcp() {
        let tools = vec![crate::llm::ToolSpec {
            name: "note".into(),
            description: "Notes".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }];
        let Rpc::Reply(init) = mcp_request(
            &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}}),
            &tools,
        ) else {
            panic!()
        };
        assert_eq!(init["id"], 0);
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(init["result"]["serverInfo"]["name"], "tessera");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        // A notification gets an empty result.
        assert_eq!(
            mcp_request(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                &tools
            ),
            Rpc::Reply(json!({"jsonrpc": "2.0", "result": {}}))
        );
        let Rpc::Reply(list) = mcp_request(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            &tools,
        ) else {
            panic!()
        };
        assert_eq!(list["result"]["tools"][0]["name"], "note");
        assert_eq!(list["result"]["tools"][0]["inputSchema"]["type"], "object");
        assert_eq!(
            list["result"]["tools"][0]["_meta"]["anthropic/alwaysLoad"],
            true
        );
        // A call says which tool call it is (Claude Code's `_meta`).
        assert_eq!(
            mcp_request(
                &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                    "name": "note", "arguments": {"action": "list"},
                    "_meta": {"claudecode/toolUseId": "toolu_9"}}}),
                &tools
            ),
            Rpc::Call {
                id: json!(2),
                name: "note".into(),
                arguments: json!({"action": "list"}),
                call: Some("toolu_9".into()),
            }
        );
        let Rpc::Reply(err) = mcp_request(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
            &tools,
        ) else {
            panic!()
        };
        assert_eq!(err["error"]["code"], -32601);
        let r = mcp_result(
            &json!(2),
            &[
                ContentPart::text("done"),
                ContentPart::Image {
                    mime: "image/png".into(),
                    data: "AAAA".into(),
                },
            ],
            false,
        );
        assert_eq!(r["result"]["content"][0]["text"], "done");
        assert_eq!(r["result"]["content"][1]["mimeType"], "image/png");
        assert_eq!(r["result"]["isError"], false);
        let line: Value = serde_json::from_str(&mcp_line("req-1", r)).unwrap();
        assert_eq!(line["response"]["request_id"], "req-1");
        assert_eq!(line["response"]["response"]["mcp_response"]["id"], 2);
    }

    #[test]
    fn what_is_bridged_and_how_it_is_named() {
        for t in [
            "note",
            "whiteboard",
            "browser",
            "machines",
            "memory",
            "notify",
            "share_file",
            "docs",
            "agents",
            "lsp",
            "save_secret",
            "mcp__lab__shout",
            "read",
            "grep",
            "write",
            "edit",
            "glob",
            "web_fetch",
        ] {
            assert!(bridged(t), "{t}");
        }
        assert!(!bridged("web_search"));
        assert!(bridged("agent"));
        // Commands do not depend on vault contents at launch.
        assert!(bridged("bash") && bridged("job"));
        assert!(bridge_description("agents", "x").contains("permissions, slots and worktrees"));
        assert!(bridge_description("bash", "x").contains("redacted $VARIABLE"));
        assert_eq!(native_name("mcp__tessera__note"), Some("note"));
        assert_eq!(native_name("mcp__github__x"), None);
        assert_eq!(shown_name("Bash"), "Bash");
    }

    #[test]
    fn native_credential_targets_are_redirected_before_results() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        for path in [
            ".env",
            ".env.production",
            ".env.example",
            "cert.pem",
            "client.key",
            "id_rsa",
            "id_ed25519",
            "id_ecdsa",
            "id_ecdsa_sk",
            "id_ed25519_sk",
            ".netrc",
            ".pgpass",
            ".npmrc",
            ".pypirc",
            "credentials.json",
            "auth.json",
            "secrets.json",
            "kubeconfig",
            ".kube/config",
            ".aws/credentials",
            ".config/gh/hosts.yml",
            ".config/gcloud/application_default_credentials.json",
            ".docker/config.json",
            ".cargo/credentials.toml",
            ".config/rclone/rclone.conf",
            ".codex/auth.json",
            ".password-store/entry.gpg",
        ] {
            for tool in ["Read", "Grep", "Glob", "NotebookEdit"] {
                assert!(
                    credential_target(
                        tool,
                        &json!({"path": path, "file_path": path, "notebook_path": path, "pattern": path}),
                        cwd
                    ),
                    "{tool}: {path}"
                );
            }
            assert!(
                credential_target("Bash", &json!({"command": format!("cat '{path}'")}), cwd),
                "{path}"
            );
        }
        for path in ["src/main.rs", "notes.txt", "id_ed25519.pub"] {
            assert!(!credential_target("Read", &json!({"file_path": path}), cwd));
            assert!(!credential_target(
                "Bash",
                &json!({"command": format!("cat '{path}'")}),
                cwd
            ));
        }
        assert!(credential_target(
            "Glob",
            &json!({"pattern":"**/*.pem"}),
            cwd
        ));
        assert!(credential_target(
            "Grep",
            &json!({"path":"src", "pattern":"config"}),
            cwd
        ));
        assert!(!credential_target(
            "Write",
            &json!({"file_path":".env"}),
            cwd
        ));
    }
    #[cfg(unix)]
    #[test]
    fn native_tools_check_an_obvious_file_alias_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.production"), "PORT=8080").unwrap();
        std::os::unix::fs::symlink(".env.production", dir.path().join("settings.txt")).unwrap();
        assert!(credential_target(
            "Read",
            &json!({"file_path":"settings.txt"}),
            dir.path()
        ));
        assert!(credential_target(
            "Bash",
            &json!({"command":"cat settings.txt"}),
            dir.path()
        ));
    }

    #[test]
    fn the_context_is_short_and_says_what_it_brings() {
        let c = context(
            &["note".into(), "memory".into()],
            Some("You are Ada (@ada)."),
            &["# Memory\n- likes tea".into(), String::new()],
        );
        assert!(c.starts_with("# Tessera\n"));
        assert!(c.contains("Only memory persists") && !c.contains("Use web_fetch"));
        assert!(c.contains(
            "Versioned ledger updates replace older work state, including cleared goals"
        ));
        assert!(c.contains("You are Ada") && c.contains("likes tea"));
        assert!(c.len() < 1500, "{}", c.len());
        let bare = context(&[], None, &[]);
        assert!(!bare.contains("mcp__tessera__"));
    }

    #[test]
    fn one_shot_answers() {
        let a = one_shot_args("haiku", Path::new("/tmp/s.md")).join(" ");
        assert!(a.contains("-p --output-format json --model haiku --tools  --strict-mcp-config --no-session-persistence --system-prompt-file /tmp/s.md"));
        let ok = one_shot_result(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"A title","usage":{"input_tokens":5,"output_tokens":2},"total_cost_usd":0.001,"modelUsage":{"claude-haiku-4-5":{}}}"#,
        )
        .unwrap();
        assert_eq!(ok.text, "A title");
        assert_eq!((ok.usage.input, ok.usage.output), (5, 2));
        assert_eq!(ok.model.as_deref(), Some("claude-haiku-4-5"));
        let e = one_shot_result(
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#,
        )
        .err()
        .unwrap();
        assert!(e.contains("sign in"), "{e}");
        assert!(one_shot_result("").is_err());
    }

    #[test]
    fn hints_name_the_server() {
        let usual = Account::of(PROVIDER, "", Some(Path::new("/srv/accounts")));
        assert_eq!(usual.dir, None, "the usual account is Claude Code's own");
        let missing = info(&Detection::default(), "my-server", PROVIDER, &usual);
        assert!(missing.hint.contains("on my-server"), "{}", missing.hint);
        let found = Detection {
            path: Some("/usr/bin/claude".into()),
            ..Default::default()
        };
        let out = info(&found, "my-server", PROVIDER, &usual);
        assert!(
            out.hint.starts_with("Sign in by running `claude` once")
                && out.hint.contains("my-server"),
            "{}",
            out.hint
        );
        assert_eq!(
            (out.sign_in, out.provider.as_deref()),
            (None, Some(PROVIDER))
        );
    }

    /// Another Claude account: its own folder (`CLAUDE_CONFIG_DIR`), named by
    /// Tessera, filled by Claude Code's own login; the hint says what to run.
    #[test]
    fn another_account_signs_in_with_its_own_folder() {
        let work = Account::of("claude-code-work", "", Some(Path::new("/srv/accounts")));
        assert_eq!(
            work.dir.as_deref(),
            Some(Path::new("/srv/accounts/claude-code-work"))
        );
        assert_ne!(
            work.key(),
            Account::of(PROVIDER, "", None).key(),
            "known apart"
        );
        assert_eq!(
            Account::of(PROVIDER, " claude ", None).key(),
            "claude",
            "the usual one as before"
        );
        // A provider set up by hand with another command is Claude Code's usual account.
        assert_eq!(
            Account::of("my-claude", "/opt/claude", Some(Path::new("/srv/accounts"))).dir,
            None
        );
        let found = Detection {
            path: Some("/usr/bin/claude".into()),
            ..Default::default()
        };
        let out = info(&found, "my-server", "claude-code-work", &work);
        let run = out.sign_in.clone().unwrap();
        assert!(
            run.contains("CLAUDE_CONFIG_DIR")
                && run.contains("/srv/accounts/claude-code-work")
                && run.ends_with("claude"),
            "{run}"
        );
        assert!(
            out.hint.contains(&format!("`{run}`")) && out.hint.contains("my-server"),
            "{}",
            out.hint
        );
        assert_eq!(out.provider.as_deref(), Some("claude-code-work"));
        // Run as that account: the folder is made (only for this user) and named to Claude Code.
        let dir = tempfile::tempdir().unwrap();
        let acc = Account::of("claude-code-two", "", Some(dir.path()));
        let mut cmd = tokio::process::Command::new("true");
        acc.apply(&mut cmd);
        let made = dir.path().join("claude-code-two");
        assert!(made.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(made.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        let envs: Vec<_> = cmd.as_std().get_envs().collect();
        assert!(envs.iter().any(|(k, v)| *k == "CLAUDE_CONFIG_DIR" && v.map(Path::new) == Some(made.as_path())));
        // Nothing is ever written inside it by Tessera.
        assert_eq!(std::fs::read_dir(&made).unwrap().count(), 0);
    }

    #[test]
    fn messages_map_to_transcript_parts() {
        let parts = assistant_parts(&json!([
            {"type": "thinking", "thinking": "", "signature": "x"},
            {"type": "text", "text": "Looking."},
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
        ]));
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[1], AssistantPart::ToolCall { name, .. } if name == "Bash"));
        let r = tool_result_parts(&json!([{"type": "text", "text": "a.txt"}]));
        assert_eq!(r, vec![ContentPart::text("a.txt")]);
        let line: Value = serde_json::from_str(&user_line(&[ContentPart::text("hi")])).unwrap();
        assert_eq!(line["message"]["content"][0]["text"], "hi");
        let allow: Value =
            serde_json::from_str(&permission_line("r1", true, &json!({"a": 1}), "")).unwrap();
        assert_eq!(allow["response"]["response"]["updatedInput"]["a"], 1);
        let deny: Value =
            serde_json::from_str(&permission_line("r1", false, &Value::Null, "no")).unwrap();
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert_eq!(deny["response"]["request_id"], "r1");
    }

    #[test]
    fn edits_become_diffs() {
        let d = edit_details(
            &json!({"filePath": "/w/src/a.rs", "structuredPatch": [
                {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-a", "+b"]}
            ]}),
            Path::new("/w"),
        )
        .unwrap();
        assert_eq!(d["path"], "src/a.rs");
        assert!(
            d["diff"]
                .as_str()
                .unwrap()
                .contains("@@ -1,1 +1,1 @@\n-a\n+b")
        );
        let c = edit_details(
            &json!({"type": "create", "filePath": "/w/n.txt", "content": "hi\n", "structuredPatch": []}),
            Path::new("/w"),
        )
        .unwrap();
        assert!(c["diff"].as_str().unwrap().contains("+hi"));
    }

    #[test]
    fn usage_is_counted_once_across_resumes() {
        let first = json!({"m": {"inputTokens": 10, "outputTokens": 5, "costUSD": 0.5}});
        let (rows, totals) = usage_since(first.as_object().unwrap(), &BTreeMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].usage.input, 10);
        // The next turn reports running totals: only the difference counts.
        let second = json!({"m": {"inputTokens": 25, "outputTokens": 7, "costUSD": 0.8},
                            "h": {"inputTokens": 3, "costUSD": 0.01}});
        let (rows, totals) = usage_since(second.as_object().unwrap(), &totals);
        let m = rows.iter().find(|r| r.model == "m").unwrap();
        assert_eq!((m.usage.input, m.usage.output), (15, 2));
        assert!((m.cost - 0.3).abs() < 1e-9);
        assert_eq!(rows.iter().find(|r| r.model == "h").unwrap().usage.input, 3);
        // Counting started over (a new process that didn't carry it on).
        let fresh = json!({"m": {"inputTokens": 4, "outputTokens": 1, "costUSD": 0.1}});
        let (rows, _) = usage_since(fresh.as_object().unwrap(), &totals);
        assert_eq!(rows[0].usage.input, 4);
    }

    #[test]
    fn the_window_is_the_chat_models_not_the_small_ones() {
        // What a result reports after a turn where Claude Code also used Haiku for a quick task:
        // Haiku sorts first, and its 200K must not become an Opus chat's window.
        let usage = serde_json::json!({
            "claude-haiku-4-5-20251001": {"inputTokens": 900, "contextWindow": 200_000},
            "claude-opus-5-5": {"inputTokens": 250_000, "contextWindow": 1_000_000},
        });
        assert_eq!(reported_window(&usage, "claude-opus-5-5"), Some(1_000_000));
        assert_eq!(
            reported_window(&usage, "claude-opus-5-5[1m]"),
            Some(1_000_000)
        );
        assert_eq!(reported_window(&usage, "opus"), Some(1_000_000));
        assert_eq!(
            reported_window(&usage, "claude-haiku-4-5-20251001"),
            Some(200_000)
        );
        // A model it didn't report: nothing (the model list's window stays).
        assert_eq!(reported_window(&usage, "claude-sonnet-5-5"), None);
        // Two of a family and only an alias: no guess.
        let two = serde_json::json!({
            "claude-opus-5-5": {"contextWindow": 1_000_000},
            "claude-opus-4-1": {"contextWindow": 200_000},
        });
        assert_eq!(reported_window(&two, "opus"), None);
    }

    #[test]
    fn claude_models_get_their_real_context_window() {
        for (id, w) in [
            ("opus", 1_000_000),
            ("sonnet", 1_000_000),
            ("haiku", 200_000),
            ("claude-opus-5-5", 1_000_000),
            ("claude-fable-5-1", 1_000_000),
            ("claude-sonnet-5", 1_000_000),
            ("claude-opus-4-8", 1_000_000),
            ("claude-opus-4-7", 1_000_000),
            ("claude-opus-4-6", 200_000),
            ("claude-opus-4-6[1m]", 1_000_000),
            ("claude-sonnet-4-6", 200_000),
            ("claude-sonnet-4-20250514", 200_000),
            ("claude-haiku-4-5-20251001", 200_000),
        ] {
            assert_eq!(context_window(id), w, "{id}");
        }
        // From the CLI's list: by the model an alias resolves to.
        let m = models_from_init(
            &json!({"models": [
                {"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus 5.5"},
                {"value": "claude-opus-4-6", "resolvedModel": "claude-opus-4-6", "displayName": "Opus 4.6"},
            ]}),
            PROVIDER,
        );
        assert_eq!(m[0].context_window, 1_000_000);
        assert_eq!(m[1].context_window, 200_000);
    }

    #[test]
    fn models_come_from_the_cli() {
        let m = models_from_init(
            &json!({"models": [
                {"value": "default", "displayName": "Default"},
                {"value": "opus", "displayName": "Opus 5.5", "supportedEffortLevels": ["low", "high"]},
                {"value": "haiku", "displayName": "Haiku 4.5"},
            ]}),
            "claude-code-work",
        );
        assert_eq!(m.len(), 2);
        assert!(
            m.iter().all(|m| m.provider == "claude-code-work"),
            "each account its own models"
        );
        assert_eq!(m[0].name, "Opus 5.5");
        assert_eq!(m[0].thinking.len(), 3);
        assert!(m[1].thinking.is_empty());
        assert_eq!(effort(ThinkingLevel::XHigh), Some("xhigh"));
        assert_eq!(effort(ThinkingLevel::Auto), None);
    }
}
