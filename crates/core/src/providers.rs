//! Provider registry: configured endpoints, credentials, model discovery and
//! dispatch to the right wire adapter.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tessera_proto::{
    AuthMethod, Event, ModelInfo, ModelRef, ProviderInfo, ProviderKind, ThinkingLevel,
};
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthStore, CODEX_BASE_URL, CODEX_PROVIDER, Credential};
use crate::config::{ModelConfig, ProviderConfig};
use crate::llm::openai_chat::Listed;
use crate::llm::window::{self, Server, Window};
use crate::llm::{self, ErrorKind, EventTx, LlmError, LlmRequest, LlmResponse};
use crate::thinking::TemplateCaps;

mod daybreak;

/// llama.cpp's flags for the model file.
const MODEL_FLAGS: &[&str] = &["-m", "--model"];

/// Where the model lists servers gave are kept, beside the template controls.
const LISTS_FILE: &str = "model-lists.json";

/// A server's model list as last read, and from which address.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedList {
    base_url: String,
    models: Vec<ModelInfo>,
}

struct ProviderState {
    cfg: ProviderConfig,
    discovered: Vec<ModelInfo>,
    /// Program discovery belongs to these credentials, never to a disk cache.
    catalog_scope: Option<[u8; 32]>,
    /// Chat-template thinking controls of local models, by model id: read
    /// while the model was loaded, and kept (see [`Providers::remembering`]).
    caps: HashMap<String, KnownCaps>,
    /// What a local server listed about each model (llama.cpp: how it runs it).
    listed: HashMap<String, Listed>,
    /// The context window the user's own server runs each model with, as it
    /// said (see `llm/window.rs`).
    windows: HashMap<String, Window>,
    /// The kind of server that answered for its windows (Ollama, LM Studio):
    /// not llama.cpp, so no `/props` is asked of it.
    window_server: Option<Server>,
    /// A server on this computer or the network didn't answer the last
    /// time it was asked: its models are the ones it listed before.
    offline: bool,
}

impl ProviderState {
    fn new(cfg: ProviderConfig) -> Self {
        Self {
            cfg,
            discovered: Vec::new(),
            catalog_scope: None,
            caps: HashMap::new(),
            listed: HashMap::new(),
            windows: HashMap::new(),
            window_server: None,
            offline: false,
        }
    }

    /// The window the server says it runs `id` with, when it said.
    fn window_tokens(&self, id: &str) -> Option<u64> {
        self.windows.get(id).map(|w| w.tokens)
    }

    /// Why `id`'s window is too small for Tessera, in plain words, with how
    /// to raise it. Never when the user set the window themselves in the
    /// provider's models.
    fn window_warning(&self, id: &str) -> Option<String> {
        if self
            .cfg
            .models
            .iter()
            .any(|mc| mc.id == id && mc.context_window.is_some())
        {
            return None;
        }
        self.windows.get(id).and_then(window::warning)
    }

    /// A local model's template controls, unless the server now runs it
    /// from another file than when they were read.
    fn template(&self, id: &str) -> Option<&TemplateCaps> {
        let k = self.caps.get(id)?;
        let now = self.listed.get(id).and_then(|l| l.arg(MODEL_FLAGS));
        (k.file.is_none() || now.is_none() || k.file.as_deref() == now).then_some(&k.caps)
    }

    /// The template controls to go by for a local model: its own, else, for
    /// a llama.cpp model of a thinking family not seen loaded yet, those of
    /// the model it was made from (`qwen3.8-27b` for `qwen3.8-27b-uncensored`)
    /// as a guess (`true`).
    fn controls(&self, id: &str) -> Option<(&TemplateCaps, bool)> {
        if let Some(c) = self.template(id).filter(|c| has_controls(c)) {
            return Some((c, false));
        }
        if !self.listed.get(id).is_some_and(|l| l.llama)
            || default_info(&self.cfg, id, None).thinking.is_empty()
        {
            return None;
        }
        let base = self
            .caps
            .keys()
            .filter(|k| {
                id.strip_prefix(k.as_str())
                    .is_some_and(|rest| rest.starts_with(['-', '_', '.', ':']))
            })
            .max_by_key(|k| k.len())?;
        self.template(base)
            .filter(|c| has_controls(c))
            .map(|c| (c, true))
    }

    /// The family a local model's chat template shows, as last read.
    fn template_family(&self, id: &str) -> Option<&str> {
        self.template(id).and_then(|c| c.family.as_deref())
    }

    /// A local model's modes from its chat template; for a llama.cpp model
    /// whose template hasn't been seen yet, the guess says so.
    fn local_thinking(&self, m: &mut ModelInfo) {
        let listed = self.listed.get(&m.id);
        let server = listed.and_then(Listed::server_effort);
        match self.controls(&m.id) {
            Some((c, guess)) => {
                let (opts, def) = crate::thinking::from_template(c, server);
                m.reasoning = true;
                m.thinking = opts;
                m.default_thinking = def;
                if guess {
                    crate::thinking::provisional(&mut m.thinking, server);
                }
                let family = self.template_family(&m.id);
                recommended_effort(m, family);
            }
            None if listed.is_some_and(|l| l.llama) => {
                crate::thinking::provisional(&mut m.thinking, server)
            }
            None => {}
        }
    }
}

/// A local model's template controls, as last read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct KnownCaps {
    /// The model file the server ran then (llama.cpp router mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    caps: TemplateCaps,
}

fn has_controls(c: &TemplateCaps) -> bool {
    c.enable_thinking || !c.efforts.is_empty()
}

/// Worth keeping: thinking controls, or a family the id may not show.
fn worth_keeping(c: &TemplateCaps) -> bool {
    has_controls(c) || c.family.is_some()
}

/// The reasoning effort a model's maker recommends as its default, where
/// the model offers it (gpt-oss: medium; Qwen3.8 keeps its own).
fn recommended_effort(m: &mut ModelInfo, template: Option<&str>) {
    if let Some(e) = crate::profiles::profile(&m.id, template).effort
        && m.thinking.iter().any(|o| o.level == e)
    {
        m.default_thinking = e;
    }
}

/// A local OpenAI-compatible server (llama.cpp and the like), not a hosted API.
fn local_chat(cfg: &ProviderConfig) -> bool {
    cfg.kind == ProviderKind::OpenaiChat
        && !cfg.base_url.contains("openrouter.ai")
        && !cfg.base_url.contains("api.openai.com")
}

/// A server on this machine or the local network (llama.cpp, Ollama, LM
/// Studio…), not a hosted API.
fn on_this_network(base_url: &str) -> bool {
    url::Url::parse(base_url).map_or(true, |u| crate::tools::web::internal_url(&u))
}

/// A model server of the user's own (on this computer or the network),
/// which may simply be switched off.
fn own_server(cfg: &ProviderConfig) -> bool {
    local_chat(cfg) && on_this_network(&cfg.base_url)
}

/// The server a base URL names, for its queue: scheme, host and port
/// (`http://192.168.1.50:1234/v1` and `http://192.168.1.50:1234` are one).
fn server_address(base_url: &str) -> String {
    match url::Url::parse(base_url) {
        Ok(u) => format!(
            "{}://{}:{}",
            u.scheme(),
            u.host_str().unwrap_or_default(),
            u.port_or_known_default().unwrap_or_default()
        ),
        Err(_) => base_url.trim_end_matches('/').to_ascii_lowercase(),
    }
}

/// Where a server is, as people say it: `192.168.1.20:8080`, `localhost:1234`.
fn server_place(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host,
            })
        })
        .unwrap_or_else(|| base_url.to_string())
}

/// What a turn says when a model server of the user's doesn't answer.
fn not_answering(cfg: &ProviderConfig) -> String {
    let name = if cfg.name.is_empty() {
        &cfg.id
    } else {
        &cfg.name
    };
    format!("{name} isn't answering at {}", server_place(&cfg.base_url))
}

/// One provider's discovery.
#[derive(Default)]
struct Found {
    models: Vec<ModelInfo>,
    catalog_scope: Option<[u8; 32]>,
    /// Template controls read just now (`None`: the template has none).
    caps: Vec<(String, Option<KnownCaps>)>,
    listed: HashMap<String, Listed>,
    windows: HashMap<String, Window>,
    window_server: Option<Server>,
}

pub struct Providers {
    http: reqwest::Client,
    auth: Arc<AuthStore>,
    state: RwLock<Vec<ProviderState>>,
    /// What the user's Claude Code said about itself, by `claude` command
    /// (the engine asks only when needed and keeps the answer).
    claude: RwLock<HashMap<String, crate::claude_code::Detection>>,
    /// Where template controls are kept between runs.
    caps_file: Option<PathBuf>,
    /// Told when a model's modes change outside a discovery.
    events: Option<tokio::sync::broadcast::Sender<Event>>,
    /// Local models whose template this run has asked for (provider, model).
    probed: Mutex<HashSet<(String, String)>>,
    /// When a local model's window was last read after a reply.
    window_read: Mutex<HashMap<(String, String), tokio::time::Instant>>,
    /// Chats already told that a model's window is too small (session,
    /// model, window).
    window_told: Mutex<HashSet<(String, String, u64)>>,
    /// The thinking level the user last picked, by `provider/model`.
    chosen: RwLock<HashMap<String, ThinkingLevel>>,
    /// Subscriptions' usage limits as last reported with a reply.
    limits: crate::limits::PlanLimits,
    /// When servers marked offline were last asked again.
    rechecked: Mutex<Option<std::time::Instant>>,
    /// Where Claude accounts besides the usual one keep their Claude Code
    /// folders (`Paths::claude_accounts_dir`).
    claude_root: Option<PathBuf>,
    /// The queue of each server that answers one request at a time, by its
    /// address (see [`Providers::turn_on`]).
    queues: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
}

pub fn codex_preset() -> ProviderConfig {
    ProviderConfig {
        id: CODEX_PROVIDER.into(),
        name: "ChatGPT (subscription)".into(),
        kind: ProviderKind::OpenaiCodex,
        base_url: CODEX_BASE_URL.into(),
        discover: true,
        // Choices after sign-in, never a source of unauthenticated defaults.
        // The live list replaces these once the account answers.
        models: [
            ("gpt-6-astra", 272_000),
            ("gpt-5.6-sol", 1_000_000),
            ("gpt-5.5", 272_000),
        ]
        .iter()
        .map(|(id, ctx)| ModelConfig {
            id: (*id).into(),
            name: None,
            context_window: Some(*ctx),
            max_output: Some(128_000),
            reasoning: Some(true),
            vision: Some(true),
            price: None,
            fast: None,
        })
        .collect(),
        headers: Default::default(),
        reasoning_replay: Default::default(),
        parallel: None,
    }
}

impl Providers {
    pub fn new(http: reqwest::Client, auth: Arc<AuthStore>, configured: &[ProviderConfig]) -> Self {
        let mut list: Vec<ProviderState> = configured
            .iter()
            .map(|c| ProviderState::new(c.clone()))
            .collect();
        if !list.iter().any(|p| p.cfg.id == CODEX_PROVIDER) {
            list.push(ProviderState::new(codex_preset()));
        }
        // The user's own Claude Code, listed once `claude` is found.
        if !list
            .iter()
            .any(|p| p.cfg.id == crate::claude_code::PROVIDER)
        {
            list.push(ProviderState::new(crate::claude_code::preset()));
        }
        Self {
            http,
            auth,
            state: RwLock::new(list),
            claude: Default::default(),
            caps_file: None,
            events: None,
            probed: Default::default(),
            window_read: Default::default(),
            window_told: Default::default(),
            chosen: Default::default(),
            limits: Default::default(),
            rechecked: Default::default(),
            claude_root: None,
            queues: Default::default(),
        }
    }

    /// Claude accounts besides the usual one keep their Claude Code folders in `dir`.
    pub fn with_claude_accounts(mut self, dir: PathBuf) -> Self {
        self.claude_root = Some(dir);
        self
    }

    /// The Claude account a `claude_code` provider runs as.
    pub fn claude_account(&self, cfg: &ProviderConfig) -> crate::claude_code::Account {
        crate::claude_code::Account::of(&cfg.id, &cfg.base_url, self.claude_root.as_deref())
    }

    /// What is known about a `claude_code` provider's account.
    fn claude_of(&self, cfg: &ProviderConfig) -> Option<crate::claude_code::Detection> {
        self.claude_code(&self.claude_account(cfg).key())
    }

    /// Keep subscriptions' usage limits in `file` (`plan_limits`).
    pub fn keeping_limits(mut self, file: PathBuf) -> Self {
        self.limits = crate::limits::PlanLimits::load(file);
        self
    }

    /// Subscriptions' usage limits, as their providers last reported them.
    pub fn plan_limits(&self) -> &crate::limits::PlanLimits {
        &self.limits
    }

    /// Keep local models' template controls in `file`, so a model the
    /// server has unloaded keeps its real modes, and tell `events` when a
    /// model's modes change after a reply from it.
    pub fn remembering(
        mut self,
        file: PathBuf,
        events: tokio::sync::broadcast::Sender<Event>,
    ) -> Self {
        if let Ok(bytes) = std::fs::read(&file) {
            match serde_json::from_slice::<HashMap<String, HashMap<String, KnownCaps>>>(&bytes) {
                Ok(mut saved) => {
                    for p in self.state.get_mut() {
                        p.caps = saved.remove(&p.cfg.id).unwrap_or_default();
                    }
                }
                Err(e) => tracing::debug!("ignoring {}: {e}", file.display()),
            }
        }
        // The model lists last read from servers that list their models:
        // shown at once, also while a server is off (a PC asleep, a router
        // restarting), until it answers again.
        let lists = file.with_file_name(LISTS_FILE);
        if let Ok(bytes) = std::fs::read(&lists) {
            match serde_json::from_slice::<HashMap<String, SavedList>>(&bytes) {
                Ok(mut saved) => {
                    for p in self.state.get_mut() {
                        if let Some(l) = saved.remove(&p.cfg.id)
                            && l.base_url == p.cfg.base_url
                            && p.cfg.discover
                            && p.cfg.kind != ProviderKind::ClaudeCode
                        {
                            p.discovered = l.models;
                        }
                    }
                }
                Err(e) => tracing::debug!("ignoring {}: {e}", lists.display()),
            }
        }
        self.caps_file = Some(file);
        self.events = Some(events);
        self
    }

    /// Keep the model lists servers gave (see [`remembering`](Self::remembering)).
    fn save_lists(&self) {
        let Some(file) = &self.caps_file else { return };
        let file = file.with_file_name(LISTS_FILE);
        let json = {
            let state = self.state.read();
            let all: BTreeMap<&String, SavedList> = state
                .iter()
                .filter(|p| p.cfg.discover && !p.discovered.is_empty())
                .map(|p| {
                    (
                        &p.cfg.id,
                        SavedList {
                            base_url: p.cfg.base_url.clone(),
                            models: p.discovered.clone(),
                        },
                    )
                })
                .collect();
            serde_json::to_vec_pretty(&all)
        };
        let saved = json
            .map_err(anyhow::Error::from)
            .and_then(|json| crate::config::write_atomic(&file, &json, false));
        if let Err(e) = saved {
            tracing::warn!("could not save {}: {e}", file.display());
        }
    }

    fn save_caps(&self) {
        let Some(file) = &self.caps_file else { return };
        let json = {
            let state = self.state.read();
            let all: BTreeMap<&String, BTreeMap<&String, &KnownCaps>> = state
                .iter()
                .filter(|p| !p.caps.is_empty())
                .map(|p| (&p.cfg.id, p.caps.iter().collect()))
                .collect();
            serde_json::to_vec_pretty(&all)
        };
        let saved = json
            .map_err(anyhow::Error::from)
            .and_then(|json| crate::config::write_atomic(file, &json, false));
        if let Err(e) = saved {
            tracing::warn!("could not save {}: {e}", file.display());
        }
    }

    /// The thinking levels the user picked, by `provider/model` (from the store).
    pub fn set_chosen_thinking(&self, levels: HashMap<String, ThinkingLevel>) {
        *self.chosen.write() = levels;
    }

    /// Remember `level` as the user's choice for `m`; all of them when that
    /// changed anything (to save).
    pub fn choose_thinking(
        &self,
        m: &ModelRef,
        level: ThinkingLevel,
    ) -> Option<HashMap<String, ThinkingLevel>> {
        let mut c = self.chosen.write();
        (c.insert(m.to_string(), level) != Some(level)).then(|| c.clone())
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn configs(&self) -> Vec<ProviderConfig> {
        self.state.read().iter().map(|p| p.cfg.clone()).collect()
    }

    /// Configs worth persisting (the built-in ChatGPT and Claude Code
    /// entries are implicit).
    pub fn persisted_configs(&self) -> Vec<ProviderConfig> {
        self.configs()
            .into_iter()
            .filter(|c| !(c.id == CODEX_PROVIDER && *c == codex_preset()))
            .filter(|c| {
                !(c.id == crate::claude_code::PROVIDER && *c == crate::claude_code::preset())
            })
            .collect()
    }

    /// Models a provider reported some other way (Claude Code lists its
    /// models when a turn starts).
    pub fn set_discovered(&self, id: &str, models: Vec<ModelInfo>) -> bool {
        let mut s = self.state.write();
        match s.iter_mut().find(|p| p.cfg.id == id) {
            Some(p) if p.discovered != models => {
                p.discovered = models;
                true
            }
            _ => false,
        }
    }

    /// What is known about a Claude account (by `Account::key`).
    pub fn claude_code(&self, key: &str) -> Option<crate::claude_code::Detection> {
        self.claude.read().get(key.trim()).cloned()
    }

    /// Everything known about Claude Code, by command.
    pub fn claude_code_all(
        &self,
    ) -> std::collections::HashMap<String, crate::claude_code::Detection> {
        self.claude.read().clone()
    }

    /// Record what Claude Code said about an account (by `Account::key`);
    /// `true` when that changed anything.
    pub fn set_claude_code(&self, key: &str, d: crate::claude_code::Detection) -> bool {
        self.claude
            .write()
            .insert(key.trim().to_string(), d.clone())
            != Some(d)
    }

    /// The provider is a server on this computer or the local network
    /// (llama.cpp and the like), by its address.
    pub fn on_local_network(&self, id: &str) -> bool {
        self.configs()
            .into_iter()
            .find(|c| c.id == id)
            .is_some_and(|c| on_this_network(&c.base_url))
    }

    /// A provider's kind, if it exists.
    pub fn kind_of(&self, id: &str) -> Option<ProviderKind> {
        self.state
            .read()
            .iter()
            .find(|p| p.cfg.id == id)
            .map(|p| p.cfg.kind)
    }

    pub fn upsert(&self, cfg: ProviderConfig) {
        let mut s = self.state.write();
        match s.iter_mut().find(|p| p.cfg.id == cfg.id) {
            Some(p) => {
                if p.cfg.base_url != cfg.base_url {
                    // Another server: what was read about its models is moot.
                    p.caps.clear();
                    p.listed.clear();
                    p.windows.clear();
                    p.window_server = None;
                    p.offline = false;
                }
                p.cfg = cfg;
                p.discovered.clear();
            }
            None => s.push(ProviderState::new(cfg)),
        }
    }

    /// A provider's name, nothing else (its models stay as listed).
    pub fn rename(&self, id: &str, name: &str) -> bool {
        let mut s = self.state.write();
        match s.iter_mut().find(|p| p.cfg.id == id) {
            Some(p) => {
                p.cfg.name = name.to_string();
                true
            }
            None => false,
        }
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut s = self.state.write();
        let before = s.len();
        s.retain(|p| p.cfg.id != id);
        before != s.len()
    }

    pub fn exists(&self, id: &str) -> bool {
        self.state.read().iter().any(|p| p.cfg.id == id)
    }

    fn authenticated(&self, cfg: &ProviderConfig) -> bool {
        match cfg.kind {
            ProviderKind::OpenaiCodex => {
                matches!(self.auth.get(&cfg.id), Some(Credential::Oauth { .. }))
            }
            ProviderKind::Anthropic | ProviderKind::OpenaiResponses => {
                self.auth.get(&cfg.id).is_some() || !cfg.headers.is_empty()
            }
            // Only local/private endpoints may be keyless. Hosted Chat APIs
            // (including OpenRouter) must not look ready just from a preset.
            ProviderKind::OpenaiChat => {
                own_server(cfg) || self.auth.get(&cfg.id).is_some() || !cfg.headers.is_empty()
            }
            // Signed in with Claude Code itself (never here).
            ProviderKind::ClaudeCode => self.claude_of(cfg).is_some_and(|d| d.signed_in),
        }
    }

    pub fn infos(&self) -> Vec<ProviderInfo> {
        let s = self.state.read();
        let chosen = self.chosen.read();
        s.iter()
            // Claude Code only where it is installed.
            .filter(|p| {
                p.cfg.kind != ProviderKind::ClaudeCode
                    || self.claude_of(&p.cfg).is_some_and(|d| d.path.is_some())
            })
            .map(|p| {
                let cred = self.auth.get(&p.cfg.id);
                ProviderInfo {
                    id: p.cfg.id.clone(),
                    name: if p.cfg.name.is_empty() {
                        p.cfg.id.clone()
                    } else {
                        p.cfg.name.clone()
                    },
                    kind: p.cfg.kind,
                    base_url: p.cfg.base_url.clone(),
                    authenticated: self.authenticated(&p.cfg),
                    auth: match (&p.cfg.kind, &cred) {
                        (ProviderKind::OpenaiCodex | ProviderKind::ClaudeCode, _)
                        | (_, Some(Credential::Oauth { .. })) => AuthMethod::Oauth,
                        (_, Some(Credential::ApiKey { .. })) => AuthMethod::ApiKey,
                        (ProviderKind::OpenaiChat, None) => AuthMethod::None,
                        _ => AuthMethod::ApiKey,
                    },
                    account: match cred {
                        Some(Credential::Oauth { email, .. }) => email,
                        _ if p.cfg.kind == ProviderKind::ClaudeCode => {
                            self.claude_of(&p.cfg).and_then(|d| d.account)
                        }
                        _ => None,
                    },
                    models: merged_models(p, &chosen)
                        .into_iter()
                        .map(|mut m| {
                            self.scope_programs(p, &mut m);
                            m
                        })
                        .collect(),
                    offline: p.offline,
                }
            })
            .collect()
    }

    /// Some model server of the user's isn't answering.
    pub fn any_offline(&self) -> bool {
        self.state.read().iter().any(|p| p.offline)
    }

    /// Mark a provider's server as answering or not; `true` when that
    /// changed (clients are told then, when this runs with events).
    fn set_offline(&self, id: &str, offline: bool) -> bool {
        let changed = self
            .state
            .write()
            .iter_mut()
            .find(|p| p.cfg.id == id)
            .is_some_and(|p| std::mem::replace(&mut p.offline, offline) != offline);
        if changed && let Some(events) = &self.events {
            let _ = events.send(Event::ProvidersChanged);
        }
        changed
    }

    /// Ask the model servers marked offline again (not more often than
    /// every `gap`); `true` when one answers now.
    pub async fn recheck_offline(&self, gap: std::time::Duration) -> bool {
        let ids: Vec<String> = {
            let mut last = self.rechecked.lock();
            if last.is_some_and(|t| t.elapsed() < gap) {
                return false;
            }
            let ids: Vec<String> = self
                .state
                .read()
                .iter()
                .filter(|p| p.offline)
                .map(|p| p.cfg.id.clone())
                .collect();
            if !ids.is_empty() {
                *last = Some(std::time::Instant::now());
            }
            ids
        };
        futures::future::join_all(ids.iter().map(|id| self.discover(Some(id)))).await;
        let s = self.state.read();
        ids.iter()
            .any(|id| s.iter().any(|p| p.cfg.id == *id && !p.offline))
    }

    /// Whether requests to `provider` carry the model's output limit: a
    /// local server otherwise lets a model stuck in a loop write until its
    /// context is full (hosted APIs have their own limit, and some reject
    /// the field for reasoning models).
    pub fn bounds_output(&self, provider: &str) -> bool {
        self.state
            .read()
            .iter()
            .any(|p| p.cfg.id == provider && local_chat(&p.cfg) && on_this_network(&p.cfg.base_url))
    }

    /// The provider is a model server of the user's own (llama.cpp, Ollama,
    /// LM Studio, vLLM… on this computer or the network).
    pub fn own_server(&self, provider: &str) -> bool {
        self.bounds_output(provider)
    }

    /// Whether `m`'s endpoint honours `tool_choice: none`, so a request whose
    /// model may not call tools (a subagent writing its report) can keep
    /// them listed and the prompt cached: Anthropic, OpenAI and OpenRouter,
    /// and llama.cpp (it then renders the same prompt, tools included).
    /// Other servers get no tools instead, as before.
    pub fn takes_tool_choice_none(&self, m: &ModelRef) -> bool {
        let s = self.state.read();
        let Some(p) = s.iter().find(|p| p.cfg.id == m.provider) else {
            return false;
        };
        match p.cfg.kind {
            ProviderKind::Anthropic | ProviderKind::OpenaiResponses => true,
            ProviderKind::OpenaiChat => {
                !local_chat(&p.cfg) || p.listed.get(&m.model).is_some_and(|l| l.llama)
            }
            ProviderKind::OpenaiCodex | ProviderKind::ClaudeCode => false,
        }
    }

    /// `m` runs on a server on this computer or the local network that
    /// answers one request at a time (llama.cpp with one slot, as one user
    /// usually runs it; Ollama; LM Studio): anything else sent to it while a
    /// chat's turn runs makes the turn wait, and pushes the chat's cached
    /// prompt out so its next step reads it all again. Unless a llama.cpp
    /// router says it runs the model with several slots (`--parallel`).
    pub fn one_at_a_time(&self, m: &ModelRef) -> bool {
        let s = self.state.read();
        let Some(p) = s.iter().find(|p| p.cfg.id == m.provider) else {
            return false;
        };
        if !(local_chat(&p.cfg) && on_this_network(&p.cfg.base_url)) {
            return false;
        }
        if let Some(n) = p.cfg.parallel {
            return n <= 1;
        }
        let listed = p.listed.get(&m.model);
        if listed.is_some_and(|l| l.batching) {
            return false;
        }
        let slots = listed
            .and_then(|l| l.arg(&["-np", "--parallel"]))
            .and_then(|v| v.parse::<i64>().ok());
        // -1: llama.cpp picks several itself.
        !slots.is_some_and(|n| n != 1)
    }

    /// The queue a request to `m` waits in when its server answers one
    /// request at a time ([`one_at_a_time`](Self::one_at_a_time)): one per
    /// server address, so every chat, helper and model name on it takes its
    /// turn in the order it asked. Sent alongside another, a request would
    /// only make that one wait, or be refused as busy (a local model mux
    /// answers "503 Local model is busy" after 30 s), and a chat on the screen
    /// gives up after its few retries. None: the server takes several at once.
    fn turn_on(&self, base_url: &str, m: &ModelRef) -> Option<Arc<tokio::sync::Semaphore>> {
        if !self.one_at_a_time(m) {
            return None;
        }
        let mut queues = self.queues.lock();
        Some(
            queues
                .entry(server_address(base_url))
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
                .clone(),
        )
    }

    pub fn model_info(&self, m: &ModelRef) -> Option<ModelInfo> {
        let s = self.state.read();
        let p = s.iter().find(|p| p.cfg.id == m.provider)?;
        let chosen = self.chosen.read();
        let mut info = merged_model(p, &chosen, &m.model).or_else(|| {
            // Unlisted model on a known provider: allow it with defaults so
            // users can type any id the endpoint accepts.
            let mut i = default_info(&p.cfg, &m.model, p.window_tokens(&m.model));
            p.local_thinking(&mut i);
            i.last_thinking = chosen.get(&m.to_string()).copied();
            i.context_warning = p.window_warning(&m.model);
            Some(i)
        })?;
        self.scope_programs(p, &mut info);
        Some(info)
    }

    /// The model accepts images (in messages and tool results).
    pub fn sees_images(&self, m: &ModelRef) -> bool {
        self.model_info(m).is_some_and(|i| i.vision)
    }

    /// First model whose provider is ready. Presets alone are not usable.
    pub fn first_model(&self) -> Option<ModelRef> {
        let infos = self.infos();
        infos.iter().filter(|p| p.authenticated).find_map(|p| {
            p.models.first().map(|m| ModelRef {
                provider: p.id.clone(),
                model: m.id.clone(),
            })
        })
    }

    /// Discover a provider's models once if it hasn't been yet, so the first
    /// request knows the real context window.
    pub async fn ensure_discovered(&self, id: &str) {
        let needed = self
            .state
            .read()
            .iter()
            .any(|p| p.cfg.id == id && p.cfg.discover && p.discovered.is_empty());
        if needed {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(10), self.discover(Some(id)))
                    .await;
        }
    }

    /// Query model lists from endpoints that support discovery.
    pub async fn discover(&self, only: Option<&str>) -> Vec<(String, String)> {
        let targets: Vec<ProviderConfig> = self
            .configs()
            .into_iter()
            // Claude Code isn't asked here (no `claude` runs at start-up):
            // the engine does when something needs it.
            .filter(|c| {
                c.discover
                    && c.kind != ProviderKind::ClaudeCode
                    && only.is_none_or(|o| o == c.id)
                    && self.authenticated(c)
            })
            .collect();
        let mut errors = Vec::new();
        let results = futures::future::join_all(
            targets
                .iter()
                .map(|cfg| async move { (cfg, self.discover_one(cfg).await) }),
        )
        .await;
        let mut saved = false;
        let mut lists = false;
        let mut programs_changed = false;
        for (cfg, res) in results {
            // A server of the user's that doesn't answer keeps the models it
            // listed before, marked offline; any answer clears that.
            let offline = res.as_ref().is_err_and(|e| e.unreachable) && own_server(cfg);
            if let Some(p) = self.state.write().iter_mut().find(|p| p.cfg.id == cfg.id) {
                p.offline = offline;
            }
            match res {
                Ok(found) if !found.models.is_empty() => {
                    if let Some(p) = self.state.write().iter_mut().find(|p| p.cfg.id == cfg.id) {
                        programs_changed |= p.catalog_scope != found.catalog_scope
                            || !p
                                .discovered
                                .iter()
                                .filter(|m| !m.cyber_access_programs.is_empty())
                                .map(|m| (&m.id, &m.cyber_access_programs))
                                .eq(found
                                    .models
                                    .iter()
                                    .filter(|m| !m.cyber_access_programs.is_empty())
                                    .map(|m| (&m.id, &m.cyber_access_programs)));
                        lists |=
                            p.discovered != found.models || p.catalog_scope != found.catalog_scope;
                        p.catalog_scope = found.catalog_scope;
                        p.discovered = found.models;
                        p.listed = found.listed;
                        p.windows = found.windows;
                        p.window_server = found.window_server;
                        for (id, c) in found.caps {
                            let before = p.caps.get(&id).cloned();
                            match c {
                                Some(c) => p.caps.insert(id.clone(), c),
                                None => p.caps.remove(&id),
                            };
                            saved |= before.as_ref() != p.caps.get(&id);
                        }
                    }
                }
                Ok(_) => {
                    programs_changed |= self.clear_programs(&cfg.id);
                }
                Err(e) => {
                    programs_changed |= self.clear_programs(&cfg.id);
                    errors.push((cfg.id.clone(), e.to_string()));
                }
            }
        }
        if saved {
            self.save_caps();
        }
        if lists {
            self.save_lists();
        }
        if programs_changed && let Some(events) = &self.events {
            let _ = events.send(Event::ProvidersChanged);
        }
        errors
    }

    async fn discover_one(&self, cfg: &ProviderConfig) -> Result<Found, LlmError> {
        let headers = self.headers(cfg, "discover").await?;
        match cfg.kind {
            ProviderKind::OpenaiChat | ProviderKind::OpenaiResponses => {
                let list =
                    llm::openai_chat::list_entries(&self.http, &cfg.base_url, &headers).await?;
                let local = local_chat(cfg);
                let mut found = Found::default();
                // What Ollama and LM Studio run each model with: the lists
                // don't say (nothing here loads a model).
                let mut other = false;
                if own_server(cfg)
                    && let Some(server) = list
                        .iter()
                        .find(|e| e.context.is_none() && !e.llama)
                        .and_then(|e| window::guess(&e.owner))
                {
                    let wanted: Vec<String> = list
                        .iter()
                        .filter(|e| !e.llama && e.context.is_none())
                        .map(|e| e.id.clone())
                        .collect();
                    if let Some(mut read) =
                        window::read(&self.http, &cfg.base_url, &headers, server, &wanted).await
                    {
                        // A model unloaded since keeps what was read while it
                        // was loaded, until its next answer shows what it
                        // runs with now (not the usual figure again).
                        let before = self
                            .state
                            .read()
                            .iter()
                            .find(|p| p.cfg.id == cfg.id)
                            .map(|p| p.windows.clone())
                            .unwrap_or_default();
                        for (id, w) in before {
                            if w.server != Server::LlamaCpp && wanted.contains(&id) {
                                read.entry(id).or_insert(w);
                            }
                        }
                        found.windows = read;
                        // llama.cpp's `/props` is not theirs to answer.
                        found.window_server = Some(server);
                        other = true;
                    }
                }
                for e in list {
                    // Only models in memory: a llama.cpp router loads any
                    // other model it is asked about. The rest keep what was
                    // read the last time they were loaded.
                    let props = if local && e.loaded && (e.llama || !other) {
                        self.probed.lock().insert((cfg.id.clone(), e.id.clone()));
                        llm::openai_chat::llama_props(&self.http, &cfg.base_url, &e.id).await
                    } else {
                        None
                    };
                    // llama.cpp: what it runs the model with (the loaded
                    // model's own answer, else the router's launch argument),
                    // not the model's trained length.
                    if let Some(tokens) = props.as_ref().and_then(|p| p.0).or(e.launch_ctx)
                        && e.llama
                    {
                        found.windows.insert(
                            e.id.clone(),
                            Window {
                                server: Server::LlamaCpp,
                                tokens,
                            },
                        );
                    }
                    let ctx = found
                        .windows
                        .get(&e.id)
                        .map(|w| w.tokens)
                        .or(e.context)
                        .or_else(|| props.as_ref().and_then(|p| p.0));
                    if let Some(c) = props.and_then(|p| p.1) {
                        let file = e.arg(MODEL_FLAGS).map(str::to_string);
                        found.caps.push((
                            e.id.clone(),
                            worth_keeping(&c).then_some(KnownCaps { file, caps: c }),
                        ));
                    }
                    found.models.push(default_info(cfg, &e.id, ctx));
                    if local {
                        found.listed.insert(e.id.clone(), e);
                    }
                }
                self.discover_daybreak(cfg, &mut found).await;
                Ok(found)
            }
            ProviderKind::OpenaiCodex => {
                let catalog_scope = self.credential_scope(cfg);
                let list =
                    llm::responses::codex_models(&self.http, &cfg.base_url, &headers).await?;
                // What the account's catalog offers, for "why is there no Daybreak switch?".
                let blue: Vec<&str> = list
                    .iter()
                    .filter(|m| llm::responses::daybreak_available(&m.cyber_access_programs))
                    .map(|m| m.id.as_str())
                    .collect();
                tracing::info!(
                    "ChatGPT models: {} listed, {} with access programs, Daybreak Blue on: {}",
                    list.len(),
                    list.iter()
                        .filter(|m| !m.cyber_access_programs.is_empty())
                        .count(),
                    if blue.is_empty() {
                        "none".to_string()
                    } else {
                        blue.join(", ")
                    }
                );
                Ok(Found {
                    catalog_scope,
                    models: list
                        .into_iter()
                        .map(|m| {
                            let mut info = default_info(cfg, &m.id, m.context);
                            info.vision = m.vision;
                            info.cyber_access_programs = m.cyber_access_programs;
                            if let Some((levels, default)) = m.levels {
                                let opts: Vec<tessera_proto::ThinkingOption> = levels
                                    .iter()
                                    .filter_map(|(e, desc)| {
                                        let lvl = crate::thinking::level_from_effort(e)?;
                                        let mut o = info
                                            .thinking
                                            .iter()
                                            .find(|o| o.level == lvl)
                                            .cloned()
                                            .unwrap_or(tessera_proto::ThinkingOption {
                                                level: lvl,
                                                label: e.clone(),
                                                hint: String::new(),
                                            });
                                        if !desc.is_empty() {
                                            o.hint = desc.clone();
                                        }
                                        Some(o)
                                    })
                                    .collect();
                                if !opts.is_empty() {
                                    info.thinking = opts;
                                }
                                if let Some(d) = default
                                    .as_deref()
                                    .and_then(crate::thinking::level_from_effort)
                                {
                                    info.default_thinking = d;
                                }
                            }
                            info
                        })
                        .collect(),
                    ..Default::default()
                })
            }
            ProviderKind::Anthropic => {
                let list = llm::anthropic::list_models(&self.http, &cfg.base_url, &headers).await?;
                Ok(Found {
                    models: list
                        .into_iter()
                        .map(|l| {
                            let mut m = default_info(cfg, &l.id, l.context);
                            if let Some(max) = l.max_output {
                                m.max_output = max;
                            }
                            if let Some(name) = l.name {
                                m.name = name;
                            }
                            if let Some(vision) = l.vision {
                                m.vision = vision;
                            }
                            m
                        })
                        .collect(),
                    ..Default::default()
                })
            }
            ProviderKind::ClaudeCode => Ok(Found::default()),
        }
    }

    /// After a reply from a local model (so it is loaded now), read its
    /// template once per run: a model that was asleep at discovery gets its
    /// real modes, and clients hear of it.
    async fn probe(&self, cfg: &ProviderConfig, model: &str) {
        if !self
            .probed
            .lock()
            .insert((cfg.id.clone(), model.to_string()))
        {
            return;
        }
        // Ollama and LM Studio have no `/props`.
        let elsewhere = self.state.read().iter().any(|p| {
            p.cfg.id == cfg.id
                && (p.window_server.is_some()
                    || p.windows
                        .get(model)
                        .is_some_and(|w| w.server != Server::LlamaCpp)
                    || p.listed
                        .get(model)
                        .is_some_and(|l| l.owner == "organization_owner"))
        });
        if elsewhere {
            return;
        }
        let Some(c) = llm::openai_chat::llama_props(&self.http, &cfg.base_url, model)
            .await
            .and_then(|p| p.1)
        else {
            return;
        };
        let changed = {
            let mut s = self.state.write();
            let Some(p) = s.iter_mut().find(|p| p.cfg.id == cfg.id) else {
                return;
            };
            let file = p
                .listed
                .get(model)
                .and_then(|l| l.arg(MODEL_FLAGS))
                .map(str::to_string);
            let before = p.caps.get(model).cloned();
            if worth_keeping(&c) {
                p.caps
                    .insert(model.to_string(), KnownCaps { file, caps: c });
            } else {
                p.caps.remove(model);
            }
            before.as_ref() != p.caps.get(model)
        };
        if changed {
            self.save_caps();
            if let Some(events) = &self.events {
                let _ = events.send(Event::ProvidersChanged);
            }
        }
    }

    /// After a reply from a model on the user's own server (so it is loaded
    /// now), read again what window the server runs it with: a model that
    /// wasn't loaded at discovery gets its real one, and a window the user
    /// changed in the server's settings is noticed. At most once a minute
    /// per model; clients hear of a change. The engine runs this beside the
    /// turn, never in front of it.
    pub async fn recheck_window(&self, m: &ModelRef) {
        let Some(cfg) = self.configs().into_iter().find(|c| c.id == m.provider) else {
            return;
        };
        if !own_server(&cfg) {
            return;
        }
        let model = m.model.as_str();
        {
            let mut read = self.window_read.lock();
            let key = (cfg.id.clone(), model.to_string());
            if read
                .get(&key)
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60))
            {
                return;
            }
            read.insert(key, tokio::time::Instant::now());
        }
        let listed = self
            .state
            .read()
            .iter()
            .find(|p| p.cfg.id == cfg.id)
            .and_then(|p| p.listed.get(model).cloned());
        let now: Option<Window> = match &listed {
            Some(l) if l.llama => llm::openai_chat::llama_props(&self.http, &cfg.base_url, model)
                .await
                .and_then(|p| p.0)
                .map(|tokens| Window {
                    server: Server::LlamaCpp,
                    tokens,
                }),
            other => {
                let owner = other.as_ref().map_or("", |l| l.owner.as_str());
                match window::guess(owner) {
                    Some(server) => match self.headers(&cfg, "window").await {
                        Ok(h) => window::read(
                            &self.http,
                            &cfg.base_url,
                            &h,
                            server,
                            &[model.to_string()],
                        )
                        .await
                        .and_then(|mut w| w.remove(model)),
                        Err(_) => None,
                    },
                    None => None,
                }
            }
        };
        // Nothing said (an unloaded model, a server that went off): what was
        // read before stays.
        let Some(now) = now else { return };
        let changed = {
            let mut s = self.state.write();
            let Some(p) = s.iter_mut().find(|p| p.cfg.id == cfg.id) else {
                return;
            };
            let before = p.windows.insert(model.to_string(), now.clone());
            if let Some(slot) = p.discovered.iter_mut().find(|m| m.id == model)
                && slot.context_window != now.tokens
            {
                let fresh = default_info(&p.cfg, model, Some(now.tokens));
                slot.context_window = fresh.context_window;
                slot.max_output = fresh.max_output;
            }
            before.as_ref() != Some(&now)
        };
        if changed {
            self.save_lists();
            if let Some(events) = &self.events {
                let _ = events.send(Event::ProvidersChanged);
            }
        }
    }

    /// What to tell a chat once about the model it just got an answer from,
    /// when its server runs it with a window too small for Tessera.
    pub fn window_notice(&self, session_id: &str, m: &ModelRef) -> Option<String> {
        let (text, tokens) = {
            let s = self.state.read();
            let p = s.iter().find(|p| p.cfg.id == m.provider)?;
            let w = p.windows.get(&m.model)?;
            (p.window_warning(&m.model)?, w.tokens)
        };
        self.window_told
            .lock()
            .insert((session_id.to_string(), m.to_string(), tokens))
            .then_some(text)
    }

    async fn headers(
        &self,
        cfg: &ProviderConfig,
        cache_key: &str,
    ) -> Result<Vec<(String, String)>, LlmError> {
        let mut h: Vec<(String, String)> = cfg
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        match cfg.kind {
            ProviderKind::OpenaiCodex => {
                let (token, account) = self
                    .auth
                    .oauth_access(&self.http, &cfg.id)
                    .await
                    .map_err(|e| LlmError::new(ErrorKind::Auth, e.to_string()))?;
                h.push(("authorization".into(), format!("Bearer {token}")));
                if let Some(a) = account {
                    h.push(("chatgpt-account-id".into(), a));
                }
                h.push(("originator".into(), "tessera".into()));
                h.push((
                    "user-agent".into(),
                    format!("tessera/{}", env!("CARGO_PKG_VERSION")),
                ));
                h.push(("session_id".into(), cache_key.into()));
                h.push(("conversation_id".into(), cache_key.into()));
                h.push(("openai-beta".into(), "responses=experimental".into()));
            }
            // API keys only: Claude.ai subscriptions can't be used by other
            // apps (they come through the user's own Claude Code instead).
            ProviderKind::Anthropic => {
                if let Some(Credential::ApiKey { key }) = self.auth.get(&cfg.id) {
                    h.push(("x-api-key".into(), key));
                }
            }
            ProviderKind::OpenaiChat | ProviderKind::OpenaiResponses => {
                if let Some(Credential::ApiKey { key }) = self.auth.get(&cfg.id) {
                    h.push(("authorization".into(), format!("Bearer {key}")));
                }
            }
            ProviderKind::ClaudeCode => {}
        }
        Ok(h)
    }

    /// The family a local model's chat template shows (`None` when unread
    /// or unknown).
    pub fn template_family(&self, m: &ModelRef) -> Option<String> {
        self.state
            .read()
            .iter()
            .find(|p| p.cfg.id == m.provider)
            .and_then(|p| p.template_family(&m.model).map(str::to_string))
    }

    /// `m`'s profile (`profiles.rs`), its chat template helping where the id
    /// says nothing.
    pub fn profile(&self, m: &ModelRef) -> crate::profiles::Profile {
        crate::profiles::profile(&m.model, self.template_family(m).as_deref())
    }

    /// The profile's additions to a request for `model` on `cfg`'s server:
    /// the model card's sampling values (only to the user's own servers, and
    /// not those the server was started with), the user's own values, one
    /// call per reply, Mistral's call ids.
    fn wire(
        &self,
        cfg: &ProviderConfig,
        model: &ModelRef,
        info: &ModelInfo,
        thinking: ThinkingLevel,
        tuning: &crate::profiles::Tuning,
    ) -> llm::openai_chat::Wire {
        use llm::openai_chat::Dialect;
        let (caps, listed, family) = {
            let s = self.state.read();
            let p = s.iter().find(|p| p.cfg.id == cfg.id);
            (
                p.and_then(|p| p.controls(&model.model).map(|(c, _)| c.clone())),
                p.and_then(|p| p.listed.get(&model.model).cloned()),
                p.and_then(|p| p.template_family(&model.model).map(str::to_string)),
            )
        };
        let profile = crate::profiles::profile(&model.model, family.as_deref());
        let thinks = match thinking {
            ThinkingLevel::Off => false,
            ThinkingLevel::Auto => caps.as_ref().map_or(info.reasoning, |c| {
                let server = listed.as_ref().and_then(Listed::server_effort);
                crate::thinking::server_default(c, server) != Some(ThinkingLevel::Off)
            }),
            _ => true,
        };
        let dialect = if cfg.base_url.contains("openrouter.ai") {
            Dialect::Extended
        } else {
            match &listed {
                Some(l) if l.llama => Dialect::Llama,
                Some(l) if l.batching => Dialect::Extended,
                Some(l) if l.owner == "organization_owner" => Dialect::LmStudio,
                _ => Dialect::Standard,
            }
        };
        let mut card = crate::profiles::Sampling::default();
        if tuning.card_sampling
            && own_server(cfg)
            && let Some(c) = profile.sampling(thinks)
        {
            card = c;
            // What the user started llama.cpp with stays theirs.
            if let Some(l) = listed.as_ref().filter(|l| l.llama) {
                let set = |flags: &[&str]| l.arg(flags).is_some();
                if set(&["--temp", "--temperature"]) {
                    card.temperature = None;
                }
                if set(&["--top-p"]) {
                    card.top_p = None;
                }
                if set(&["--top-k"]) {
                    card.top_k = None;
                }
                if set(&["--min-p"]) {
                    card.min_p = None;
                }
                if set(&["--presence-penalty"]) {
                    card.presence_penalty = None;
                }
                if set(&["--repeat-penalty"]) {
                    card.repetition_penalty = None;
                }
            }
        }
        llm::openai_chat::Wire {
            sampling: llm::openai_chat::sampling_fields(&card.over(&tuning.sampling), dialect),
            single_calls: tuning.parallel_calls == Some(false),
            mistral_ids: profile.mistral_ids,
        }
    }

    pub async fn stream(
        &self,
        model: &ModelRef,
        req: &LlmRequest,
        tx: &EventTx,
        cancel: &CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        self.stream_tuned(model, req, &Default::default(), tx, cancel)
            .await
    }

    /// [`stream`](Self::stream) with what the engine asks beside the
    /// conversation (an agent's steps: sampling, one call per reply).
    pub async fn stream_tuned(
        &self,
        model: &ModelRef,
        req: &LlmRequest,
        tuning: &crate::profiles::Tuning,
        tx: &EventTx,
        cancel: &CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        let cfg = self
            .configs()
            .into_iter()
            .find(|c| c.id == model.provider)
            .ok_or_else(|| {
                LlmError::new(
                    ErrorKind::Fatal,
                    format!("unknown provider `{}`", model.provider),
                )
            })?;
        if cfg.kind == ProviderKind::ClaudeCode {
            return Err(claude_code_only());
        }
        if !self.authenticated(&cfg) {
            return Err(LlmError::new(
                ErrorKind::Auth,
                format!("`{}` is not signed in or has no API key", cfg.id),
            ));
        }
        if req.daybreak && !daybreak::official(&cfg) {
            return Err(LlmError::new(
                ErrorKind::Fatal,
                "Daybreak Blue is available only through the official OpenAI services. Turn it off or choose a supported OpenAI model.",
            ));
        }
        if req.daybreak && daybreak::official(&cfg) {
            let errors = tokio::select! {
                _ = cancel.cancelled() => return Ok(LlmResponse {
                    content: Vec::new(), usage: Default::default(),
                    stop: tessera_proto::StopReason::Aborted, allow_tool_repair: false,
                }),
                errors = self.discover(Some(&cfg.id)) => errors,
            };
            if !errors.is_empty() {
                return Err(LlmError::new(
                    ErrorKind::Fatal,
                    "Daybreak Blue access could not be checked. Refresh OpenAI models or turn Daybreak Blue off in chat options.",
                ));
            }
        }
        let info = self
            .model_info(model)
            .unwrap_or_else(|| default_info(&cfg, &model.model, None));
        let cyber = if daybreak::official(&cfg) {
            llm::responses::cyber_program(&info.cyber_access_programs, req.daybreak)?
        } else {
            None
        };
        let headers = self.headers(&cfg, &req.cache_key).await?;
        match cfg.kind {
            ProviderKind::OpenaiChat => {
                let caps = self
                    .state
                    .read()
                    .iter()
                    .find(|p| p.cfg.id == cfg.id)
                    .and_then(|p| p.controls(&model.model).map(|(c, _)| c.clone()));
                let thinking = caps.as_ref().map_or(req.thinking, |c| {
                    crate::thinking::for_template(
                        req.thinking,
                        c,
                        &info.thinking,
                        info.default_thinking,
                    )
                });
                let snapped;
                let req = if thinking == req.thinking {
                    req
                } else {
                    snapped = LlmRequest {
                        thinking,
                        ..req.clone()
                    };
                    &snapped
                };
                let wire = self.wire(&cfg, model, &info, req.thinking, tuning);
                // Which past reasoning goes back: the provider's setting,
                // else what the model's family wants (all for most).
                let replay = if cfg.reasoning_replay.is_default() {
                    self.profile(model).replay.unwrap_or_default()
                } else {
                    cfg.reasoning_replay
                };
                let ep = llm::openai_chat::Endpoint {
                    http: &self.http,
                    base_url: &cfg.base_url,
                    headers: &headers,
                    style: crate::thinking::chat_style(
                        &cfg.base_url,
                        &model.model,
                        info.reasoning,
                        caps.as_ref(),
                    ),
                    vision: info.vision,
                    replay,
                    wire: &wire,
                };
                // Held until the reply (and its template check) is in.
                let _turn = match self.turn_on(&cfg.base_url, model) {
                    Some(queue) => tokio::select! {
                        turn = queue.acquire_owned() => turn.ok(),
                        () = cancel.cancelled() => return Ok(LlmResponse {
                            content: vec![],
                            usage: Default::default(),
                            stop: tessera_proto::StopReason::Aborted,
                            allow_tool_repair: false,
                        }),
                    },
                    None => None,
                };
                let res = llm::openai_chat::stream(&ep, req, tx, cancel).await;
                if local_chat(&cfg)
                    && res
                        .as_ref()
                        .is_ok_and(|r| r.stop != tessera_proto::StopReason::Aborted)
                {
                    self.probe(&cfg, &model.model).await;
                }
                if !own_server(&cfg) {
                    return res;
                }
                // The user's own server: the model picker shows whether it
                // answers, and a turn says so in plain words.
                match res {
                    Err(mut e) if e.unreachable => {
                        self.set_offline(&cfg.id, true);
                        e.message = not_answering(&cfg);
                        Err(e)
                    }
                    res => {
                        if res.is_ok() {
                            self.set_offline(&cfg.id, false);
                        }
                        res
                    }
                }
            }
            ProviderKind::OpenaiResponses | ProviderKind::OpenaiCodex => {
                let codex = cfg.kind == ProviderKind::OpenaiCodex;
                // A ChatGPT plan's limits come along with every reply.
                let keep = |l: Vec<tessera_proto::PlanLimit>| self.limits.record(&cfg.id, l, true);
                let ep = llm::responses::Endpoint {
                    http: &self.http,
                    base_url: &cfg.base_url,
                    headers: &headers,
                    reasoning: info.reasoning,
                    vision: info.vision,
                    codex,
                    fast: req.fast && info.fast,
                    cyber,
                    limits: codex
                        .then_some(&keep as &(dyn Fn(Vec<tessera_proto::PlanLimit>) + Sync)),
                };
                let response = llm::responses::stream(&ep, req, tx, cancel).await;
                if cyber == Some("daybreak_blue")
                    && response
                        .as_ref()
                        .is_err_and(|e| matches!(e.kind, ErrorKind::Auth | ErrorKind::Fatal))
                {
                    self.clear_programs(&cfg.id);
                    if let Some(events) = &self.events {
                        let _ = events.send(Event::ProvidersChanged);
                    }
                }
                response
            }
            ProviderKind::Anthropic => {
                let ep = llm::anthropic::Endpoint {
                    http: &self.http,
                    base_url: &cfg.base_url,
                    headers: &headers,
                    max_output: info.max_output,
                };
                llm::anthropic::stream(&ep, req, tx, cancel).await
            }
            ProviderKind::ClaudeCode => Err(claude_code_only()),
        }
    }
}

/// Claude Code runs whole chat turns as itself; it isn't a model endpoint
/// for Tessera's own requests (titles, summaries, other agents' steps).
fn claude_code_only() -> LlmError {
    LlmError::new(
        ErrorKind::Fatal,
        "Claude (your subscription) runs chats through your own Claude Code and can't be used for this; pick another model for it",
    )
}

fn merged_models(p: &ProviderState, chosen: &HashMap<String, ThinkingLevel>) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = p.discovered.clone();
    for mc in &p.cfg.models {
        let base = out
            .iter()
            .find(|m| m.id == mc.id)
            .cloned()
            .unwrap_or_else(|| default_info(&p.cfg, &mc.id, None));
        let merged = with_config(p, mc, base);
        match out.iter_mut().find(|m| m.id == mc.id) {
            Some(slot) => *slot = merged,
            None => out.push(merged),
        }
    }
    for m in &mut out {
        p.local_thinking(m);
        m.last_thinking = chosen.get(&format!("{}/{}", p.cfg.id, m.id)).copied();
        m.context_warning = p.window_warning(&m.id);
    }
    out
}

/// One model of [`merged_models`], without building the whole list (a
/// provider can list hundreds, and this is asked several times a step).
fn merged_model(
    p: &ProviderState,
    chosen: &HashMap<String, ThinkingLevel>,
    id: &str,
) -> Option<ModelInfo> {
    let mut found = p.discovered.iter().find(|m| m.id == id).cloned();
    for mc in p.cfg.models.iter().filter(|mc| mc.id == id) {
        let base = found
            .take()
            .unwrap_or_else(|| default_info(&p.cfg, &mc.id, None));
        found = Some(with_config(p, mc, base));
    }
    let mut m = found?;
    p.local_thinking(&mut m);
    m.last_thinking = chosen.get(&format!("{}/{}", p.cfg.id, m.id)).copied();
    m.context_warning = p.window_warning(&m.id);
    Some(m)
}

/// `base` with what the configuration says about the model.
fn with_config(p: &ProviderState, mc: &ModelConfig, base: ModelInfo) -> ModelInfo {
    ModelInfo {
        provider: p.cfg.id.clone(),
        id: mc.id.clone(),
        name: mc.name.clone().unwrap_or(base.name),
        context_window: mc.context_window.unwrap_or(base.context_window),
        max_output: mc.max_output.unwrap_or(base.max_output),
        reasoning: mc.reasoning.unwrap_or(base.reasoning),
        vision: mc.vision.unwrap_or(base.vision),
        thinking: base.thinking,
        default_thinking: base.default_thinking,
        last_thinking: None,
        fast: mc.fast.unwrap_or(base.fast),
        cyber_access_programs: base.cyber_access_programs,
        small_mode: base.small_mode,
        small: base.small,
        context_warning: None,
    }
}

/// Sensible defaults from the model id when an endpoint doesn't say.
pub fn default_info(cfg: &ProviderConfig, id: &str, ctx: Option<u64>) -> ModelInfo {
    let l = id.to_ascii_lowercase();
    let reasoning = matches!(
        cfg.kind,
        ProviderKind::OpenaiCodex | ProviderKind::Anthropic | ProviderKind::ClaudeCode
    ) || [
        "gpt-5",
        "o1",
        "o3",
        "o4",
        "qwen3",
        "qwq",
        "deepseek",
        "r1",
        "thinking",
        "reason",
        "glm-4.5",
        "glm-4.6",
        "glm-5",
        "kimi",
        "gpt-oss",
        "magistral",
        "minimax",
    ]
    .iter()
    .any(|k| l.contains(k));
    let vision = matches!(
        cfg.kind,
        ProviderKind::OpenaiCodex | ProviderKind::Anthropic | ProviderKind::ClaudeCode
    ) || [
        "vl", "vision", "gpt-4o", "gpt-5", "gemini", "llava", "gemma-3", "gemma3", "pixtral",
        "qwen3.5", "qwen3.6", "qwen3.8", "claude", "omni",
    ]
    .iter()
    .any(|k| l.contains(k));
    let window = ctx.unwrap_or(match cfg.kind {
        ProviderKind::OpenaiCodex => 272_000,
        ProviderKind::Anthropic => anthropic_limits(&l).0,
        ProviderKind::ClaudeCode => 200_000,
        _ if l.contains("gemini") => 1_000_000,
        _ => 128_000,
    });
    let (thinking, default_thinking) =
        crate::thinking::known(cfg.kind, &cfg.base_url, id, reasoning);
    let mut m = ModelInfo {
        provider: cfg.id.clone(),
        id: id.to_string(),
        name: match cfg.kind {
            ProviderKind::ClaudeCode => crate::claude_code::model_name(id),
            _ => id.to_string(),
        },
        context_window: window,
        max_output: match cfg.kind {
            ProviderKind::Anthropic => anthropic_limits(&l).1,
            ProviderKind::ClaudeCode => 64_000,
            ProviderKind::OpenaiCodex => 128_000,
            _ => (window / 4).min(32_768),
        },
        reasoning,
        vision,
        thinking,
        default_thinking,
        last_thinking: None,
        fast: fast_tier(cfg.kind),
        cyber_access_programs: Vec::new(),
        small_mode: Default::default(),
        small: false,
        context_warning: None,
    };
    if cfg.kind == ProviderKind::OpenaiChat {
        recommended_effort(&mut m, None);
    }
    m
}

/// (context window, max output) of a Claude model when the API doesn't say:
/// 1M and 128K from the 4.6 generation on, 200K and 64K before (32K for
/// Opus 4.0/4.1).
fn anthropic_limits(id: &str) -> (u64, u64) {
    let older = |p: &[&str]| p.iter().any(|p| id.contains(p));
    if older(&[
        "claude-3",
        "opus-4-0",
        "opus-4-1",
        "opus-4-2025",
        "claude-opus-4@",
    ]) {
        (200_000, 32_000)
    } else if older(&["-4-5", "sonnet-4-0", "sonnet-4-2025", "claude-sonnet-4@"]) {
        (200_000, 64_000)
    } else {
        (1_000_000, 128_000)
    }
}

/// Whether a provider's models offer priority processing (fast mode) unless
/// a model says otherwise: the OpenAI Responses API and the ChatGPT backend.
pub fn fast_tier(kind: ProviderKind) -> bool {
    matches!(
        kind,
        ProviderKind::OpenaiResponses | ProviderKind::OpenaiCodex
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_limits_follow_the_generation() {
        assert_eq!(anthropic_limits("claude-opus-5-5"), (1_000_000, 128_000));
        assert_eq!(anthropic_limits("claude-sonnet-4-6"), (1_000_000, 128_000));
        assert_eq!(
            anthropic_limits("claude-haiku-4-5-20251001"),
            (200_000, 64_000)
        );
        assert_eq!(anthropic_limits("claude-sonnet-4-0"), (200_000, 64_000));
        assert_eq!(
            anthropic_limits("claude-opus-4-1-20250805"),
            (200_000, 32_000)
        );
    }
}

#[cfg(test)]
mod serving {
    use super::*;

    fn providers(cfgs: Vec<ProviderConfig>) -> (Providers, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let auth = Arc::new(AuthStore::load(dir.path().join("auth.json")).unwrap());
        (Providers::new(reqwest::Client::new(), auth, &cfgs), dir)
    }

    fn cfg(id: &str, kind: ProviderKind, url: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            name: String::new(),
            kind,
            base_url: url.into(),
            discover: true,
            models: vec![],
            headers: Default::default(),
            reasoning_replay: Default::default(),
            parallel: None,
        }
    }

    fn listed(p: &Providers, id: &str, model: &str, l: Listed) {
        let mut s = p.state.write();
        let st = s.iter_mut().find(|p| p.cfg.id == id).unwrap();
        st.listed.insert(
            model.into(),
            Listed {
                id: model.into(),
                ..l
            },
        );
    }

    fn m(provider: &str, model: &str) -> ModelRef {
        ModelRef {
            provider: provider.into(),
            model: model.into(),
        }
    }

    #[test]
    fn fresh_install_presets_are_not_ready_or_selected() {
        let (p, _d) = providers(vec![]);
        let codex = p
            .infos()
            .into_iter()
            .find(|p| p.id == CODEX_PROVIDER)
            .unwrap();
        assert!(!codex.authenticated);
        assert!(
            !codex.models.is_empty(),
            "presets remain visible for sign-in"
        );
        assert!(p.first_model().is_none());
    }

    #[test]
    fn first_model_requires_sign_in_and_forgets_signed_out_accounts() {
        let (p, _d) = providers(vec![]);
        p.auth
            .set(
                CODEX_PROVIDER,
                Credential::Oauth {
                    access: "mock-access".into(),
                    refresh: "mock-refresh".into(),
                    expires: u64::MAX,
                    account_id: None,
                    email: None,
                },
            )
            .unwrap();
        assert_eq!(p.first_model().unwrap().provider, CODEX_PROVIDER);
        p.auth.remove(CODEX_PROVIDER).unwrap();
        assert!(p.first_model().is_none());
    }

    #[test]
    fn hosted_chat_requires_credentials_but_local_servers_do_not() {
        let mut hosted = cfg(
            "hosted",
            ProviderKind::OpenaiChat,
            "https://openrouter.ai/api/v1",
        );
        hosted.models = codex_preset().models;
        let (p, _d) = providers(vec![hosted]);
        assert!(p.first_model().is_none());
        p.auth
            .set(
                "hosted",
                Credential::ApiKey {
                    key: "mock-key".into(),
                },
            )
            .unwrap();
        assert_eq!(p.first_model().unwrap().provider, "hosted");
        p.auth.remove("hosted").unwrap();
        let mut local = cfg(
            "local",
            ProviderKind::OpenaiChat,
            "http://127.0.0.1:11434/v1",
        );
        local.models = codex_preset().models;
        p.upsert(local);
        assert_eq!(p.first_model().unwrap().provider, "local");
    }
    #[test]
    fn which_servers_take_one_request_at_a_time() {
        let mut fixed = cfg(
            "fixed",
            ProviderKind::OpenaiChat,
            "http://192.168.1.5:8080/v1",
        );
        fixed.parallel = Some(4);
        let (p, _d) = providers(vec![
            cfg(
                "local",
                ProviderKind::OpenaiChat,
                "http://127.0.0.1:1234/v1",
            ),
            cfg(
                "lan",
                ProviderKind::OpenaiChat,
                "http://192.168.1.50:1234/v1",
            ),
            cfg("vllm", ProviderKind::OpenaiChat, "http://10.0.0.2:8000/v1"),
            cfg(
                "hosted",
                ProviderKind::OpenaiChat,
                "https://openrouter.ai/api/v1",
            ),
            cfg(
                "anthropic",
                ProviderKind::Anthropic,
                "https://api.anthropic.com",
            ),
            fixed,
        ]);
        let args = |a: &[&str]| Listed {
            llama: true,
            args: a.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        listed(
            &p,
            "lan",
            "one",
            args(&["--parallel", "1", "--model", "/m.gguf"]),
        );
        listed(&p, "lan", "four", args(&["-np", "4"]));
        listed(&p, "lan", "auto", args(&["--parallel", "-1"]));
        listed(&p, "lan", "unsaid", args(&["--model", "/m.gguf"]));
        listed(
            &p,
            "vllm",
            "big",
            Listed {
                batching: true,
                ..Default::default()
            },
        );
        // Unknown local servers: one at a time (llama.cpp's way for one user, Ollama, LM Studio).
        assert!(p.one_at_a_time(&m("local", "x")));
        assert!(p.one_at_a_time(&m("lan", "one")));
        assert!(p.one_at_a_time(&m("lan", "unsaid")));
        assert!(!p.one_at_a_time(&m("lan", "four")));
        assert!(!p.one_at_a_time(&m("lan", "auto")));
        assert!(!p.one_at_a_time(&m("vllm", "big")));
        assert!(!p.one_at_a_time(&m("fixed", "x")));
        // Hosted APIs never.
        assert!(!p.one_at_a_time(&m("hosted", "x")));
        assert!(!p.one_at_a_time(&m("anthropic", "claude-opus-5-5")));
        assert!(!p.one_at_a_time(&m("missing", "x")));
    }

    #[test]
    fn one_queue_per_single_slot_server_whatever_names_it() {
        let (p, _d) = providers(vec![
            cfg(
                "mux",
                ProviderKind::OpenaiChat,
                "http://192.168.1.50:1234/v1",
            ),
            // The same server under another provider and path.
            cfg(
                "also",
                ProviderKind::OpenaiChat,
                "http://192.168.1.50:1234/",
            ),
            cfg(
                "other",
                ProviderKind::OpenaiChat,
                "http://192.168.1.50:18080/v1",
            ),
            cfg(
                "hosted",
                ProviderKind::OpenaiChat,
                "https://api.example.com/v1",
            ),
        ]);
        listed(
            &p,
            "other",
            "four",
            Listed {
                llama: true,
                args: vec!["-np".into(), "4".into()],
                ..Default::default()
            },
        );
        let turn = |id: &str, url: &str, model: &str| p.turn_on(url, &m(id, model));
        let a = turn("mux", "http://192.168.1.50:1234/v1", "qwen3.8-flash").unwrap();
        let alias = turn(
            "mux",
            "http://192.168.1.50:1234/v1",
            "qwen3.8-flash-next-iq3_s",
        )
        .unwrap();
        let also = turn("also", "http://192.168.1.50:1234/", "m").unwrap();
        let elsewhere = turn("other", "http://192.168.1.50:18080/v1", "m").unwrap();
        assert!(
            Arc::ptr_eq(&a, &alias),
            "model names on one server share its queue"
        );
        assert!(
            Arc::ptr_eq(&a, &also),
            "so does another provider for the same address"
        );
        assert!(!Arc::ptr_eq(&a, &elsewhere));
        assert_eq!(a.available_permits(), 1);
        // Several slots, or a hosted API: no queue.
        assert!(turn("other", "http://192.168.1.50:18080/v1", "four").is_none());
        assert!(turn("hosted", "https://api.example.com/v1", "x").is_none());
    }

    #[test]
    fn tool_choice_none_only_where_it_is_honoured() {
        let (p, _d) = providers(vec![
            cfg(
                "local",
                ProviderKind::OpenaiChat,
                "http://127.0.0.1:1234/v1",
            ),
            cfg(
                "openai",
                ProviderKind::OpenaiChat,
                "https://api.openai.com/v1",
            ),
            cfg(
                "router",
                ProviderKind::OpenaiChat,
                "https://openrouter.ai/api/v1",
            ),
            cfg(
                "anthropic",
                ProviderKind::Anthropic,
                "https://api.anthropic.com",
            ),
            cfg(
                "responses",
                ProviderKind::OpenaiResponses,
                "https://api.openai.com/v1",
            ),
        ]);
        listed(
            &p,
            "local",
            "llama",
            Listed {
                llama: true,
                ..Default::default()
            },
        );
        assert!(p.takes_tool_choice_none(&m("local", "llama")));
        // Ollama, LM Studio and unknown servers: the tools are left out instead.
        assert!(!p.takes_tool_choice_none(&m("local", "other")));
        assert!(p.takes_tool_choice_none(&m("openai", "gpt-5.5")));
        assert!(p.takes_tool_choice_none(&m("router", "x")));
        assert!(p.takes_tool_choice_none(&m("anthropic", "claude-opus-5-5")));
        assert!(p.takes_tool_choice_none(&m("responses", "gpt-5.5")));
        // The ChatGPT backend: not known to.
        assert!(!p.takes_tool_choice_none(&m(CODEX_PROVIDER, "gpt-5.5")));
    }

    /// The model card's sampling goes only to the user's own servers, under
    /// the names each takes, never over what llama.cpp was started with; the
    /// user's own values go anywhere OpenAI-compatible.
    #[test]
    fn card_sampling_only_to_own_servers() {
        let (p, _d) = providers(vec![
            cfg(
                "lan",
                ProviderKind::OpenaiChat,
                "http://192.168.1.50:1234/v1",
            ),
            cfg(
                "router",
                ProviderKind::OpenaiChat,
                "https://openrouter.ai/api/v1",
            ),
            cfg(
                "groq",
                ProviderKind::OpenaiChat,
                "https://api.groq.com/openai/v1",
            ),
        ]);
        let args = |a: &[&str], owner: &str| Listed {
            llama: owner == "llamacpp",
            batching: owner == "vllm",
            owner: owner.into(),
            args: a.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        listed(
            &p,
            "lan",
            "qwen3.8-27b",
            args(&["--temp", "0.6"], "llamacpp"),
        );
        listed(&p, "lan", "Qwen3-8B", args(&[], "vllm"));
        listed(&p, "lan", "qwen3.5-9b", args(&[], "organization_owner"));
        listed(&p, "lan", "qwen3:8b", args(&[], "library"));
        let wire = |provider: &str, model: &str, thinking, tuning: &crate::profiles::Tuning| {
            let c = p.configs().into_iter().find(|c| c.id == provider).unwrap();
            let m = m(provider, model);
            let info = p.model_info(&m).unwrap();
            let w = p.wire(&c, &m, &info, thinking, tuning);
            (
                w.sampling
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                w.single_calls,
            )
        };
        let card = crate::profiles::Tuning {
            card_sampling: true,
            ..Default::default()
        };
        // llama.cpp started with `--temp`: everything else from the card.
        assert_eq!(
            wire("lan", "qwen3.8-27b", ThinkingLevel::High, &card).0,
            "top_p=0.95 presence_penalty=0.0 top_k=20 min_p=0.0"
        );
        // Thinking off: the instruct values.
        assert_eq!(
            wire("lan", "qwen3.8-27b", ThinkingLevel::Off, &card).0,
            "top_p=0.8 presence_penalty=1.5 top_k=20 min_p=0.0"
        );
        assert_eq!(
            wire("lan", "Qwen3-8B", ThinkingLevel::Auto, &card).0,
            "temperature=0.6 top_p=0.95 top_k=20 min_p=0.0"
        );
        assert_eq!(
            wire("lan", "qwen3.5-9b", ThinkingLevel::High, &card).0,
            "temperature=0.6 top_p=0.95 presence_penalty=0.0 top_k=20"
        );
        assert_eq!(
            wire("lan", "qwen3:8b", ThinkingLevel::High, &card).0,
            "temperature=0.6 top_p=0.95"
        );
        // Hosted: nothing from the card; the user's own values do go.
        assert_eq!(
            wire("router", "qwen/qwen3-8b", ThinkingLevel::High, &card).0,
            ""
        );
        assert_eq!(
            wire("groq", "qwen/qwen3-32b", ThinkingLevel::High, &card).0,
            ""
        );
        let mine = crate::profiles::Tuning {
            card_sampling: true,
            sampling: crate::profiles::Sampling {
                temperature: Some(0.2),
                min_p: Some(0.1),
                ..Default::default()
            },
            parallel_calls: Some(false),
        };
        assert_eq!(
            wire("router", "qwen/qwen3-8b", ThinkingLevel::High, &mine),
            ("temperature=0.2 min_p=0.1".to_string(), true)
        );
        assert_eq!(
            wire("groq", "x", ThinkingLevel::High, &mine).0,
            "temperature=0.2"
        );
        // Off by `[tuning] sampling = false`.
        assert_eq!(
            wire("lan", "Qwen3-8B", ThinkingLevel::High, &Default::default()),
            (String::new(), false)
        );
    }

    #[test]
    fn one_model_is_found_as_the_whole_list_has_it() {
        let mut c = cfg(
            "local",
            ProviderKind::OpenaiChat,
            "http://127.0.0.1:1234/v1",
        );
        c.models = vec![
            ModelConfig {
                id: "a".into(),
                name: Some("Model A".into()),
                context_window: Some(32_000),
                max_output: None,
                reasoning: Some(true),
                vision: None,
                price: None,
                fast: None,
            },
            ModelConfig {
                id: "only-here".into(),
                name: None,
                context_window: None,
                max_output: Some(4_000),
                reasoning: None,
                vision: Some(true),
                price: None,
                fast: None,
            },
        ];
        let (p, _d) = providers(vec![c]);
        p.set_discovered(
            "local",
            ["a", "b", "qwen3-8b"]
                .iter()
                .map(|id| default_info(&p.configs()[0], id, Some(100_000)))
                .collect(),
        );
        p.choose_thinking(&m("local", "b"), ThinkingLevel::Low);
        let s = p.state.read();
        let st = s.iter().find(|x| x.cfg.id == "local").unwrap();
        let chosen = p.chosen.read();
        let all = merged_models(st, &chosen);
        assert_eq!(all.len(), 4);
        for want in &all {
            assert_eq!(merged_model(st, &chosen, &want.id).as_ref(), Some(want));
        }
        assert_eq!(merged_model(st, &chosen, "nope"), None);
    }
}
