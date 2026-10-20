//! What model requests cost: how each provider is paid for (per token, by
//! plan, or not at all), and the public price catalog that fills in prices
//! Tessera doesn't know.
//!
//! The catalog (models.dev's `api.json` by default) is a plain download:
//! nothing about the user is sent. It is fetched at most once a day, only
//! when the usage stats meet a model without a known price, and only the
//! prices of a few first-party providers are kept (in memory and in
//! `price-catalog.json` in the data directory), so it works offline from the
//! last copy.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use parking_lot::Mutex;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use tessera_proto::{Billing, Price, ProviderKind};

use crate::auth::Credential;
use crate::config::{Config, ProviderConfig};
use crate::util::now_ms;

/// The provider id under which chats run by the user's own Claude Code
/// (`claude -p`) are recorded: a Claude Pro/Max subscription.
pub const CLAUDE_CODE: &str = "claude-code";

/// Catalog providers whose prices are kept, first-party first: an id listed
/// by several is priced by the first. OpenRouter ids carry their vendor
/// (`anthropic/claude-…`), so they don't collide with the others.
const KEPT: &[&str] = &[
    "openai",
    "anthropic",
    "google",
    "deepseek",
    "mistral",
    "xai",
    "moonshotai",
    "zai",
    "alibaba",
    "minimax",
    "openrouter",
];

/// API hosts and the catalog provider whose prices they charge.
const HOSTS: &[(&str, &str)] = &[
    ("openrouter.ai", "openrouter"),
    ("api.openai.com", "openai"),
    ("api.anthropic.com", "anthropic"),
    ("generativelanguage.googleapis.com", "google"),
    ("api.deepseek.com", "deepseek"),
    ("api.mistral.ai", "mistral"),
    ("api.x.ai", "xai"),
    ("api.moonshot.ai", "moonshotai"),
    ("api.z.ai", "zai"),
    ("dashscope-intl.aliyuncs.com", "alibaba"),
    ("api.minimax.io", "minimax"),
];

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// After a failed download, wait this long before trying again.
const RETRY_MS: u64 = 60 * 60 * 1000;
const MAX_BYTES: usize = 32 << 20;

/// How a provider is paid for: a flat-fee plan (ChatGPT sign-in, Claude
/// Code, or a plan the user set), nothing (an OpenAI-compatible server on
/// this machine or network, or one without a key), else per token.
pub fn billing(cfg: &ProviderConfig, cred: Option<&Credential>, config: &Config) -> Billing {
    if cfg.id == CLAUDE_CODE
        || matches!(
            cfg.kind,
            ProviderKind::OpenaiCodex | ProviderKind::ClaudeCode
        )
        || config.pricing.subscriptions.contains_key(&cfg.id)
        || matches!(cred, Some(Credential::Oauth { .. }))
    {
        return Billing::Subscription;
    }
    let internal =
        url::Url::parse(&cfg.base_url).is_ok_and(|u| crate::tools::web::internal_url(&u));
    if cfg.kind == ProviderKind::OpenaiChat && (internal || cred.is_none()) {
        Billing::Local
    } else {
        Billing::Api
    }
}

/// The catalog provider whose prices `cfg`'s endpoint charges, if known.
pub fn catalog_home(cfg: &ProviderConfig) -> Option<&'static str> {
    if cfg.id == CLAUDE_CODE
        || matches!(cfg.kind, ProviderKind::Anthropic | ProviderKind::ClaudeCode)
    {
        return Some("anthropic");
    }
    if cfg.kind == ProviderKind::OpenaiCodex {
        return Some("openai");
    }
    let url = url::Url::parse(&cfg.base_url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    HOSTS
        .iter()
        .find(|(h, _)| host == *h || host.ends_with(&format!(".{h}")))
        .map(|(_, p)| *p)
}

#[derive(Default, Serialize, Deserialize)]
struct Cached {
    /// Unix ms of the download.
    fetched_at: u64,
    /// Catalog provider → model id → price.
    providers: HashMap<String, HashMap<String, Price>>,
}

#[derive(Default)]
struct State {
    loaded: bool,
    cached: Cached,
    /// Unix ms of the last download attempt (this process).
    attempted_at: u64,
}

pub struct Catalog {
    file: PathBuf,
    state: Mutex<State>,
    /// One download at a time.
    fetching: tokio::sync::Mutex<()>,
}

impl Catalog {
    /// The catalog kept in `data_dir` (read when first needed).
    pub fn new(data_dir: &std::path::Path) -> Self {
        Self {
            file: data_dir.join("price-catalog.json"),
            state: Mutex::new(State::default()),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let mut st = self.state.lock();
        if !st.loaded {
            st.loaded = true;
            if let Ok(bytes) = std::fs::read(&self.file) {
                match serde_json::from_slice::<Cached>(&bytes) {
                    Ok(c) => st.cached = c,
                    Err(e) => tracing::debug!("ignoring {}: {e}", self.file.display()),
                }
            }
        }
        f(&mut st)
    }

    /// The catalog's price for `model`: as `home` lists it, else as the
    /// first kept provider that does (by the full id, then its last part).
    pub fn lookup(&self, home: Option<&str>, model: &str) -> Option<Price> {
        let short = model.rsplit('/').next().unwrap_or(model);
        self.with(|st| {
            let find = |p: &str| {
                let models = st.cached.providers.get(p)?;
                models.get(model).or_else(|| models.get(short)).copied()
            };
            home.and_then(find)
                .or_else(|| KEPT.iter().find_map(|p| find(p)))
        })
    }

    /// Older than a day (or never fetched), and not tried within the hour.
    pub fn stale(&self) -> bool {
        let now = now_ms();
        self.with(|st| {
            now.saturating_sub(st.cached.fetched_at) > DAY_MS
                && now.saturating_sub(st.attempted_at) > RETRY_MS
        })
    }

    /// Download the catalog from `url` if it is [`stale`](Self::stale) and
    /// keep its prices. `true` when the prices changed.
    pub async fn refresh(&self, http: &reqwest::Client, url: &str) -> Result<bool> {
        let _one = self.fetching.lock().await;
        if !self.stale() {
            return Ok(false);
        }
        self.with(|st| st.attempted_at = now_ms());
        let mut resp = http
            .get(url)
            .header("user-agent", concat!("tessera/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?
            .error_for_status()?;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BYTES {
                bail!("the price catalog is larger than {} MB", MAX_BYTES >> 20);
            }
        }
        let providers = parse(&body)?;
        drop(body);
        let cached = Cached {
            fetched_at: now_ms(),
            providers,
        };
        match serde_json::to_vec(&cached) {
            Ok(bytes) => {
                if let Err(e) = crate::config::write_atomic(&self.file, &bytes, false) {
                    tracing::warn!("saving the price catalog: {e}");
                }
            }
            Err(e) => tracing::warn!("saving the price catalog: {e}"),
        }
        let changed = self.with(|st| {
            let changed = st.cached.providers != cached.providers;
            st.cached = cached;
            changed
        });
        Ok(changed)
    }
}

/// The prices of the [`KEPT`] providers in a models.dev-style catalog
/// (`{provider: {models: {id: {cost: {input, output, cache_read?,
/// cache_write?}}}}}`, US dollars per million tokens); everything else is
/// skipped while parsing.
fn parse(body: &[u8]) -> Result<HashMap<String, HashMap<String, Price>>> {
    let Kept(out) = serde_json::from_slice(body)?;
    if out.is_empty() {
        bail!("no prices in the catalog");
    }
    Ok(out)
}

struct Kept(HashMap<String, HashMap<String, Price>>);

#[derive(Deserialize)]
struct RawProvider {
    #[serde(default)]
    models: HashMap<String, RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(default)]
    cost: Option<RawCost>,
}

#[derive(Deserialize)]
struct RawCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl<'de> Deserialize<'de> for Kept {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Kept;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("providers by id")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Kept, A::Error> {
                let mut out = HashMap::new();
                while let Some(id) = map.next_key::<String>()? {
                    if !KEPT.contains(&id.as_str()) {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    let p: RawProvider = map.next_value()?;
                    let prices: HashMap<String, Price> = p
                        .models
                        .into_iter()
                        .filter_map(|(model, m)| {
                            let c = m.cost?;
                            let price = Price {
                                input: c.input?,
                                output: c.output?,
                                cache_read: c.cache_read,
                                cache_write: c.cache_write,
                            };
                            price.is_valid().then_some((model, price))
                        })
                        .collect();
                    if !prices.is_empty() {
                        out.insert(id, prices);
                    }
                }
                Ok(Kept(out))
            }
        }
        d.deserialize_map(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, kind: ProviderKind, url: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            name: String::new(),
            kind,
            base_url: url.into(),
            discover: false,
            models: Vec::new(),
            headers: Default::default(),
            reasoning_replay: Default::default(),
            parallel: None,
        }
    }

    #[test]
    fn who_pays_how() {
        let mut config = Config::default();
        let key = Credential::ApiKey { key: "k".into() };
        let oauth = Credential::Oauth {
            access: "a".into(),
            refresh: String::new(),
            expires: 0,
            account_id: None,
            email: None,
        };
        let anthropic = provider(
            "anthropic",
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
        );
        let codex = provider("openai-codex", ProviderKind::OpenaiCodex, "");
        let local = provider(
            "local",
            ProviderKind::OpenaiChat,
            "http://127.0.0.1:1234/v1",
        );
        let router = provider(
            "openrouter",
            ProviderKind::OpenaiChat,
            "https://openrouter.ai/api/v1",
        );
        let claude = provider(CLAUDE_CODE, ProviderKind::OpenaiChat, "");
        assert_eq!(billing(&anthropic, Some(&key), &config), Billing::Api);
        assert_eq!(
            billing(&codex, Some(&oauth), &config),
            Billing::Subscription
        );
        assert_eq!(billing(&codex, None, &config), Billing::Subscription);
        assert_eq!(billing(&claude, None, &config), Billing::Subscription);
        // A llama.cpp on this machine is free, key or not; a hosted one isn't.
        assert_eq!(billing(&local, None, &config), Billing::Local);
        assert_eq!(billing(&local, Some(&key), &config), Billing::Local);
        assert_eq!(billing(&router, Some(&key), &config), Billing::Api);
        // A plan set for a pay-per-token provider (a "coding plan").
        config
            .pricing
            .subscriptions
            .insert("openrouter".into(), Default::default());
        assert_eq!(billing(&router, Some(&key), &config), Billing::Subscription);
        assert_eq!(catalog_home(&codex), Some("openai"));
        assert_eq!(catalog_home(&claude), Some("anthropic"));
        assert_eq!(catalog_home(&router), Some("openrouter"));
        assert_eq!(catalog_home(&local), None);
        assert_eq!(
            catalog_home(&provider(
                "x",
                ProviderKind::OpenaiChat,
                "https://evil-api.x.ai.example/v1"
            )),
            None
        );
    }

    #[test]
    fn the_catalog_keeps_only_first_party_prices_and_survives_restarts() {
        let body = serde_json::json!({
            "openai": {"id": "openai", "name": "OpenAI", "models": {
                "gpt-6.1-sol": {"id": "gpt-6.1-sol", "limit": {"context": 1}, "cost": {"input": 2.0, "output": 10.0, "cache_read": 0.1, "tiers": [{"input": 4.0}]}},
                "gpt-image-2": {"id": "gpt-image-2"},
                "bad": {"cost": {"input": -1.0, "output": 1.0}}
            }},
            "anthropic": {"models": {"claude-new-1": {"cost": {"input": 3.0, "output": 15.0, "cache_read": 0.3, "cache_write": 3.75}}}},
            "some-reseller": {"models": {"gpt-6.1-sol": {"cost": {"input": 9.0, "output": 9.0}}}},
            "openrouter": {"models": {"openai/gpt-6.1-sol": {"cost": {"input": 2.5, "output": 12.0}}}}
        });
        let parsed = parse(body.to_string().as_bytes()).unwrap();
        assert!(!parsed.contains_key("some-reseller"));
        assert_eq!(parsed["openai"].len(), 1, "no price, bad price: skipped");
        assert!(parse(b"{\"other\": {}}").is_err());
        assert!(parse(b"not json").is_err());

        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::new(dir.path());
        assert!(cat.stale() && cat.lookup(Some("openai"), "gpt-6.1-sol").is_none());
        let cached = Cached {
            fetched_at: now_ms(),
            providers: parsed,
        };
        std::fs::write(
            dir.path().join("price-catalog.json"),
            serde_json::to_vec(&cached).unwrap(),
        )
        .unwrap();
        let cat = Catalog::new(dir.path());
        assert!(!cat.stale());
        let p = cat.lookup(Some("openai"), "gpt-6.1-sol").unwrap();
        assert_eq!(
            (p.input, p.cache_read, p.cache_write),
            (2.0, Some(0.1), None)
        );
        // The endpoint's own provider first, then any first-party one.
        assert_eq!(
            cat.lookup(Some("openrouter"), "openai/gpt-6.1-sol")
                .unwrap()
                .input,
            2.5
        );
        assert_eq!(
            cat.lookup(Some("openrouter"), "anthropic/claude-new-1")
                .unwrap()
                .input,
            3.0
        );
        assert_eq!(cat.lookup(None, "gpt-6.1-sol").unwrap().input, 2.0);
        assert!(cat.lookup(None, "qwen3.8-27b").is_none());
    }
}
