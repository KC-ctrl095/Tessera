//! Several subscriptions of one kind (batch 17): "I know a lot of people do
//! this". More ChatGPT sign-ins, each its own provider entry and login (the
//! same device-code sign-in as the usual one), and more Claude accounts, each
//! with its own Claude Code folder (`CLAUDE_CONFIG_DIR`, see
//! `claude_code::Account`), signed in by the user through Claude Code's own
//! login: Tessera never reads, writes or passes on a Claude credential. Each
//! account has the name the user picks, its own models, usage and limits; a
//! chat uses an account by its model (`provider/model`). With `[accounts]
//! continue_on_next`, a chat whose account reached its usage limit carries on
//! with the same model on the next signed-in account of that kind that hasn't.

use super::*;
use crate::claude_code as cc;

/// Accounts of one kind at most (the usual one included).
const MAX_ACCOUNTS: usize = 8;

/// What a subscription's entry is called: the user's name, saying which
/// service it is ("Work" → "ChatGPT (Work)") unless it already does.
pub(super) fn account_label(kind: ProviderKind, name: &str) -> String {
    let service = match kind {
        ProviderKind::OpenaiCodex => "ChatGPT",
        ProviderKind::ClaudeCode => "Claude",
        _ => return name.to_string(),
    };
    if name.to_lowercase().contains(&service.to_lowercase()) {
        name.to_string()
    } else {
        format!("{service} ({name})")
    }
}

/// A provider id's part from a name: lowercase letters, digits and dashes.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    out.chars()
        .take(24)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

impl Inner {
    /// `add_account`: another ChatGPT or Claude subscription, named by the user.
    pub(super) async fn add_account(
        &self,
        kind: ProviderKind,
        name: &str,
    ) -> Result<ProviderInfo, ApiError> {
        let name = one_line(name, 40);
        if name.is_empty() {
            return Err(bad("give the account a name, such as Work"));
        }
        let (base, mut cfg) = match kind {
            ProviderKind::OpenaiCodex => (CODEX_PROVIDER, crate::providers::codex_preset()),
            ProviderKind::ClaudeCode => (cc::PROVIDER, cc::preset()),
            _ => {
                return Err(bad(
                    "only ChatGPT and Claude subscriptions take more accounts; add another service with its own key",
                ));
            }
        };
        let configs = self.providers.configs();
        // Another Claude account runs the same `claude` as the usual one, with its own folder.
        if let Some(usual) = configs
            .iter()
            .find(|c| c.id == cc::PROVIDER && kind == ProviderKind::ClaudeCode)
        {
            cfg.base_url = usual.base_url.clone();
        }
        if configs.iter().filter(|c| c.kind == kind).count() >= MAX_ACCOUNTS {
            return Err(bad(format!(
                "{MAX_ACCOUNTS} accounts of one kind at most; remove one first"
            )));
        }
        // An id of its own, from the name ("openai-codex-work"), never another's.
        let taken = |id: &str| configs.iter().any(|c| c.id == id);
        let stem = match slug(&name) {
            s if s.is_empty() => base.to_string(),
            s => format!("{base}-{s}"),
        };
        let id = (1..)
            .map(|n| {
                if n == 1 {
                    stem.clone()
                } else {
                    format!("{stem}-{n}")
                }
            })
            .find(|id| id != base && !taken(id))
            .expect("a free id");
        cfg.id = id.clone();
        cfg.name = account_label(kind, &name);
        if kind == ProviderKind::ClaudeCode {
            // Only where Claude Code is installed (its usual account says).
            let account = self.providers.claude_account(&cfg);
            let d = self.claude_code_detect(&account).await;
            if d.path.is_none() {
                return Err(bad(cc::info(
                    &d,
                    &crate::node::hostname(),
                    cc::PROVIDER,
                    &account,
                )
                .hint));
            }
        }
        self.providers.upsert(cfg);
        self.persist_providers().map_err(internal)?;
        let _ = self.events.send(Event::ProvidersChanged);
        self.providers
            .infos()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| internal("the account vanished"))
    }

    /// `rename_provider`: the name the user calls it (a subscription's says
    /// which service it is).
    pub(super) fn rename_provider(&self, id: &str, name: &str) -> Result<ProviderInfo, ApiError> {
        let name = one_line(name, 60);
        if name.is_empty() {
            return Err(bad("give it a name"));
        }
        let mut cfg = self
            .providers
            .configs()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no provider `{id}`")))?;
        cfg.name = account_label(cfg.kind, &name);
        // `upsert` would forget the models it listed: only the name changes.
        self.providers.rename(id, &cfg.name);
        self.persist_providers().map_err(internal)?;
        let _ = self.events.send(Event::ProvidersChanged);
        self.providers
            .infos()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no provider `{id}`")))
    }

    pub(super) fn account_settings(&self) -> AccountSettings {
        AccountSettings {
            continue_on_next: self.config.read().accounts.continue_on_next,
        }
    }

    /// Another ChatGPT entry signed in to the same ChatGPT account as
    /// `cred` (its name): the two share one plan.
    pub(super) fn same_chatgpt_account(&self, provider: &str, cred: &Credential) -> Option<String> {
        let Credential::Oauth {
            account_id: Some(mine),
            ..
        } = cred
        else {
            return None;
        };
        self.providers
            .configs()
            .into_iter()
            .filter(|c| c.kind == ProviderKind::OpenaiCodex && c.id != provider)
            .find(|c| {
                matches!(self.auth.get(&c.id), Some(Credential::Oauth { account_id: Some(a), .. }) if a == *mine)
            })
            .map(|c| if c.name.is_empty() { c.id } else { c.name })
    }

    /// Whether `provider` reached a usage limit that applies to `model` and
    /// hasn't reset yet (as it last reported).
    fn account_full(&self, provider: &str, model: &str) -> bool {
        let now = now_ms();
        let model = model.to_ascii_lowercase();
        self.providers
            .plan_limits()
            .get(provider, now)
            .iter()
            .any(|l| {
                let reached = l.status == Some(LimitStatus::Reached)
                    || l.used_percent.is_some_and(|p| p >= 100.0);
                // A model's own weekly limit (`weekly_opus`) only for that model.
                let applies = match l.window.strip_prefix("weekly_") {
                    Some(family) => model.contains(family),
                    None => true,
                };
                reached && applies && l.resets_at.is_none_or(|at| at > now)
            })
    }

    /// The account a chat on `current` moves to when that one reached its
    /// usage limit: the next of the same kind (in the user's order) that is
    /// signed in to its plan, offers the same model, hasn't reached its own
    /// limit and isn't in `tried`. Only with `[accounts] continue_on_next`.
    pub(super) fn next_account(&self, current: &ModelRef, tried: &[String]) -> Option<ModelRef> {
        if !self.config.read().accounts.continue_on_next {
            return None;
        }
        let kind = self.providers.kind_of(&current.provider)?;
        if !matches!(kind, ProviderKind::OpenaiCodex | ProviderKind::ClaudeCode) {
            return None;
        }
        let configs = self.providers.configs();
        let infos = self.providers.infos();
        let same: Vec<&ProviderConfig> = configs.iter().filter(|c| c.kind == kind).collect();
        let at = same.iter().position(|c| c.id == current.provider)?;
        same[at + 1..]
            .iter()
            .chain(same[..at].iter())
            .find(|c| {
                let ready = infos.iter().any(|p| {
                    p.id == c.id
                        && p.authenticated
                        && p.models.iter().any(|m| m.id == current.model)
                });
                // A Claude account signed in with an API key is billed per token: never moved to.
                let plan = kind != ProviderKind::ClaudeCode
                    || self
                        .providers
                        .claude_code(&self.providers.claude_account(c).key())
                        .is_some_and(|d| d.signed_in && !d.api_key);
                ready && plan && !tried.contains(&c.id) && !self.account_full(&c.id, &current.model)
            })
            .map(|c| ModelRef {
                provider: c.id.clone(),
                model: current.model.clone(),
            })
    }

    /// The chat goes on with `to` (another account's same model) from now
    /// on, and says so.
    pub(super) fn move_to_account(&self, rt: &Rt, from: &ModelRef, to: &ModelRef) -> bool {
        {
            let mut st = rt.st.lock();
            // A room speaks with its bots' models: nothing to move.
            if self.effective_model(&st) != *from || st.info.model != *from {
                return false;
            }
            st.info.model = to.clone();
            if let Some(info) = self.providers.model_info(to) {
                st.info.context.window = self.working_window(to, info.context_window);
            }
            st.info.updated_at = now_ms();
        }
        self.save(rt);
        let name = |id: &str| {
            self.providers
                .infos()
                .into_iter()
                .find(|p| p.id == id)
                .map(|p| p.name)
                .unwrap_or_else(|| id.to_string())
        };
        self.notice(
            rt,
            NoticeLevel::Warning,
            format!(
                "{} reached its usage limit; this chat goes on with {} (same model).",
                name(&from.provider),
                name(&to.provider)
            ),
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    fn signed_in(e: &Engine, id: &str, account: &str) {
        e.0.auth
            .set(
                id,
                Credential::Oauth {
                    access: "a".into(),
                    refresh: "r".into(),
                    expires: now_ms() + 3_600_000,
                    account_id: Some(account.into()),
                    email: None,
                },
            )
            .unwrap();
    }

    /// Which account a chat moves to when its own reached its limit: the
    /// next signed-in one of the same kind with the same model and room
    /// left; none unless the user turned it on.
    #[tokio::test]
    async fn the_next_account_is_signed_in_has_the_model_and_room_left() {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let inner = &e.0;
        let work = inner
            .add_account(ProviderKind::OpenaiCodex, "Work")
            .await
            .unwrap();
        let home = inner
            .add_account(ProviderKind::OpenaiCodex, "Home")
            .await
            .unwrap();
        assert_eq!(
            (work.id.as_str(), work.name.as_str()),
            ("openai-codex-work", "ChatGPT (Work)")
        );
        // The same name again: an id of its own.
        let again = inner
            .add_account(ProviderKind::OpenaiCodex, "Work")
            .await
            .unwrap();
        assert_eq!(again.id, "openai-codex-work-2");
        assert!(
            inner
                .add_account(ProviderKind::Anthropic, "Keys")
                .await
                .is_err()
        );
        let model = ModelRef {
            provider: CODEX_PROVIDER.into(),
            model: "gpt-5.5".into(),
        };
        signed_in(&e, CODEX_PROVIDER, "acct-1");
        signed_in(&e, &home.id, "acct-3");
        // Off by default.
        assert_eq!(inner.next_account(&model, &[]), None);
        inner.config.write().accounts.continue_on_next = true;
        // Work isn't signed in: Home is next.
        let next = inner.next_account(&model, &[]).unwrap();
        assert_eq!(next.provider, home.id);
        assert_eq!(next.model, "gpt-5.5");
        signed_in(&e, &work.id, "acct-2");
        assert_eq!(inner.next_account(&model, &[]).unwrap().provider, work.id);
        // Work reached its 5-hour limit: Home.
        inner.providers.plan_limits().record(
            &work.id,
            vec![PlanLimit {
                window: "five_hour".into(),
                label: "5-hour limit".into(),
                used_percent: Some(100.0),
                status: None,
                resets_at: Some(now_ms() + 3_600_000),
                updated_at: now_ms(),
            }],
            true,
        );
        assert_eq!(inner.next_account(&model, &[]).unwrap().provider, home.id);
        // Tried already (this turn moved away from it): none left.
        assert_eq!(
            inner.next_account(&model, std::slice::from_ref(&home.id)),
            None
        );
        // Round the list: from Home back to the first.
        let on_home = ModelRef {
            provider: home.id.clone(),
            model: "gpt-5.5".into(),
        };
        assert_eq!(
            inner.next_account(&on_home, &[]).unwrap().provider,
            CODEX_PROVIDER
        );
        // A model another account doesn't have, or another kind of provider: none.
        let other = ModelRef {
            provider: CODEX_PROVIDER.into(),
            model: "no-such-model".into(),
        };
        assert_eq!(inner.next_account(&other, &[]), None);
        // The same ChatGPT account signed in twice is noticed.
        let twin = Credential::Oauth {
            access: "a".into(),
            refresh: "r".into(),
            expires: 0,
            account_id: Some("acct-3".into()),
            email: None,
        };
        assert_eq!(
            inner.same_chatgpt_account(&again.id, &twin).as_deref(),
            Some("ChatGPT (Home)")
        );
        // Renamed: only its name changes.
        let renamed = inner.rename_provider(&home.id, "Family").unwrap();
        assert_eq!(renamed.name, "ChatGPT (Family)");
        assert_eq!(renamed.models.len(), home.models.len());
        // Kept in config.toml, with the setting.
        let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            saved.contains("openai-codex-work") && saved.contains("ChatGPT (Family)"),
            "{saved}"
        );
    }

    #[test]
    fn names_say_which_service_and_ids_come_from_them() {
        assert_eq!(
            account_label(ProviderKind::OpenaiCodex, "Work"),
            "ChatGPT (Work)"
        );
        assert_eq!(
            account_label(ProviderKind::ClaudeCode, "Claude Max"),
            "Claude Max"
        );
        assert_eq!(
            account_label(ProviderKind::ClaudeCode, "Family"),
            "Claude (Family)"
        );
        assert_eq!(
            account_label(ProviderKind::OpenaiChat, "Home GPU"),
            "Home GPU"
        );
        assert_eq!(slug("  My Work / Plan! "), "my-work-plan");
        assert_eq!(slug("Ünïcode"), "n-code");
        assert_eq!(slug("!!!"), "");
        assert!(slug(&"x".repeat(80)).len() <= 24);
    }
}
