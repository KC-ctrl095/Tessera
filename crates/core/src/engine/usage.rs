//! Usage stats: one row per model response (`Store::record_usage`), summed
//! for `get_usage` and priced there with today's prices, so a price set
//! later applies to past requests too.

use super::*;
use crate::config::SubscriptionConfig;
use crate::pricing::{self, CLAUDE_CODE};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// An average month, for prorating plan fees.
const MONTH_MS: f64 = 30.436_875 * DAY_MS as f64;

/// How a model is paid for and its price now.
#[derive(Clone, Copy)]
struct Rate {
    /// `None`: its provider is gone.
    billing: Option<Billing>,
    price: Option<(Price, PriceSource)>,
    /// Billed is what was recorded with each request: Claude Code bills
    /// per token only when it runs on an API key, which only it knows.
    recorded: bool,
}

impl Rate {
    /// (billed, API-equivalent) cost of a group of requests.
    fn costs(&self, g: &crate::store::UsageGroup) -> (Option<f64>, Option<f64>) {
        let t = &g.totals;
        let custom = matches!(self.price, Some((_, PriceSource::Custom)));
        let price = self.price.map(|(p, _)| p);
        let api = match g.reported {
            // A provider's own figure (Claude Code) unless the user set a price.
            Some((cost, [i, o, r, w])) if !custom => {
                let rest = price.map(|p| {
                    p.cost_of(
                        t.input.saturating_sub(i),
                        t.output.saturating_sub(o),
                        t.cache_read.saturating_sub(r),
                        t.cache_write.saturating_sub(w),
                    )
                });
                Some(cost + rest.unwrap_or(0.0))
            }
            _ => price.map(|p| p.cost_of(t.input, t.output, t.cache_read, t.cache_write)),
        };
        let billed = match self.billing {
            _ if self.recorded => t.cost_usd,
            Some(Billing::Subscription) => None,
            Some(_) => api,
            // Gone: what was recorded then.
            None => t.cost_usd,
        };
        (billed, api)
    }
}

impl Inner {
    /// What a session's own turns count as.
    pub(super) fn usage_kind(info: &SessionInfo) -> &'static str {
        match info.kind {
            _ if info.internal => "memory",
            SessionKind::Subagent => "subagent",
            SessionKind::Job => "job",
            SessionKind::Chat | SessionKind::Channel => "chat",
        }
    }

    /// Record a model response (not for private chats, not when it used
    /// nothing, e.g. stopped before the first token). Never fails the caller.
    pub(super) fn log_usage(
        &self,
        session_id: Option<&str>,
        model: &ModelRef,
        kind: &str,
        usage: &Usage,
    ) {
        self.log_usage_reported(session_id, model, kind, usage, None);
    }

    /// [`log_usage`](Self::log_usage) with the API-equivalent cost the
    /// provider reported itself (Claude Code's `total_cost_usd`), which the
    /// usage stats use instead of the price list.
    pub(super) fn log_usage_reported(
        &self,
        session_id: Option<&str>,
        model: &ModelRef,
        kind: &str,
        usage: &Usage,
        api_cost: Option<f64>,
    ) {
        if usage.input + usage.output + usage.cache_read + usage.cache_write == 0 {
            return;
        }
        let record = crate::store::UsageRecord {
            session_id,
            model,
            kind,
            usage,
            api_cost: api_cost.filter(|c| c.is_finite() && *c >= 0.0),
        };
        if let Err(e) = self.store.record_usage(&record) {
            tracing::warn!("recording usage: {e}");
        }
        // Bridged helpers are separate usage rows, but their chat's counter
        // includes them too. Claude Code's own Agent usage already includes
        // its children, so only Tessera-run helpers contribute here.
        let parent = session_id.and_then(|id| self.rt(id).ok()).and_then(|rt| {
            let st = rt.st.lock();
            (st.info.parent_call.is_some() && st.internal.subagent.is_some())
                .then(|| st.info.parent_id.clone())
                .flatten()
        });
        if let Some(parent) = parent.and_then(|id| self.rt(&id).ok()) {
            {
                let mut st = parent.st.lock();
                let u = &mut st.info.usage;
                u.input += usage.input;
                u.output += usage.output;
                u.cache_read += usage.cache_read;
                u.cache_write += usage.cache_write;
                if let Some(cost) = usage
                    .cost_usd
                    .or_else(|| self.price_of(model).map(|p| p.cost(usage)))
                {
                    *u.cost_usd.get_or_insert(0.0) += cost;
                }
            }
            // Its context counter is still the parent model's window, not
            // the helper's (which may have an entirely different size).
            self.save(&parent);
        }
    }

    /// How `provider`'s `model` is paid for and its price now.
    fn rate(
        &self,
        configs: &[ProviderConfig],
        config: &Config,
        provider: &str,
        model: &str,
    ) -> Rate {
        let m = ModelRef {
            provider: provider.into(),
            model: model.into(),
        };
        let cfg = configs.iter().find(|c| c.id == provider);
        // A Claude account signed in to Claude Code with an API key (or a cloud
        // provider) is billed per token, not to a plan.
        let claude_api_key = |cfg: &ProviderConfig| {
            self.providers
                .claude_code(&self.providers.claude_account(cfg).key())
                .is_some_and(|d| d.signed_in && d.api_key)
        };
        let billing = match cfg {
            Some(cfg)
                if (cfg.id == CLAUDE_CODE || cfg.kind == ProviderKind::ClaudeCode)
                    && claude_api_key(cfg) =>
            {
                Some(Billing::Api)
            }
            Some(cfg) => Some(pricing::billing(
                cfg,
                self.auth.get(provider).as_ref(),
                config,
            )),
            None if provider == CLAUDE_CODE => Some(Billing::Subscription),
            None => None,
        };
        let home = match cfg {
            Some(cfg) => pricing::catalog_home(cfg),
            None => (provider == CLAUDE_CODE).then_some("anthropic"),
        };
        // Local servers cost only what the user says they do.
        let hosted = billing != Some(Billing::Local);
        let price = config.price_with_source(&m, hosted).or_else(|| {
            (hosted && config.pricing.catalog)
                .then(|| self.catalog.lookup(home, model))
                .flatten()
                .map(|p| (p, PriceSource::Catalog))
        });
        Rate {
            billing,
            price,
            // Any Claude account's requests: Claude Code reports their cost.
            recorded: provider == CLAUDE_CODE
                || cfg.is_some_and(|c| c.kind == ProviderKind::ClaudeCode),
        }
    }

    /// What a response from `m` is billed per token (`Usage::cost_usd`):
    /// nothing for plans, and only prices the user set for local servers.
    pub(super) fn billed_price(&self, m: &ModelRef) -> Option<Price> {
        let configs = self.providers.configs();
        let rate = self.rate(&configs, &self.config.read(), &m.provider, &m.model);
        match rate.billing {
            Some(Billing::Subscription) => None,
            _ => rate.price.map(|(p, _)| p),
        }
    }

    /// `get_usage`, `set_subscription` and `set_model_price`.
    pub(super) async fn usage_request(&self, req: Request) -> ApiResult {
        match req {
            Request::GetUsage {
                from,
                to,
                utc_offsets,
            } => {
                check_offsets(&utc_offsets)?;
                let report = self.usage_report(from, to, &utc_offsets)?;
                let (enabled, url) = {
                    let c = self.config.read();
                    (c.pricing.catalog, c.pricing.catalog_url.clone())
                };
                let from_catalog = report
                    .by_model
                    .iter()
                    .any(|m| m.price_source == Some(PriceSource::Catalog));
                if !enabled
                    || (report.unpriced.is_empty() && !from_catalog)
                    || !self.catalog.stale()
                {
                    return to_value(report);
                }
                let (catalog, http) = (self.catalog.clone(), self.providers.http().clone());
                let task = tokio::spawn(async move {
                    let r = catalog.refresh(&http, &url).await;
                    if let Err(e) = &r {
                        tracing::info!("price catalog: {e:#}");
                    }
                    r.unwrap_or(false)
                });
                // Missing prices: wait a little so this look already has
                // them; a slow download finishes for the next one.
                if !report.unpriced.is_empty()
                    && let Ok(Ok(true)) =
                        tokio::time::timeout(std::time::Duration::from_secs(4), task).await
                {
                    return to_value(self.usage_report(from, to, &utc_offsets)?);
                }
                to_value(report)
            }
            Request::SetSubscription {
                provider,
                plan,
                monthly_usd,
            } => {
                let provider = provider.trim().to_string();
                if !(self.providers.exists(&provider) || provider == CLAUDE_CODE) {
                    return Err(bad(format!("there is no provider `{provider}`")));
                }
                let plan = plan
                    .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|p| !p.is_empty());
                if plan.as_ref().is_some_and(|p| p.chars().count() > 80) {
                    return Err(bad("the plan's name is too long (80 characters at most)"));
                }
                if monthly_usd.is_some_and(|m| !(m.is_finite() && (0.0..=100_000.0).contains(&m))) {
                    return Err(bad("the monthly fee is US dollars, from 0 to 100000"));
                }
                let mut c = self.config.write();
                if plan.is_none() && monthly_usd.is_none() {
                    c.pricing.subscriptions.remove(&provider);
                } else {
                    c.pricing
                        .subscriptions
                        .insert(provider, SubscriptionConfig { plan, monthly_usd });
                }
                c.save(&self.paths).map_err(internal)?;
                Ok(Value::Null)
            }
            Request::SetModelPrice {
                provider,
                model,
                price,
            } => {
                let (provider, model) = (provider.trim(), model.trim());
                if provider.is_empty() || model.is_empty() || provider.contains('/') {
                    return Err(bad("name the model as a provider id and a model id"));
                }
                if price.is_some_and(|p| !p.is_valid()) {
                    return Err(bad(
                        "prices are US dollars per million tokens, from 0 to 100000",
                    ));
                }
                let key = format!("{provider}/{model}");
                let mut c = self.config.write();
                match price {
                    Some(p) => c.prices.insert(key, p),
                    None => c.prices.remove(&key),
                };
                c.save(&self.paths).map_err(internal)?;
                Ok(Value::Null)
            }
            _ => Err(bad("not a usage request")),
        }
    }

    /// Usage from `from` to `to`, by the asking device's days (`offsets`,
    /// see `Request::GetUsage`; empty: this computer's).
    pub(super) fn usage_report(
        &self,
        from: Option<u64>,
        to: Option<u64>,
        offsets: &[UtcOffset],
    ) -> Result<UsageReport, ApiError> {
        let to = to.unwrap_or_else(now_ms);
        let from = from.unwrap_or(to.saturating_sub(30 * DAY_MS));
        if from > to {
            return Err(bad("`from` is after `to`"));
        }
        let groups = self
            .store
            .usage_groups(from, to, offsets)
            .map_err(internal)?;
        let configs = self.providers.configs();
        let config = self.config.read().clone();
        let mut rates: HashMap<(String, String), Rate> = HashMap::new();
        let mut total = UsageTotals::default();
        let mut by_model: Vec<ModelUsage> = Vec::new();
        let mut by_kind: Vec<KindUsage> = Vec::new();
        let mut by_day: Vec<DayUsage> = Vec::new();
        let model_slot =
            |list: &mut Vec<ModelUsage>, g: &crate::store::UsageGroup, billing| -> usize {
                match list
                    .iter()
                    .position(|m| m.provider == g.provider && m.model == g.model)
                {
                    Some(i) => i,
                    None => {
                        list.push(ModelUsage {
                            provider: g.provider.clone(),
                            model: g.model.clone(),
                            totals: UsageTotals::default(),
                            billing,
                            price: None,
                            price_source: None,
                        });
                        list.len() - 1
                    }
                }
            };
        for g in &groups {
            let rate = *rates
                .entry((g.provider.clone(), g.model.clone()))
                .or_insert_with(|| self.rate(&configs, &config, &g.provider, &g.model));
            let mut t = g.totals.clone();
            (t.cost_usd, t.api_cost_usd) = rate.costs(g);
            add_totals(&mut total, &t);
            let i = model_slot(&mut by_model, g, rate.billing);
            add_totals(&mut by_model[i].totals, &t);
            match by_kind.iter_mut().find(|k| k.kind == g.kind) {
                Some(k) => add_totals(&mut k.totals, &t),
                None => by_kind.push(KindUsage {
                    kind: g.kind.clone(),
                    totals: t.clone(),
                }),
            }
            // Groups come ordered by day.
            if by_day.last().is_none_or(|d| d.day != g.day) {
                by_day.push(DayUsage {
                    day: g.day.clone(),
                    totals: UsageTotals::default(),
                    models: Vec::new(),
                });
            }
            let day = by_day.last_mut().expect("just pushed");
            add_totals(&mut day.totals, &t);
            let i = model_slot(&mut day.models, g, rate.billing);
            add_totals(&mut day.models[i].totals, &t);
        }
        let mut unpriced = Vec::new();
        for m in &mut by_model {
            let rate = rates[&(m.provider.clone(), m.model.clone())];
            (m.price, m.price_source) = match rate.price {
                Some((p, s)) => (Some(p), Some(s)),
                None => (None, None),
            };
            let reported = m.totals.api_cost_usd.is_some();
            if rate.price.is_none()
                && !reported
                && matches!(rate.billing, Some(Billing::Api | Billing::Subscription))
            {
                unpriced.push(format!("{}/{}", m.provider, m.model));
            }
        }
        unpriced.sort();
        let weight = |t: &UsageTotals| t.input + t.output + t.cache_read + t.cache_write;
        by_model.sort_by_key(|m| std::cmp::Reverse(weight(&m.totals)));
        by_kind.sort_by_key(|k| std::cmp::Reverse(k.totals.requests));
        for d in &mut by_day {
            d.models
                .sort_by_key(|m| std::cmp::Reverse(weight(&m.totals)));
        }
        // A plan's fee counts from the first recorded request: "All" (from 0)
        // or a range reaching back before Tessera was used isn't billed for
        // time nobody used it.
        let start = match self.store.first_usage().map_err(internal)? {
            Some(first) => from.max(first.min(to)),
            None => to,
        };
        let subscriptions = self.subscriptions(&configs, &config, &by_model, to - start);
        Ok(UsageReport {
            from,
            to,
            total,
            by_model,
            by_kind,
            by_day,
            unpriced,
            subscriptions,
        })
    }

    /// Providers billed by plan that are signed in (Claude Code: set up and
    /// signed in), used in the period, have a plan set, or reported limits
    /// that haven't reset yet, with the plan's share of the period and its
    /// limits.
    fn subscriptions(
        &self,
        configs: &[ProviderConfig],
        config: &Config,
        by_model: &[ModelUsage],
        period_ms: u64,
    ) -> Vec<SubscriptionInfo> {
        let infos = self.providers.infos();
        let now = now_ms();
        let limits_of = |id: &str| {
            let l = self.providers.plan_limits().get(id, now);
            let current = l.iter().any(|l| l.resets_at.is_some_and(|at| at > now));
            (l, current)
        };
        let mut out = Vec::new();
        for cfg in configs {
            let set = config.pricing.subscriptions.get(&cfg.id);
            let used: Vec<&ModelUsage> = by_model.iter().filter(|m| m.provider == cfg.id).collect();
            let plan = pricing::billing(cfg, self.auth.get(&cfg.id).as_ref(), config)
                == Billing::Subscription;
            let ready = infos.iter().any(|p| p.id == cfg.id && p.authenticated);
            let (limits, current) = limits_of(&cfg.id);
            if !plan || (set.is_none() && used.is_empty() && !ready && !current) {
                continue;
            }
            let label = infos.iter().find(|p| p.id == cfg.id).map_or_else(
                || {
                    if cfg.name.is_empty() {
                        cfg.id.clone()
                    } else {
                        cfg.name.clone()
                    }
                },
                |p| p.name.clone(),
            );
            out.push(subscription(
                cfg.id.clone(),
                label,
                set,
                &used,
                period_ms,
                limits,
            ));
        }
        // Claude Code's usage is kept when it isn't set up any more.
        if !out.iter().any(|s| s.provider == CLAUDE_CODE) {
            let set = config.pricing.subscriptions.get(CLAUDE_CODE);
            let used: Vec<&ModelUsage> = by_model
                .iter()
                .filter(|m| m.provider == CLAUDE_CODE)
                .collect();
            let (limits, current) = limits_of(CLAUDE_CODE);
            if set.is_some() || !used.is_empty() || current {
                let label = "Claude (your subscription)".to_string();
                out.push(subscription(
                    CLAUDE_CODE.into(),
                    label,
                    set,
                    &used,
                    period_ms,
                    limits,
                ));
            }
        }
        out
    }
}

/// A device's time zone as `get_usage` takes it: a few changes of offset
/// (summer time, twice a year), each a real offset (±18 h), in order.
fn check_offsets(offsets: &[UtcOffset]) -> Result<(), ApiError> {
    if offsets.len() > 400 {
        return Err(bad("`utc_offsets`: at most 400 changes"));
    }
    if offsets
        .iter()
        .any(|o| !(-18 * 60..=18 * 60).contains(&o.minutes))
    {
        return Err(bad(
            "`utc_offsets`: offsets are minutes from UTC, at most 18 hours",
        ));
    }
    if offsets.windows(2).any(|w| w[1].since < w[0].since) {
        return Err(bad("`utc_offsets`: oldest first"));
    }
    Ok(())
}

fn subscription(
    provider: String,
    label: String,
    set: Option<&SubscriptionConfig>,
    used: &[&ModelUsage],
    period_ms: u64,
    limits: Vec<PlanLimit>,
) -> SubscriptionInfo {
    let monthly_usd = set.and_then(|s| s.monthly_usd);
    let api: Vec<f64> = used.iter().filter_map(|m| m.totals.api_cost_usd).collect();
    SubscriptionInfo {
        provider,
        label,
        plan: set.and_then(|s| s.plan.clone()),
        monthly_usd,
        period_usd: monthly_usd.map(|m| m * period_ms as f64 / MONTH_MS),
        requests: used.iter().map(|m| m.totals.requests).sum(),
        api_cost_usd: (!api.is_empty()).then(|| api.iter().sum()),
        limits,
    }
}

fn add_totals(a: &mut UsageTotals, b: &UsageTotals) {
    a.requests += b.requests;
    a.input += b.input;
    a.output += b.output;
    a.cache_read += b.cache_read;
    a.cache_write += b.cache_write;
    if let Some(c) = b.cost_usd {
        *a.cost_usd.get_or_insert(0.0) += c;
    }
    if let Some(c) = b.api_cost_usd {
        *a.api_cost_usd.get_or_insert(0.0) += c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::UsageGroup;

    fn group(input: u64, output: u64, read: u64, write: u64) -> UsageGroup {
        UsageGroup {
            day: "2026-10-01".into(),
            provider: "p".into(),
            model: "m".into(),
            kind: "chat".into(),
            totals: UsageTotals {
                requests: 1,
                input,
                output,
                cache_read: read,
                cache_write: write,
                cost_usd: Some(99.0),
                api_cost_usd: None,
            },
            reported: None,
        }
    }

    fn rate(billing: Option<Billing>, price: Option<(Price, PriceSource)>) -> Rate {
        Rate {
            billing,
            price,
            recorded: false,
        }
    }

    const M: u64 = 1_000_000;

    #[test]
    fn costs_follow_billing_cache_rates_and_reported_figures() {
        let sonnet = Price::new(3.0, 15.0, 0.3, 3.75);
        let g = group(M, M, 10 * M, 2 * M);
        let all = 3.0 + 15.0 + 10.0 * 0.3 + 2.0 * 3.75;
        let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        // Per token: billed is what it costs at API prices.
        let (billed, api) =
            rate(Some(Billing::Api), Some((sonnet, PriceSource::Builtin))).costs(&g);
        assert!(close(billed, all) && close(api, all), "{billed:?} {api:?}");
        // A plan: nothing billed per token, the API price for comparison.
        let (billed, api) = rate(
            Some(Billing::Subscription),
            Some((sonnet, PriceSource::Catalog)),
        )
        .costs(&g);
        assert!(billed.is_none() && close(api, all));
        // Cache reads and writes cost the input price when not given.
        let flat = Price {
            input: 1.0,
            output: 2.0,
            ..Default::default()
        };
        let (_, api) = rate(Some(Billing::Api), Some((flat, PriceSource::Custom))).costs(&g);
        assert!(close(api, 1.0 + 2.0 + 10.0 + 2.0));
        // Unknown price: no cost at all.
        assert_eq!(rate(Some(Billing::Api), None).costs(&g), (None, None));
        assert_eq!(rate(Some(Billing::Local), None).costs(&g), (None, None));
        // A provider that is gone keeps the cost recorded then.
        assert_eq!(rate(None, None).costs(&g).0, Some(99.0));
        // Claude Code reported its own cost for half the input: that figure,
        // plus the price list for the rest…
        let mut g2 = group(2 * M, 0, 0, 0);
        g2.reported = Some((0.5, [M, 0, 0, 0]));
        let opus = Price::new(4.0, 20.0, 0.2, 5.0);
        let (billed, api) = rate(
            Some(Billing::Subscription),
            Some((opus, PriceSource::Builtin)),
        )
        .costs(&g2);
        assert!(billed.is_none() && close(api, 0.5 + 4.0));
        // …or just the figure when the price is unknown…
        let (_, api) = rate(Some(Billing::Subscription), None).costs(&g2);
        assert!(close(api, 0.5));
        // …while a price the user set wins over it.
        let (_, api) = rate(
            Some(Billing::Subscription),
            Some((opus, PriceSource::Custom)),
        )
        .costs(&g2);
        assert!(close(api, 8.0));
        // Claude Code on an API key records what it billed; on a plan, nothing.
        let mut cc = rate(
            Some(Billing::Subscription),
            Some((opus, PriceSource::Builtin)),
        );
        cc.recorded = true;
        g2.totals.cost_usd = Some(0.5);
        assert_eq!(cc.costs(&g2).0, Some(0.5));
        g2.totals.cost_usd = None;
        assert_eq!(cc.costs(&g2).0, None);
    }

    #[test]
    fn totals_sum_known_costs_only() {
        let mut a = UsageTotals::default();
        add_totals(
            &mut a,
            &UsageTotals {
                requests: 2,
                input: 5,
                api_cost_usd: Some(1.5),
                ..Default::default()
            },
        );
        add_totals(
            &mut a,
            &UsageTotals {
                requests: 1,
                input: 1,
                ..Default::default()
            },
        );
        assert_eq!((a.requests, a.input), (3, 6));
        assert_eq!((a.cost_usd, a.api_cost_usd), (None, Some(1.5)));
    }
}
