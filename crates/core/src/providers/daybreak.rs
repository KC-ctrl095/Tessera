//! Program access comes from OpenAI's caller-specific catalog, not model names.

use sha2::{Digest, Sha256};

use super::*;

/// OpenAI's deprecated Daybreak model alias: valid when chosen, never suggested.
const DAYBREAK_ALIAS: &str = "gpt-daybreak-blue-latest";

pub(super) fn official(cfg: &ProviderConfig) -> bool {
    matches!(
        (cfg.kind, cfg.base_url.trim_end_matches('/')),
        (ProviderKind::OpenaiCodex, CODEX_BASE_URL)
            | (ProviderKind::OpenaiResponses, "https://api.openai.com/v1")
    )
}

impl Providers {
    pub(super) fn credential_scope(&self, cfg: &ProviderConfig) -> Option<[u8; 32]> {
        if !official(cfg) {
            return None;
        }
        // Token refresh may require rediscovery too. Never persist or expose this digest.
        let credential = self.auth.get(&cfg.id)?;
        if !matches!(
            (cfg.kind, &credential),
            (ProviderKind::OpenaiCodex, Credential::Oauth { .. })
                | (ProviderKind::OpenaiResponses, Credential::ApiKey { .. })
        ) {
            return None;
        }
        if cfg.headers.keys().any(|name| {
            name.eq_ignore_ascii_case("authorization")
                || name.eq_ignore_ascii_case("chatgpt-account-id")
        }) {
            return None;
        }
        let bytes =
            serde_json::to_vec(&(&cfg.id, cfg.kind, &cfg.base_url, &cfg.headers, &credential))
                .ok()?;
        Some(Sha256::digest(bytes).into())
    }

    pub(super) fn scope_programs(&self, p: &ProviderState, model: &mut ModelInfo) {
        if p.catalog_scope.is_none() || p.catalog_scope != self.credential_scope(&p.cfg) {
            model.cyber_access_programs.clear();
        }
    }

    pub(super) fn clear_programs(&self, id: &str) -> bool {
        let mut state = self.state.write();
        let mut changed = false;
        if let Some(p) = state.iter_mut().find(|p| p.cfg.id == id) {
            changed = p.catalog_scope.take().is_some();
            for model in &mut p.discovered {
                changed |= !model.cyber_access_programs.is_empty();
                model.cyber_access_programs.clear();
            }
        }
        changed
    }

    pub(super) async fn discover_daybreak(&self, cfg: &ProviderConfig, found: &mut Found) {
        if cfg.kind != ProviderKind::OpenaiResponses || !official(cfg) {
            return;
        }
        let Some(Credential::ApiKey { key }) = self.auth.get(&cfg.id) else {
            return;
        };
        let Some(scope) = self.credential_scope(cfg) else {
            return;
        };
        // The official Codex client uses this catalog for API keys as well.
        // Do not forward arbitrary custom endpoint headers to another origin.
        let mut headers = vec![("authorization".into(), format!("Bearer {key}"))];
        // These documented routing headers keep discovery on the same API project.
        for (name, value) in &cfg.headers {
            if name.eq_ignore_ascii_case("openai-project")
                || name.eq_ignore_ascii_case("openai-organization")
            {
                headers.push((name.clone(), value.clone()));
            }
        }
        let Ok(catalog) = llm::responses::codex_models(&self.http, CODEX_BASE_URL, &headers).await
        else {
            return;
        };
        if Some(scope) != self.credential_scope(cfg) {
            return;
        }
        found.catalog_scope = Some(scope);
        for model in &mut found.models {
            if let Some(entry) = catalog.iter().find(|m| m.id == model.id) {
                model
                    .cyber_access_programs
                    .clone_from(&entry.cyber_access_programs);
            }
        }
    }

    /// The models (`provider/model`) whose account's fresh catalog offers
    /// Blue, OpenAI's deprecated alias last: what to suggest when a chosen
    /// model doesn't offer it.
    pub fn daybreak_models(&self) -> Vec<ModelRef> {
        let mut out: Vec<ModelRef> = self
            .state
            .read()
            .iter()
            .filter(|p| {
                p.catalog_scope.is_some() && p.catalog_scope == self.credential_scope(&p.cfg)
            })
            .flat_map(|p| {
                p.discovered
                    .iter()
                    .filter(|m| llm::responses::daybreak_available(&m.cyber_access_programs))
                    .map(|m| ModelRef {
                        provider: p.cfg.id.clone(),
                        model: m.id.clone(),
                    })
            })
            .collect();
        out.sort_by_key(|m| m.model == DAYBREAK_ALIAS);
        out
    }

    /// The Blue model closest to `model`: the same provider and family (the
    /// part after the last dash, like `sol`), the newest version first; else
    /// that provider's newest. Never the deprecated alias.
    pub fn daybreak_sibling(&self, model: &ModelRef) -> Option<ModelRef> {
        let family = |id: &str| id.rsplit('-').next().unwrap_or("").to_string();
        let version = |id: &str| -> f64 {
            let start = id.find(|c: char| c.is_ascii_digit());
            start
                .map(|s| {
                    let digits: String = id[s..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    digits.trim_end_matches('.').parse().unwrap_or(0.0)
                })
                .unwrap_or(0.0)
        };
        let mut pool: Vec<ModelRef> = self
            .daybreak_models()
            .into_iter()
            .filter(|m| m.provider == model.provider && m.model != DAYBREAK_ALIAS)
            .collect();
        if pool
            .iter()
            .any(|m| family(&m.model) == family(&model.model))
        {
            pool.retain(|m| family(&m.model) == family(&model.model));
        }
        // Equal versions: the one the catalog lists first.
        pool.into_iter()
            .rev()
            .max_by(|a, b| version(&a.model).total_cmp(&version(&b.model)))
    }

    /// Like Codex: the account's fresh catalog decides. A model it lists
    /// programs for must offer Blue; one it lists none for qualifies when the
    /// account has Blue on any model (the request then leaves the choice to
    /// OpenAI, as Codex does).
    pub fn daybreak_available(&self, model: &ModelRef) -> bool {
        let Some(info) = self.model_info(model) else {
            return false;
        };
        if !info.cyber_access_programs.is_empty() {
            return llm::responses::daybreak_available(&info.cyber_access_programs);
        }
        self.state.read().iter().any(|p| {
            p.cfg.id == model.provider
                && p.catalog_scope.is_some()
                && p.catalog_scope == self.credential_scope(&p.cfg)
                && p.discovered
                    .iter()
                    .any(|m| llm::responses::daybreak_available(&m.cyber_access_programs))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(url: &str, kind: ProviderKind) -> ProviderConfig {
        ProviderConfig {
            id: "openai".into(),
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

    #[test]
    fn daybreak_is_restricted_to_exact_official_endpoints() {
        for (url, kind, expected) in [
            (CODEX_BASE_URL, ProviderKind::OpenaiCodex, true),
            (
                "https://api.openai.com/v1/",
                ProviderKind::OpenaiResponses,
                true,
            ),
            (
                "https://api.openai.com.example/v1",
                ProviderKind::OpenaiResponses,
                false,
            ),
            (
                "https://api.openai.com/v1/other",
                ProviderKind::OpenaiResponses,
                false,
            ),
            (
                "http://127.0.0.1:18760/v1",
                ProviderKind::OpenaiResponses,
                false,
            ),
            (CODEX_BASE_URL, ProviderKind::OpenaiChat, false),
        ] {
            assert_eq!(official(&configured(url, kind)), expected);
        }
    }

    #[test]
    fn daybreak_discovery_is_scoped_to_current_credentials_and_not_restored_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = configured("https://api.openai.com/v1", ProviderKind::OpenaiResponses);
        let auth = Arc::new(AuthStore::load(dir.path().join("auth.json")).unwrap());
        auth.set(
            &cfg.id,
            Credential::ApiKey {
                key: "fixture-one".into(),
            },
        )
        .unwrap();
        let events = tokio::sync::broadcast::channel(16).0;
        let cache = dir.path().join("caps.json");
        let providers = Providers::new(
            reqwest::Client::new(),
            auth.clone(),
            std::slice::from_ref(&cfg),
        )
        .remembering(cache.clone(), events.clone());
        let model = ModelRef {
            provider: cfg.id.clone(),
            model: "gpt-6-sol".into(),
        };
        let mut info = default_info(&cfg, &model.model, None);
        info.cyber_access_programs = vec!["standard".into(), "daybreak_blue".into()];
        {
            let mut state = providers.state.write();
            state[0].discovered = vec![info];
            state[0].catalog_scope = providers.credential_scope(&cfg);
        }
        assert!(providers.daybreak_available(&model));
        // A model the catalog lists no programs for qualifies when the account
        // has Blue on another (Codex leaves the choice to OpenAI then).
        let other = ModelRef {
            provider: cfg.id.clone(),
            model: "gpt-without-programs".into(),
        };
        assert!(providers.daybreak_available(&other));
        assert!(
            !providers.infos()[0].models[0]
                .cyber_access_programs
                .is_empty()
        );
        providers.save_lists();
        let restarted = Providers::new(
            reqwest::Client::new(),
            auth.clone(),
            std::slice::from_ref(&cfg),
        )
        .remembering(cache, events);
        assert!(!restarted.daybreak_available(&model));
        assert!(!restarted.daybreak_available(&other));
        assert!(
            restarted.infos()[0].models[0]
                .cyber_access_programs
                .is_empty()
        );
        auth.set(
            &cfg.id,
            Credential::ApiKey {
                key: "fixture-two".into(),
            },
        )
        .unwrap();
        assert!(!providers.daybreak_available(&model));
        assert!(
            providers.infos()[0].models[0]
                .cyber_access_programs
                .is_empty()
        );
        auth.remove(&cfg.id).unwrap();
        assert!(!providers.daybreak_available(&model));
    }

    /// An account's catalog may list Blue on gpt-6-sol and older models but
    /// only `standard` on gpt-6.1-sol: helpers and the error must point at
    /// what the catalog offers.
    #[test]
    fn the_closest_blue_model_is_suggested_from_the_accounts_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = configured(CODEX_BASE_URL, ProviderKind::OpenaiCodex);
        let auth = Arc::new(AuthStore::load(dir.path().join("auth.json")).unwrap());
        auth.set(
            &cfg.id,
            Credential::Oauth {
                access: "fixture-access".into(),
                refresh: "fixture-refresh".into(),
                expires: u64::MAX,
                account_id: None,
                email: None,
            },
        )
        .unwrap();
        let providers = Providers::new(reqwest::Client::new(), auth, std::slice::from_ref(&cfg));
        let listed = |id: &str, programs: &[&str]| {
            let mut info = default_info(&cfg, id, None);
            info.cyber_access_programs = programs.iter().map(|p| p.to_string()).collect();
            info
        };
        let both = ["standard", "daybreak_blue"];
        {
            let mut state = providers.state.write();
            state[0].discovered = vec![
                listed("gpt-6.1-sol", &["standard"]),
                listed("gpt-6.1-luna", &["standard"]),
                listed("gpt-daybreak-blue-latest", &both),
                listed("gpt-5.6-sol", &both),
                listed("gpt-6-sol", &both),
                listed("gpt-6-luna", &both),
            ];
            state[0].catalog_scope = providers.credential_scope(&cfg);
        }
        let at = |model: &str| ModelRef {
            provider: cfg.id.clone(),
            model: model.into(),
        };
        assert!(!providers.daybreak_available(&at("gpt-6.1-sol")));
        assert_eq!(
            providers.daybreak_sibling(&at("gpt-6.1-sol")),
            Some(at("gpt-6-sol"))
        );
        assert_eq!(
            providers.daybreak_sibling(&at("gpt-6.1-luna")),
            Some(at("gpt-6-luna"))
        );
        // Another family: the newest that has it, never the old alias.
        assert_eq!(
            providers.daybreak_sibling(&at("gpt-6.1-terra")),
            Some(at("gpt-6-sol"))
        );
        let offered: Vec<String> = providers
            .daybreak_models()
            .iter()
            .map(|m| m.model.clone())
            .collect();
        assert_eq!(
            offered.last().map(String::as_str),
            Some("gpt-daybreak-blue-latest")
        );
        assert_eq!(offered.len(), 4);
        // Another account's catalog suggests nothing.
        providers.state.write()[0].catalog_scope = None;
        assert!(providers.daybreak_models().is_empty());
        assert_eq!(providers.daybreak_sibling(&at("gpt-6.1-sol")), None);
    }
}
