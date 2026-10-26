//! Configuration (`config.toml`) and filesystem layout.
//!
//! Layout (XDG by default, or everything under `$TESSERA_HOME`):
//! - config:  `~/.config/tessera/config.toml`, `skills/`
//! - data:    `~/.local/share/tessera/` → `tessera.db`, `auth.json`, `memory/`
//!
//! On Windows both are `%APPDATA%\tessera` (one folder, as with `TESSERA_HOME`).

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tessera_proto::{ModelInfo, ModelRef, PermissionMode, ProviderKind};

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        if let Ok(home) = std::env::var("TESSERA_HOME") {
            let home = PathBuf::from(home);
            return Self {
                config_dir: home.clone(),
                data_dir: home,
            };
        }
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tessera");
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tessera");
        Self {
            config_dir,
            data_dir,
        }
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            config_dir: dir.clone(),
            data_dir: dir,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("tessera.db")
    }
    pub fn auth_file(&self) -> PathBuf {
        self.data_dir.join("auth.json")
    }
    pub fn memory_dir(&self) -> PathBuf {
        self.data_dir.join("memory")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.config_dir.join("skills")
    }
    /// The user's own subagent definitions (`*.md`).
    pub fn agents_dir(&self) -> PathBuf {
        self.config_dir.join("agents")
    }
    /// Where Claude accounts other than the usual one keep Claude Code's
    /// configuration and sign-in (`CLAUDE_CONFIG_DIR`), a folder each, named
    /// by their provider id: next to Tessera's data, outside it, as Claude
    /// Code runs in the command sandbox with Tessera's own folders hidden.
    /// Tessera never reads or writes anything inside them.
    pub fn claude_accounts_dir(&self) -> PathBuf {
        let name = self
            .data_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tessera".into());
        self.data_dir.with_file_name(format!("{name}-claude"))
    }

    /// Files the user attached to messages, one folder per session. Agents
    /// read them, so they live outside the protected directories (in the
    /// cache), or in a readable exception under a single `TESSERA_HOME`.
    pub fn uploads_dir(&self) -> PathBuf {
        if self.config_dir == self.data_dir {
            self.data_dir.join("uploads")
        } else {
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("tessera")
                .join("uploads")
        }
    }
    pub fn ensure(&self) -> Result<()> {
        for d in [
            &self.config_dir,
            &self.data_dir,
            &self.memory_dir(),
            &self.skills_dir(),
        ] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        // Sessions, credentials and settings are nobody else's business.
        #[cfg(unix)]
        for d in [&self.config_dir, &self.data_dir] {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    /// Where the desktop app keeps its saved servers (device tokens).
    pub fn app_data_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("dev.tessera.app"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    /// `provider/model` used for new sessions (the `default` role; a
    /// `default` set under `[models]` takes precedence).
    pub default_model: Option<String>,
    /// Optional cheaper model for titles and compaction summaries (the
    /// `utility` role, also called `fast`).
    pub utility_model: Option<String>,
    /// Model roles usable as `@role` wherever a model is chosen, and
    /// fallback chains for when a model keeps failing.
    pub models: ModelsConfig,
    /// Prices in US dollars per million tokens, keyed by `provider/model`,
    /// model id, or a pattern with `*` (e.g. `"claude-sonnet-*"`).
    pub prices: BTreeMap<String, Price>,
    /// Per-model harness settings over the built-in model profiles, keyed
    /// like `prices` (see [`ModelTuning`]).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tuning: BTreeMap<String, ModelTuning>,
    /// The price catalog and flat-fee plans (`[pricing]`).
    pub pricing: PricingConfig,
    pub server: ServerConfig,
    pub agent: AgentConfig,
    pub context: ContextConfig,
    pub memory: MemoryConfig,
    pub skills: SkillsConfig,
    pub web: WebConfig,
    pub voice: VoiceSettings,
    pub security: SecurityConfig,
    pub lsp: LspConfig,
    /// The server-side browser behind web view tiles and the `browser` tool.
    pub browser: crate::browser::BrowserConfig,
    /// The user's own terminals on this server (off unless turned on).
    pub terminal: TerminalConfig,
    /// Several ChatGPT or Claude subscriptions (`[accounts]`).
    #[serde(skip_serializing_if = "AccountsConfig::is_default")]
    pub accounts: AccountsConfig,
    #[serde(rename = "provider")]
    pub providers: Vec<ProviderConfig>,
    #[serde(rename = "mcp")]
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Address `tessera serve` listens on. Use `0.0.0.0:7878` to reach it from
    /// other devices on the LAN or through a tunnel.
    pub bind: String,
    /// Display name shown to clients.
    pub name: String,
    /// Default working directory for new sessions created by remote clients.
    pub default_cwd: Option<String>,
    /// "Use Tessera in a web browser" (off by default): the web app at
    /// `/web/` on the server's https address, for browsers paired with a
    /// code from one of the user's devices. Off: nothing is served there and
    /// paired browsers can't connect. Switched in Settings → Connection.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub web_access: bool,
    /// Windows: while the server runs in the background (started at
    /// sign-in), keep the computer from going to sleep by itself as long as
    /// it is plugged in, so devices can reach it. On by default; nothing
    /// elsewhere.
    #[serde(skip_serializing_if = "Clone::clone")]
    pub keep_awake: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7878".into(),
            name: "Tessera".into(),
            default_cwd: None,
            web_access: false,
            keep_awake: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub permission: PermissionMode,
    /// Upper bound on model calls in one turn (a runaway guard, not a budget).
    pub max_steps: u32,
    /// Seconds before a foreground shell command is moved to the background.
    pub bash_timeout: u64,
    /// Extra server environment names inherited by agent commands and Claude Code.
    /// User-only: values may be secrets or change how child programs run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pass_env: Vec<String>,
    /// Extra text appended to the system prompt.
    pub instructions: Option<String>,
    /// Background review that curates memory and skills every N user turns (0 = off).
    pub review_interval: u32,
    /// Also review a chat that went quiet for this many minutes after at
    /// least two unreviewed turns, so short chats teach it too (0 = off).
    pub review_idle_minutes: u32,
    /// A turn that ran this many minutes ends with a notification on the
    /// user's devices (also when it starts waiting for a usage limit; 0 = off).
    pub notify_after_minutes: f64,
    /// Subagents (helpers) working at once in one chat, unless the chat set
    /// its own (more wait their turn); legacy 0 switches helpers off,
    /// u32::MAX means no limit. Settings → Models → Helpers changes it live.
    pub max_subagents: usize,
    /// Tool calls a subagent may make before it is asked to write its report
    /// (0: no limit; a chat may set its own).
    pub subagent_steps: u32,
    /// Minutes a subagent may work before it is asked to report (it is
    /// stopped a few minutes later if it doesn't; 0: no limit; a chat may set its own).
    pub subagent_minutes: u32,
    /// Helpers working at once across all chats, whatever each chat chose
    /// (0: what this machine's memory and processors carry; see
    /// `engine/helper_limits.rs`).
    pub helper_capacity: usize,
    /// Before a turn ends, ask the agent once to check its "done when"
    /// criteria and plan (only when the task ledger has them).
    pub completion_check: bool,
    /// Before a turn that changed code ends with nothing run since (tests,
    /// a build, the program, a linter), ask once to run a check or say why
    /// none applies (`engine/verify.rs`). `on` also counts a clean language
    /// server or syntax check of the file; `strict` only commands; `off`.
    /// In Claude Code chats a `Stop` hook asks it (`engine/claude_code.rs`),
    /// counting only commands.
    pub verify_check: VerifyCheck,
    /// After `write`/`edit`, a syntax check with what is installed
    /// (Python, Node, shell) where no language server covers the file.
    pub quick_checks: bool,
    /// Cut a reply that repeats itself while it streams; ask once to go on.
    pub repetition_cut: bool,
    /// One note when a final reply says it is about to act but calls nothing.
    pub action_nudge: bool,
    /// The same failing call is refused after this many tries (min 2).
    pub loop_block_after: u32,
    /// One code file changed this many times with nothing run in between
    /// gets a note (0 = never).
    pub edit_warn_after: u32,
    /// Take `apply_patch` ("*** Begin Patch") edits from GPT/Codex models.
    pub apply_patch: bool,
    /// Minutes a chat's Claude Code process stays open after a turn, so the
    /// next message skips its start (2.4–3.2 s a turn measured on this
    /// server); 0 starts a new one for every turn.
    pub claude_keep_open_minutes: u32,
    /// Which interrupted sessions continue by themselves after a restart.
    pub resume_on_restart: ResumeOnRestart,
    /// Lean start: chats begin with the core tools and a one-line list of
    /// the others, which turn on the first time one is called (or `docs`
    /// topic `tools` is read); shorter descriptions for the largest tools, and
    /// capped skills and password lists. Fewer tokens before the user's
    /// first word, one prompt-cache miss in chats that use another tool.
    /// Off until benchmarked (`profiles::LEAN_START_LATER`).
    pub lean_tools: bool,
}

/// What counts as checking changed code (`agent.verify_check`; `true` and
/// `false` mean `on` and `off`).
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerifyCheck {
    /// Commands, and a clean language server or syntax check of the file.
    #[default]
    On,
    /// Only commands (tests, builds, linters, the program).
    Strict,
    Off,
}

impl<'de> Deserialize<'de> for VerifyCheck {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Name(String),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Bool(true) => Self::On,
            Raw::Bool(false) => Self::Off,
            Raw::Name(n) => match n.trim().to_ascii_lowercase().as_str() {
                "on" | "true" => Self::On,
                "strict" => Self::Strict,
                "off" | "false" => Self::Off,
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "verify_check: `{other}` is not one of on, strict, off"
                    )));
                }
            },
        })
    }
}

/// `unattended`: scheduled jobs and chats running in `auto` permission;
/// `all`: every interrupted chat too; `off`: none (they wait for a message).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResumeOnRestart {
    #[default]
    Unattended,
    All,
    Off,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            permission: PermissionMode::Default,
            max_steps: 250,
            bash_timeout: 120,
            pass_env: Vec::new(),
            instructions: None,
            review_interval: 4,
            review_idle_minutes: 10,
            notify_after_minutes: 10.0,
            // Fewer on a small machine (see `budget.rs`).
            max_subagents: crate::budget::get().max_subagents,
            subagent_steps: 40,
            subagent_minutes: 15,
            helper_capacity: 0,
            completion_check: true,
            verify_check: VerifyCheck::On,
            quick_checks: true,
            repetition_cut: true,
            action_nudge: true,
            loop_block_after: 5,
            edit_warn_after: 5,
            apply_patch: true,
            claude_keep_open_minutes: 10,
            resume_on_restart: ResumeOnRestart::Unattended,
            lean_tools: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Compact when estimated context exceeds this share of the usable window.
    pub compact_at: f64,
    /// Cap on the working context regardless of the model window (0 =
    /// automatic: at most `context::AUTO_MAX_TOKENS`, 256k). Large windows
    /// are slow to prefill and costly to re-read; capping keeps turns fast.
    pub max_tokens: u64,
    /// Tokens of recent history kept verbatim after compaction (0 = auto).
    pub keep_recent_tokens: u64,
    /// Tool outputs larger than this (bytes) are stored as artifacts and shown truncated.
    pub max_tool_output: usize,
    /// Compact before the context passes this many tokens (0: no such
    /// limit). Never read from the file: set per model from its profile
    /// (the length a model of its size still works well with) or
    /// `[tuning] context_budget`.
    #[serde(skip)]
    pub budget: u64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            compact_at: 0.8,
            max_tokens: 0,
            keep_recent_tokens: 0,
            max_tool_output: 32 * 1024,
            budget: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub memory_chars: u32,
    pub user_chars: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_chars: 4000,
            user_chars: 2000,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Additional skill directories (e.g. `~/.claude/skills`, `~/.hermes/skills`).
    pub extra_dirs: Vec<String>,
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Search provider tried first: `auto` (follow `chain`), `searxng`,
    /// `brave`, `tavily` or `duckduckgo`. When it fails or rate-limits, the
    /// other configured providers in `chain` follow.
    pub search: String,
    pub searxng_url: Option<String>,
    /// Fallback order. Brave and Tavily are skipped without an API key
    /// (stored with the provider credentials), SearXNG without a URL.
    pub chain: Vec<String>,
    /// Endpoint overrides (proxies, tests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brave_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavily_url: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            search: "auto".into(),
            searxng_url: None,
            chain: ["searxng", "brave", "tavily", "duckduckgo"]
                .map(String::from)
                .to_vec(),
            brave_url: None,
            tavily_url: None,
        }
    }
}

/// Voice: where speech is handled (`engine`), the server's own engines, and
/// "another transcription service" (an OpenAI-compatible
/// `/audio/transcriptions`: `enabled`…`model`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    pub enabled: bool,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
    /// `auto`, `device`, `server`, `openai`, `openrouter`, `service` or `off`; empty in
    /// files written before it existed (see [`VoiceSettings::engine`]).
    pub engine: String,
    /// Keep what reaches the server in `<data>/voice/recordings`.
    pub keep_audio: bool,
    /// `auto`, `tiny` or `base`.
    pub server_model: String,
    /// A voice id; empty = the one for `language` (English otherwise).
    pub server_voice: String,
    /// ElevenLabs voice through OpenRouter; empty = Sarah.
    pub openrouter_voice: String,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "groq".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            model: "whisper-large-v3-turbo".into(),
            language: String::new(),
            engine: String::new(),
            keep_audio: false,
            server_model: "auto".into(),
            server_voice: String::new(),
            openrouter_voice: String::new(),
        }
    }
}

impl VoiceSettings {
    pub const ENGINES: [&str; 7] = [
        "auto",
        "device",
        "server",
        "openai",
        "openrouter",
        "service",
        "off",
    ];

    /// The choice in effect. A file from before the choice existed keeps the
    /// service it had set up; without one it starts on `auto` (private
    /// engines only, so nothing leaves the user's devices and server).
    pub fn engine(&self) -> &str {
        match self.engine.as_str() {
            "" if self.enabled => "service",
            "" => "auto",
            e => e,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Scrub credential-looking values (keys, tokens, private keys,
    /// `PASSWORD=`-style assignments) from tool output before models see it.
    pub redact_patterns: bool,
    /// Require end-to-end encrypted connections for everything except
    /// localhost (plain token connections are then refused from the network).
    pub require_e2ee: bool,
    /// Run the agent's commands in a sandbox that hides Tessera's own data
    /// and settings (see `sandbox.rs`).
    pub sandbox: SandboxMode,
    /// Folders beyond a chat's own where the agent may change files without
    /// asking (`write = true`), or only read them (`write = false`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<tessera_proto::FolderAccess>,
    /// Ask before the agent writes files anywhere else.
    pub ask_outside: bool,
    /// Take API keys, logins and two-factor codes typed in a chat out of the
    /// conversation before the model sees them.
    pub save_secrets_from_chat: bool,
    /// Where they go then: Passwords & keys at once (`auto`), that chat
    /// only until the user decides (`ask`), or that chat only (`chat_only`).
    pub secret_saving: SecretSaving,
    /// Where saved passwords and keys may be used on machines when one
    /// doesn't choose for itself: nowhere (`off`), on the machines in
    /// `secret_machines_picked` (`picked`), or on all of them (`all`).
    pub secret_machines: SecretMachines,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub secret_machines_picked: Vec<String>,
    /// Whether ordinary chats may hand work to bots (`on`, `ask`, `off`).
    pub bot_handoff: BotHandoff,
    /// How much freedom new chats get, one value for all the user's
    /// devices (set from one of them; unset: each app keeps its own choice
    /// and the server falls back to `agent.permission`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_permission: Option<PermissionMode>,
}

pub use tessera_proto::{BotHandoff, SandboxMode, SecretMachines, SecretSaving};

/// Several subscriptions of one kind (`[accounts]`; `engine/accounts.rs`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AccountsConfig {
    /// A chat whose account reached its usage limit carries on with the
    /// same model on the next account of that kind (ChatGPT, Claude) that
    /// is signed in and hasn't reached its own. Off unless turned on.
    pub continue_on_next: bool,
}

impl AccountsConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Terminals the user opens from their devices (`[terminal]`; see
/// `engine/terminals.rs`). Agents never get one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TerminalConfig {
    /// Off (the default), sandboxed, or full access.
    pub access: tessera_proto::TerminalAccess,
    /// Close a terminal after this many minutes without typing while
    /// nothing runs in it (15 to 240).
    pub idle_minutes: u32,
    /// Terminals open at once on this server.
    pub max_open: usize,
    /// How long a terminal waits for its device to come back.
    pub grace_secs: u64,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            access: tessera_proto::TerminalAccess::Off,
            idle_minutes: tessera_proto::TERMINAL_IDLE_MINUTES,
            max_open: crate::terminal::MAX_OPEN,
            grace_secs: crate::terminal::GRACE.as_secs(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            redact_patterns: true,
            require_e2ee: true,
            sandbox: SandboxMode::Auto,
            folders: Vec::new(),
            ask_outside: true,
            save_secrets_from_chat: true,
            secret_saving: SecretSaving::Auto,
            secret_machines: SecretMachines::Off,
            secret_machines_picked: Vec::new(),
            bot_handoff: BotHandoff::On,
            default_permission: None,
        }
    }
}

/// Language servers (`[lsp]`): diagnostics after edits and the `lsp` tool.
/// Servers are never installed by Tessera; only ones on `PATH` are used, and
/// they run in the same sandbox as `bash` (they can execute project code).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LspConfig {
    pub enabled: bool,
    /// Stop a server nobody used for this long.
    pub idle_minutes: u64,
    /// Longest wait for diagnostics after an edit (adaptive below this);
    /// later arrivals come with the next tool result.
    pub wait_ms: u64,
    /// Servers running at once (least recently used is stopped first).
    pub max_servers: usize,
    /// Kill a server whose resident memory exceeds this (0 = no limit).
    pub max_memory_mb: u64,
    /// Also start servers in read-only sessions (they may run build scripts).
    pub in_read_only: bool,
    /// Command per language (`rust`, `python`, `typescript`, `go`, `c`),
    /// replacing the built-in choice; an empty list disables the language.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub command: BTreeMap<String, Vec<String>>,
    /// Extra servers (`[[lsp.server]]`).
    #[serde(rename = "server", skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<LspServerConfig>,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_minutes: 10,
            wait_ms: 2500,
            // 4 and 8 GB; fewer and less on a small machine (see `budget.rs`).
            max_servers: crate::budget::get().lsp_servers,
            max_memory_mb: crate::budget::get().lsp_memory_mb,
            in_read_only: false,
            command: BTreeMap::new(),
            servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspServerConfig {
    pub name: String,
    /// Program and arguments, e.g. `["lua-language-server"]`.
    pub command: Vec<String>,
    /// File extensions it handles, without the dot.
    pub extensions: Vec<String>,
    /// Files or directories marking a project root (e.g. `.luarc.json`).
    #[serde(default)]
    pub markers: Vec<String>,
    /// LSP `languageId` for opened files (default: the extension).
    #[serde(default)]
    pub language_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub base_url: String,
    /// Query `/models` for the model list.
    #[serde(default)]
    pub discover: bool,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    /// Extra HTTP headers sent to this provider.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Which past reasoning to send back (OpenAI-compatible endpoints).
    #[serde(default, skip_serializing_if = "ReasoningReplay::is_default")]
    pub reasoning_replay: ReasoningReplay,
    /// Requests a server on this computer or network answers at the same
    /// time. Unset: what a llama.cpp router says it runs the model with
    /// (`--parallel`), vLLM and SGLang many, any other one at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<u32>,
}

/// `all` keeps the history append-only (best for prompt/KV caching and for
/// servers that preserve reasoning); `turn` sends it only within the current
/// tool loop; `none` never sends it (for APIs that reject it).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningReplay {
    #[default]
    All,
    Turn,
    None,
}

impl ReasoningReplay {
    pub fn is_default(&self) -> bool {
        *self == Self::All
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output: Option<u64>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub vision: Option<bool>,
    /// Price per million tokens (overrides `[prices]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    /// Offers priority processing (fast mode); default: by provider kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
}

impl ModelConfig {
    pub fn from_info(m: &ModelInfo, kind: ProviderKind) -> Self {
        Self {
            id: m.id.clone(),
            name: (m.name != m.id).then(|| m.name.clone()),
            context_window: Some(m.context_window),
            max_output: Some(m.max_output),
            reasoning: Some(m.reasoning),
            vision: Some(m.vision),
            price: None,
            fast: (m.fast != crate::providers::fast_tier(kind)).then_some(m.fast),
        }
    }
}

/// `[tuning."provider/model"]` (or a model id, or a pattern with `*`): what
/// Tessera does differently for a model, over its built-in profile
/// (`profiles.rs`). Every field is optional; where several keys match, the
/// most specific one that sets a field wins.
///
/// ```toml
/// [tuning."local/qwen3.8-27b"]
/// small = "on"              # small model mode: "auto" (default), "on", "off"
/// sampling = false          # don't send the model card's sampling values
/// temperature = 0.6         # your own values (sent to your own servers)
/// parallel_tool_calls = true
/// reply_guard = false       # small model mode: run every call of a reply, even after a failed change
/// read_paths = false        # `read` takes one file per call
/// plan_alongside = false    # no guideline to update the plan alongside other calls
/// observation_budget = 0    # never trim old tool output before compaction
/// context_budget = 0        # compact only when the window fills (0), or at N tokens
/// check = false             # skip the one-time tool-use check
/// [tuning."*"]              # every model
/// small = "off"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelTuning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small: Option<tessera_proto::SmallMode>,
    /// Send the sampling values the model's card recommends to the model's
    /// server on this computer or network (default: yes, where known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    /// Let the model make several tool calls in one reply (default: yes;
    /// not in small model mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// In small model mode, a reply's calls after a failed change are not
    /// run, and identical calls in one reply run once (default: yes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_guard: Option<bool>,
    /// `read` takes several files at once (`paths`; default: yes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_paths: Option<bool>,
    /// The guidelines ask for plan updates in the same reply as other
    /// calls (default: yes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_alongside: Option<bool>,
    /// Replace old tool output with references, in one batch, once the
    /// context passes this many tokens (well before compaction; the newest
    /// output stays). 0: off. Default: 48,000 in small model mode on big
    /// windows, else off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_budget: Option<u64>,
    /// Compact before the context passes this many tokens; 0: only when
    /// the window fills (default: by the model's size).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<u64>,
    /// Check once whether a model on your own server uses tools (default: yes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<bool>,
}

impl ModelTuning {
    /// `other`'s fields where it sets them, else these.
    fn overlay(&mut self, other: &ModelTuning) {
        macro_rules! take {
            ($($f:ident),*) => {$(if other.$f.is_some() { self.$f = other.$f; })*};
        }
        take!(
            small,
            sampling,
            temperature,
            top_p,
            top_k,
            min_p,
            presence_penalty,
            repetition_penalty,
            parallel_tool_calls,
            reply_guard,
            read_paths,
            plan_alongside,
            observation_budget,
            context_budget,
            check
        );
    }
}

/// `[models]`: named roles and fallback chains.
///
/// ```toml
/// [models]
/// default = "openai-codex/gpt-5.5"
/// fast = "local/qwen3.8-27b"
/// smart = "anthropic/claude-opus-4-5"
/// vision = "@smart"
/// [models.fallback]
/// default = ["anthropic/claude-sonnet-4-5", "@fast"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelsConfig {
    /// Models tried in order when a role's model (or the model named as the
    /// key) still fails after retries: rate limits, server errors, outages.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fallback: BTreeMap<String, Vec<String>>,
    /// Role name → `provider/model` or `@other-role`.
    #[serde(flatten)]
    pub roles: BTreeMap<String, String>,
}

/// US dollars per million tokens (the wire type, also used in `config.toml`).
pub use tessera_proto::Price;

/// The public price catalog, fetched at most daily (see `crate::pricing`).
pub const CATALOG_URL: &str = "https://models.dev/api.json";

/// `[pricing]`: where unknown prices come from, and flat-fee plans.
///
/// ```toml
/// [pricing]
/// catalog = true                    # fill in unknown prices from models.dev
/// [pricing.subscriptions.openai-codex]
/// plan = "ChatGPT Pro"
/// monthly_usd = 200.0
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PricingConfig {
    /// Look up prices Tessera doesn't know in a public catalog (only model
    /// ids are compared; nothing is sent but a plain download), at most once
    /// a day and only when the usage stats need it.
    pub catalog: bool,
    #[serde(skip_serializing_if = "is_catalog_url")]
    pub catalog_url: String,
    /// Plans by provider id. A provider listed here is billed by plan, even
    /// with an API key.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub subscriptions: BTreeMap<String, SubscriptionConfig>,
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            catalog: true,
            catalog_url: CATALOG_URL.into(),
            subscriptions: BTreeMap::new(),
        }
    }
}

fn is_catalog_url(url: &str) -> bool {
    url == CATALOG_URL
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubscriptionConfig {
    /// e.g. "ChatGPT Pro".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_usd: Option<f64>,
}

/// List prices of pay-per-token models when this was written (cache writes
/// at Anthropic's 5-minute rate; OpenAI doesn't charge for them). Used for
/// hosted models only: `[prices]` overrides them and the catalog fills in
/// what's missing. New models stay unpriced rather than guessed.
const DEFAULT_PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-1", Price::new(10.0, 50.0, 0.25, 12.5)),
    ("claude-mythos-5-1", Price::new(10.0, 50.0, 0.25, 12.5)),
    ("claude-fable-5", Price::new(10.0, 50.0, 1.0, 12.5)),
    ("claude-mythos-5", Price::new(10.0, 50.0, 1.0, 12.5)),
    ("claude-opus-5-5", Price::new(4.0, 20.0, 0.2, 5.0)),
    ("claude-opus-5", Price::new(5.0, 25.0, 0.5, 6.25)),
    ("claude-opus-4-8", Price::new(5.0, 25.0, 0.5, 6.25)),
    ("claude-opus-4-7", Price::new(5.0, 25.0, 0.5, 6.25)),
    ("claude-opus-4-6", Price::new(5.0, 25.0, 0.5, 6.25)),
    ("claude-opus-4-5*", Price::new(5.0, 25.0, 0.5, 6.25)),
    ("claude-opus-4-1*", Price::new(15.0, 75.0, 1.5, 18.75)),
    ("claude-opus-4-0", Price::new(15.0, 75.0, 1.5, 18.75)),
    ("claude-opus-4-2025*", Price::new(15.0, 75.0, 1.5, 18.75)),
    ("claude-sonnet-5-5", Price::new(2.0, 10.0, 0.2, 2.5)),
    ("claude-sonnet-5", Price::new(2.0, 10.0, 0.2, 2.5)),
    ("claude-sonnet-4*", Price::new(3.0, 15.0, 0.3, 3.75)),
    ("claude-haiku-4-5*", Price::new(1.0, 5.0, 0.1, 1.25)),
    ("gpt-5", Price::new(1.25, 10.0, 0.125, 1.25)),
    ("gpt-5-2025*", Price::new(1.25, 10.0, 0.125, 1.25)),
    ("gpt-5-codex", Price::new(1.25, 10.0, 0.125, 1.25)),
    ("gpt-5.1*", Price::new(1.25, 10.0, 0.125, 1.25)),
    ("gpt-5.1-codex-mini*", Price::new(0.25, 2.0, 0.025, 0.25)),
    ("gpt-5.2", Price::new(1.75, 14.0, 0.175, 1.75)),
    ("gpt-5.2-2025*", Price::new(1.75, 14.0, 0.175, 1.75)),
    ("gpt-5-mini*", Price::new(0.25, 2.0, 0.025, 0.25)),
    ("gpt-5-nano*", Price::new(0.05, 0.4, 0.005, 0.05)),
    ("gpt-4.1*", Price::new(2.0, 8.0, 0.5, 2.0)),
    ("gpt-4.1-mini*", Price::new(0.4, 1.6, 0.1, 0.4)),
    ("gpt-4o*", Price::new(2.5, 10.0, 1.25, 2.5)),
    ("gpt-4o-mini*", Price::new(0.15, 0.6, 0.075, 0.15)),
];

/// `pattern` with `*` wildcards matches all of `text`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// The most specific (longest) pattern in `table` matching one of `names`.
fn lookup_price<'a>(
    table: impl Iterator<Item = (&'a str, &'a Price)>,
    names: &[&str],
) -> Option<Price> {
    table
        .filter(|(k, _)| names.iter().any(|n| glob_match(k, n)))
        .max_by_key(|(k, _)| (!k.contains('*'), k.len()))
        .map(|(_, p)| *p)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {
    pub name: String,
    /// stdio transport
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Streamable HTTP transport
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Its tools wait until first used (the prompt names them; calling one
    /// turns them on): `true` always, `false` never; unset: when all MCP
    /// tools together would take more than a tenth of the chat's working
    /// context window up front (`mcp.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer: Option<bool>,
}

fn yes() -> bool {
    true
}

const HEADER: &str = "# Tessera configuration. Edited by `tessera` and the apps; comments are not preserved.\n# Secrets (API keys, OAuth tokens) live in auth.json next to the database, never here.\n\n";

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }

    /// The freedom a new chat gets when nothing else chooses (no bot,
    /// parent or client choice): the one the user set for all their
    /// devices, else `agent.permission`.
    pub fn new_chat_permission(&self) -> PermissionMode {
        self.shared_permission().unwrap_or(self.agent.permission)
    }

    /// New chats ask first wherever Automatic cannot provide the full
    /// sandbox. Only an explicit Off choice permits unconfined automation.
    pub fn shared_permission(&self) -> Option<PermissionMode> {
        self.shared_permission_on(
            self.security.sandbox == SandboxMode::Auto && !crate::sandbox::support().complete(),
        )
    }

    fn shared_permission_on(&self, unavailable: bool) -> Option<PermissionMode> {
        let chosen = self.security.default_permission;
        if self.security.sandbox == SandboxMode::Auto && unavailable {
            Some(crate::tools::cap_permission(
                chosen.unwrap_or(self.agent.permission),
                PermissionMode::Ask,
            ))
        } else {
            chosen
        }
    }

    /// Also covers existing chats, bots and explicit per-chat YOLO choices.
    pub(crate) fn sandbox_permission(&self, mode: PermissionMode) -> PermissionMode {
        if self.security.sandbox == SandboxMode::Auto && !crate::sandbox::support().complete() {
            crate::tools::cap_permission(mode, PermissionMode::Ask)
        } else {
            mode
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = format!("{HEADER}{}", toml::to_string_pretty(self)?);
        // MCP `env`/`headers` and provider headers often carry tokens.
        write_atomic(&paths.config_file(), text.as_bytes(), true)
    }

    /// The `default` role's model (`@role` chains followed).
    pub fn default_model(&self) -> Option<ModelRef> {
        self.role("default").ok().and_then(|s| s.parse().ok())
    }
    /// The `utility` (`fast`) role's model.
    pub fn utility_model(&self) -> Option<ModelRef> {
        self.role("utility").ok().and_then(|s| s.parse().ok())
    }
    /// The `memory` role's model (the background review), else `utility`'s.
    pub fn memory_model(&self) -> Option<ModelRef> {
        self.role("memory")
            .ok()
            .and_then(|s| s.parse().ok())
            .or_else(|| self.utility_model())
    }

    /// Set (`Some`) or clear a role: `provider/model` or `@other-role`.
    /// `fast` is `utility`; `default` is the top-level `default_model`.
    pub fn set_role(&mut self, name: &str, value: Option<&str>) -> Result<(), String> {
        let name = name.trim().trim_start_matches('@').to_ascii_lowercase();
        if name.is_empty()
            || name == "fallback"
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "`{name}` can't be a role name (letters, digits, - or _)"
            ));
        }
        let name = if name == "fast" {
            "utility".into()
        } else {
            name
        };
        let value = value.map(str::trim).filter(|v| !v.is_empty());
        if let Some(v) = value {
            v.parse::<ModelRef>()?;
        }
        let mut next = self.clone();
        let roles = &mut next.models.roles;
        match name.as_str() {
            "default" => {
                roles.remove("default");
                next.default_model = value.map(str::to_string);
            }
            "utility" => {
                roles.remove("fast");
                next.utility_model = None;
                match value {
                    Some(v) => roles.insert(name.clone(), v.to_string()),
                    None => roles.remove(&name),
                };
            }
            _ => {
                match value {
                    Some(v) => roles.insert(name.clone(), v.to_string()),
                    None => roles.remove(&name),
                };
            }
        }
        if let Err(e) = next.role(&name)
            && e.contains("cycle")
        {
            return Err(e);
        }
        *self = next;
        Ok(())
    }

    /// What a role is set to, before following `@` links.
    pub fn role_setting(&self, name: &str) -> Option<String> {
        self.role_value(name)
    }

    /// What a role names before following `@` links.
    fn role_value(&self, name: &str) -> Option<String> {
        let get = |k: &str| {
            self.models
                .roles
                .get(k)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let top = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        match name {
            "default" => get("default").or_else(|| top(&self.default_model)),
            "utility" => get("utility")
                .or_else(|| top(&self.utility_model))
                .or_else(|| get("fast")),
            "fast" => get("fast")
                .or_else(|| get("utility"))
                .or_else(|| top(&self.utility_model)),
            _ => get(name),
        }
    }

    /// The model a role names (`provider/model`, or whatever a user wrote
    /// there), following `@role` links. Errors name a missing role or a cycle.
    pub fn role(&self, name: &str) -> Result<String, String> {
        let mut seen: Vec<String> = Vec::new();
        let mut cur = name.trim().trim_start_matches('@').to_string();
        loop {
            if seen.contains(&cur) {
                seen.push(cur);
                return Err(format!("model roles form a cycle: @{}", seen.join(" → @")));
            }
            let Some(v) = self.role_value(&cur) else {
                return Err(format!(
                    "no model role `@{cur}` (configured: {})",
                    self.role_names()
                        .iter()
                        .map(|r| format!("@{r}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            };
            seen.push(cur);
            match v.strip_prefix('@') {
                Some(next) => cur = next.trim().to_string(),
                None => return Ok(v),
            }
        }
    }

    /// Roles that currently name something.
    pub fn role_names(&self) -> Vec<String> {
        let mut names: Vec<String> = ["default", "utility", "fast"]
            .into_iter()
            .map(String::from)
            .chain(self.models.roles.keys().cloned())
            .filter(|r| self.role_value(r).is_some())
            .collect();
        names.dedup();
        names.sort();
        names.dedup();
        names
    }

    /// Models to try, in order, when `model` (`provider/model`) keeps
    /// failing: the chain keyed by the model itself, else the chain of a
    /// role it plays, else the `default` chain. Entries may be `@role`s.
    pub fn fallback_chain(&self, model: &str) -> Vec<String> {
        let fb = &self.models.fallback;
        if let Some(chain) = fb.get(model) {
            return chain.clone();
        }
        let mut roles = vec!["default".to_string(), "utility".into(), "fast".into()];
        roles.extend(self.models.roles.keys().cloned());
        for r in roles {
            if let Some(chain) = fb.get(&r)
                && self.role(&r).ok().as_deref() == Some(model)
            {
                return chain.clone();
            }
        }
        fb.get("default").cloned().unwrap_or_default()
    }

    /// Price of a model: [`price_with_source`](Self::price_with_source)
    /// without the source.
    pub fn price(&self, model: &ModelRef, hosted: bool) -> Option<Price> {
        self.price_with_source(model, hosted).map(|(p, _)| p)
    }

    /// Price of a model and where it comes from: `[prices]."provider/model"`
    /// (what the apps set), its `[[provider.models]]` entry, the rest of
    /// `[prices]`, then, for `hosted` models (paid per token or by plan, not
    /// local servers), the built-in list. `None`: unknown here (the price
    /// catalog may know it).
    pub fn price_with_source(
        &self,
        model: &ModelRef,
        hosted: bool,
    ) -> Option<(Price, tessera_proto::PriceSource)> {
        use tessera_proto::PriceSource;
        let full = format!("{}/{}", model.provider, model.model);
        if let Some(p) = self.prices.get(&full).or_else(|| {
            self.providers
                .iter()
                .find(|p| p.id == model.provider)
                .and_then(|p| p.models.iter().find(|m| m.id == model.model))
                .and_then(|m| m.price.as_ref())
        }) {
            return Some((*p, PriceSource::Custom));
        }
        // OpenRouter-style ids (`anthropic/claude-…`) match on their last part.
        let short = model.model.rsplit('/').next().unwrap_or(&model.model);
        let names = [full.as_str(), model.model.as_str(), short];
        if let Some(p) = lookup_price(self.prices.iter().map(|(k, v)| (k.as_str(), v)), &names) {
            return Some((p, PriceSource::Custom));
        }
        hosted
            .then(|| lookup_price(DEFAULT_PRICES.iter().map(|(k, v)| (*k, v)), &names))
            .flatten()
            .map(|p| (p, PriceSource::Builtin))
    }

    /// `[tuning]` for a model: every key matching `provider/model`, the
    /// model id or (for OpenRouter-style ids) its last part, the more
    /// specific ones over the others, field by field.
    pub fn tuning_for(&self, model: &ModelRef) -> ModelTuning {
        let full = format!("{}/{}", model.provider, model.model);
        let short = model.model.rsplit('/').next().unwrap_or(&model.model);
        let names = [full.as_str(), model.model.as_str(), short];
        let mut matching: Vec<(&String, &ModelTuning)> = self
            .tuning
            .iter()
            .filter(|(k, _)| names.iter().any(|n| glob_match(k, n)))
            .collect();
        matching.sort_by_key(|(k, _)| (!k.contains('*'), k.len()));
        let mut out = ModelTuning::default();
        for (_, t) in matching {
            out.overlay(t);
        }
        out
    }
}

/// Write via temp file + rename so readers never see a partial file.
pub fn write_atomic(path: &Path, data: &[u8], private: bool) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        #[cfg(windows)]
        if private {
            crate::platform::private_file::create_dir_all(parent)?;
        } else {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(not(windows))]
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(windows)]
    {
        crate::platform::atomic_write::write(path, data)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        // Private files are created owner-only, so they are never readable by
        // others, not even between creation and a later chmod.
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        {
            use std::io::Write;
            let mut f = opts.open(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_proto::Usage;

    #[test]
    fn extra_agent_environment_is_empty_unless_the_user_names_it() {
        let old: Config = toml::from_str("[agent]\nmax_steps = 20\n").unwrap();
        assert!(old.agent.pass_env.is_empty());
        let chosen: Config = toml::from_str("[agent]\npass_env = ['BUILD_CACHE_DIR']\n").unwrap();
        assert_eq!(chosen.agent.pass_env, ["BUILD_CACHE_DIR"]);
        let again: Config = toml::from_str(&toml::to_string(&chosen).unwrap()).unwrap();
        assert_eq!(again.agent.pass_env, chosen.agent.pass_env);
    }

    #[test]
    fn roundtrip_with_providers() {
        let toml_src = r#"
default_model = "local/qwen"
[server]
bind = "0.0.0.0:7878"
[[provider]]
id = "local"
kind = "openai_chat"
base_url = "http://127.0.0.1:1234/v1"
discover = true
[[provider.models]]
id = "qwen"
context_window = 262144
"#;
        let c: Config = toml::from_str(toml_src).unwrap();
        assert_eq!(c.providers.len(), 1);
        assert_eq!(c.providers[0].models[0].context_window, Some(262144));
        assert_eq!(c.agent.max_steps, 250);
        let again: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(again.providers, c.providers);
    }

    /// A config from before batch 16: no shared freedom default (new chats
    /// keep `agent.permission`, nothing is written back) and no lean start.
    #[test]
    fn older_configs_keep_their_freedom_and_tools() {
        let old: Config = toml::from_str(
            "default_model = \"a/b\"\n[agent]\npermission = \"read_only\"\n[security]\nask_outside = false\n",
        )
        .unwrap();
        assert_eq!(old.security.default_permission, None);
        assert_eq!(old.new_chat_permission(), PermissionMode::ReadOnly);
        assert!(!old.agent.lean_tools);
        let text = toml::to_string_pretty(&old).unwrap();
        assert!(!text.contains("default_permission"), "{text}");
        let mut set = old.clone();
        set.security.default_permission = Some(PermissionMode::Ask);
        let again: Config = toml::from_str(&toml::to_string_pretty(&set).unwrap()).unwrap();
        assert_eq!(again.security.default_permission, Some(PermissionMode::Ask));
        assert_eq!(again.new_chat_permission(), PermissionMode::Ask);
    }

    /// Automatic asks on any unsupported host, even with a saved YOLO
    /// default. Explicit Off preserves the owner's existing behavior.
    #[test]
    fn new_chats_ask_without_a_sandbox_until_explicit_off() {
        let mut c = Config::default();
        assert_eq!(c.shared_permission_on(false), None);
        assert_eq!(c.shared_permission_on(true), Some(PermissionMode::Ask));
        c.agent.permission = PermissionMode::ReadOnly;
        assert_eq!(c.shared_permission_on(true), Some(PermissionMode::ReadOnly));
        c.security.default_permission = Some(PermissionMode::Auto);
        assert_eq!(c.shared_permission_on(true), Some(PermissionMode::Ask));
        assert_eq!(c.shared_permission_on(false), Some(PermissionMode::Auto));
        c.security.sandbox = SandboxMode::Off;
        assert_eq!(c.shared_permission_on(true), Some(PermissionMode::Auto));
        c.security.default_permission = None;
        assert_eq!(c.shared_permission_on(true), None);
        // Nothing is written for the fallback: choosing later still works.
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!text.contains("default_permission"), "{text}");
    }

    /// Keeping a Windows computer awake is on unless turned off, and only
    /// "off" is written down.
    #[test]
    fn keep_awake_is_on_unless_turned_off() {
        let c = Config::default();
        assert!(c.server.keep_awake);
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(!text.contains("keep_awake"), "{text}");
        let off: Config = toml::from_str("[server]\nkeep_awake = false\n").unwrap();
        assert!(!off.server.keep_awake);
        let again: Config = toml::from_str(&toml::to_string_pretty(&off).unwrap()).unwrap();
        assert!(!again.server.keep_awake);
    }

    #[test]
    fn model_roles_chains_cycles_and_fallbacks() {
        let c: Config = toml::from_str(
            r#"
default_model = "old/top"
utility_model = "local/qwen"
[models]
smart = "cloud/big"
vision = "@smart"
loop-a = "@loop-b"
loop-b = "@loop-a"
[models.fallback]
default = ["cloud/backup", "@fast"]
smart = ["@utility"]
"#,
        )
        .unwrap();
        assert_eq!(c.role("default").unwrap(), "old/top");
        assert_eq!(c.role("@vision").unwrap(), "cloud/big");
        // `fast` and `utility` are one role unless both are set.
        assert_eq!(c.role("fast").unwrap(), "local/qwen");
        assert_eq!(c.utility_model().unwrap().to_string(), "local/qwen");
        let cycle = c.role("loop-a").unwrap_err();
        assert!(
            cycle.contains("cycle") && cycle.contains("@loop-a → @loop-b → @loop-a"),
            "{cycle}"
        );
        assert!(c.role("nope").unwrap_err().contains("@smart"));
        assert_eq!(c.fallback_chain("old/top"), vec!["cloud/backup", "@fast"]);
        assert_eq!(c.fallback_chain("cloud/big"), vec!["@utility"]);
        // A model playing no role falls back like the default.
        assert_eq!(c.fallback_chain("x/y"), vec!["cloud/backup", "@fast"]);
        // `[models] default` wins over the top-level alias.
        let mut c2 = c.clone();
        c2.models.roles.insert("default".into(), "@smart".into());
        assert_eq!(c2.default_model().unwrap().to_string(), "cloud/big");
        // Survives a save/load round trip.
        let again: Config = toml::from_str(&toml::to_string_pretty(&c2).unwrap()).unwrap();
        assert_eq!(again.models, c2.models);
        // Old configs (no [models]) still parse and behave as before.
        let old: Config = toml::from_str("default_model = \"a/b\"\n").unwrap();
        assert_eq!(old.default_model().unwrap().to_string(), "a/b");
        assert!(old.utility_model().is_none() && old.fallback_chain("a/b").is_empty());
        assert_eq!(old.web.search, "auto");
    }

    #[test]
    fn the_memory_role_falls_back_to_utility_and_roles_can_be_set_and_cleared() {
        let mut c: Config = toml::from_str("default_model = \"a/chat\"\n").unwrap();
        // Nothing set: the review uses the chat's own model.
        assert!(c.memory_model().is_none());
        c.set_role("fast", Some("local/small")).unwrap();
        assert_eq!(c.models.roles.get("utility").unwrap(), "local/small");
        assert_eq!(c.memory_model().unwrap().to_string(), "local/small");
        c.set_role("@memory", Some("cloud/careful")).unwrap();
        assert_eq!(c.memory_model().unwrap().to_string(), "cloud/careful");
        // A role may name another role, but not in a cycle.
        c.set_role("memory", Some("@utility")).unwrap();
        assert_eq!(c.memory_model().unwrap().to_string(), "local/small");
        c.set_role("utility", Some("@memory")).unwrap_err();
        assert_eq!(c.role_setting("utility").as_deref(), Some("local/small"));
        // Bad values and names are refused.
        assert!(c.set_role("memory", Some("no-slash")).is_err());
        assert!(c.set_role("fallback", Some("a/b")).is_err());
        assert!(c.set_role("x y", Some("a/b")).is_err());
        // Clearing: empty or `None`; `default` is the top-level setting.
        c.set_role("memory", Some(" ")).unwrap();
        c.set_role("utility", None).unwrap();
        assert!(c.memory_model().is_none() && c.utility_model().is_none());
        c.set_role("default", Some("b/other")).unwrap();
        assert_eq!(c.default_model.as_deref(), Some("b/other"));
        c.set_role("research", Some("@default")).unwrap();
        assert_eq!(c.role("research").unwrap(), "b/other");
        let again: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(again.models, c.models);
        assert!(!c.models.roles.contains_key("memory"));
    }

    #[test]
    fn prices_follow_explicit_then_table_then_defaults() {
        let c: Config = toml::from_str(
            r#"
[prices]
"claude-sonnet-*" = { input = 2.0, output = 10.0 }
"local/qwen" = { input = 0.0, output = 0.0 }
[[provider]]
id = "p"
kind = "openai_chat"
[[provider.models]]
id = "special"
price = { input = 1.0, output = 2.0, cache_read = 0.5 }
"#,
        )
        .unwrap();
        let m = |s: &str| s.parse::<ModelRef>().unwrap();
        let usage = Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            ..Default::default()
        };
        let p = c.price(&m("p/special"), false).unwrap();
        assert!((p.cost(&usage) - (1.0 + 1.0 + 1.0)).abs() < 1e-9);
        // The user's table beats the built-in list, even for unbilled providers.
        assert_eq!(
            c.price(&m("x/claude-sonnet-4-5"), false).unwrap().input,
            2.0
        );
        assert_eq!(c.price(&m("local/qwen"), false).unwrap().output, 0.0);
        // Built-in list prices only for hosted models (not local servers).
        assert!(c.price(&m("openai/gpt-4o-mini"), false).is_none());
        assert_eq!(c.price(&m("openai/gpt-4o-mini"), true).unwrap().input, 0.15);
        assert_eq!(
            c.price(&m("router/openai/gpt-4o"), true).unwrap().input,
            2.5
        );
        assert!(c.price(&m("openai/gpt-6-astra"), true).is_none());
        assert!(glob_match("a*c*e", "abcde") && !glob_match("a*c", "abcd") && glob_match("*", "x"));
    }

    #[test]
    fn built_in_claude_prices_and_where_prices_come_from() {
        use tessera_proto::PriceSource;
        let mut c: Config = toml::from_str(
            r#"
[[provider]]
id = "p"
kind = "openai_chat"
[[provider.models]]
id = "special"
price = { input = 1.0, output = 2.0 }
"#,
        )
        .unwrap();
        let m = |s: &str| s.parse::<ModelRef>().unwrap();
        let (p, src) = c
            .price_with_source(&m("anthropic/claude-opus-5-5"), true)
            .unwrap();
        assert_eq!((p.input, p.output, src), (4.0, 20.0, PriceSource::Builtin));
        // A million tokens each of input, output, cache read and cache write.
        let all = |p: Price| p.cost_of(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((all(p) - (4.0 + 20.0 + 0.2 + 5.0)).abs() < 1e-9);
        let fable = c.price(&m("anthropic/claude-fable-5-1"), true).unwrap();
        assert!((all(fable) - (10.0 + 50.0 + 0.25 + 12.5)).abs() < 1e-9);
        // Fable 5 reads its cache at the usual tenth; dated ids match too.
        assert_eq!(
            c.price(&m("anthropic/claude-fable-5"), true)
                .unwrap()
                .cache_read,
            Some(1.0)
        );
        assert_eq!(
            c.price(&m("anthropic/claude-haiku-4-5-20251001"), true)
                .unwrap()
                .input,
            1.0
        );
        assert_eq!(
            c.price(&m("anthropic/claude-sonnet-5-5"), true)
                .unwrap()
                .output,
            10.0
        );
        // A newer minor version isn't guessed from an older one.
        assert!(c.price(&m("anthropic/claude-opus-5-9"), true).is_none());
        assert_eq!(
            c.price(&m("openai/gpt-5.1-codex-mini"), true)
                .unwrap()
                .input,
            0.25
        );
        // What the apps set (`[prices]."provider/model"`) beats everything.
        assert_eq!(
            c.price_with_source(&m("p/special"), false).unwrap().1,
            PriceSource::Custom
        );
        c.prices.insert(
            "p/special".into(),
            Price {
                input: 9.0,
                ..Default::default()
            },
        );
        c.prices.insert(
            "anthropic/claude-opus-5-5".into(),
            Price::new(1.0, 1.0, 0.0, 0.0),
        );
        assert_eq!(c.price(&m("p/special"), false).unwrap().input, 9.0);
        let (p, src) = c
            .price_with_source(&m("anthropic/claude-opus-5-5"), true)
            .unwrap();
        assert_eq!((p.input, src), (1.0, PriceSource::Custom));
        // `[pricing]` defaults to the catalog and round-trips plans.
        assert!(c.pricing.catalog && c.pricing.catalog_url == CATALOG_URL);
        c.pricing.subscriptions.insert(
            "openai-codex".into(),
            SubscriptionConfig {
                plan: Some("ChatGPT Pro".into()),
                monthly_usd: Some(200.0),
            },
        );
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(!text.contains("catalog_url"), "{text}");
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(again.pricing, c.pricing);
        assert_eq!(again.prices, c.prices);
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_and_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().join("home"));
        paths.ensure().unwrap();
        Config::default().save(&paths).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&paths.config_file()), 0o600);
        assert_eq!(mode(&paths.data_dir), 0o700);
    }
}
