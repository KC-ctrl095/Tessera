//! A subscription's usage limits (the 5-hour and weekly windows of a ChatGPT
//! or Claude plan) as the provider last reported them along with a reply:
//! the ChatGPT backend in response headers (`llm::responses::codex_limits`),
//! Claude Code in its `rate_limit_event` messages ([`from_claude`]). Nothing
//! is ever requested just to learn them. Kept in `plan-limits.json` in the
//! data directory, so they survive a restart.

use std::collections::BTreeMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use serde_json::Value;
use tessera_proto::{LimitStatus, PlanLimit};

pub const FIVE_HOUR: &str = "five_hour";
pub const WEEKLY: &str = "weekly";

const MINUTE_MS: u64 = 60_000;
const WEEK_MS: u64 = 7 * 24 * 60 * MINUTE_MS;
/// Only fresher times than this don't make a save on their own.
const SAVE_EVERY_MS: u64 = MINUTE_MS;

#[derive(Default)]
pub struct PlanLimits {
    file: Option<PathBuf>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    by_provider: BTreeMap<String, Vec<PlanLimit>>,
    saved_at: u64,
}

impl PlanLimits {
    /// The limits kept in `file` (none when it is missing or unreadable).
    pub fn load(file: PathBuf) -> Self {
        let by_provider = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            file: Some(file),
            state: Mutex::new(State {
                by_provider,
                saved_at: 0,
            }),
        }
    }

    /// What `provider` just reported. `all`: these are all its windows (the
    /// ChatGPT backend sends every one each time); otherwise they replace
    /// only the windows of the same name (Claude Code names the one limiting
    /// it now, which may be a model's own).
    pub fn record(&self, provider: &str, windows: Vec<PlanLimit>, all: bool) {
        if windows.is_empty() {
            return;
        }
        let now = windows.iter().map(|w| w.updated_at).max().unwrap_or(0);
        let mut st = self.state.lock();
        let list = st.by_provider.entry(provider.to_string()).or_default();
        let before = list.clone();
        if all {
            *list = windows;
        } else {
            for mut w in windows {
                match list.iter_mut().find(|l| l.window == w.window) {
                    Some(l) => {
                        // A status holds until the window resets, also once
                        // another window is the limiting one.
                        if w.status.is_none() && w.resets_at == l.resets_at {
                            w.status = l.status;
                        }
                        *l = w;
                    }
                    None => list.push(w),
                }
            }
        }
        list.sort_by_key(rank);
        // Fresher times alone are saved at most once a minute: a restart may
        // show them a little older than they are, never wrong numbers.
        let moved = list.len() != before.len()
            || list.iter().zip(&before).any(|(a, b)| {
                a.window != b.window
                    || a.status != b.status
                    || a.resets_at != b.resets_at
                    || a.used_percent.map(f64::to_bits) != b.used_percent.map(f64::to_bits)
            });
        if !moved && now.saturating_sub(st.saved_at) < SAVE_EVERY_MS {
            return;
        }
        st.saved_at = now;
        let Some(file) = &self.file else { return };
        let saved = serde_json::to_vec_pretty(&st.by_provider)
            .map_err(anyhow::Error::from)
            .and_then(|json| crate::config::write_atomic(file, &json, false));
        if let Err(e) = saved {
            tracing::warn!("could not save {}: {e}", file.display());
        }
    }

    /// `provider`'s limits to show at `now`: the 5-hour and weekly ones
    /// always (one that has reset since says so by its time), others only
    /// until they reset (they come up only while they are the limiting one).
    pub fn get(&self, provider: &str, now: u64) -> Vec<PlanLimit> {
        let st = self.state.lock();
        let Some(list) = st.by_provider.get(provider) else {
            return Vec::new();
        };
        list.iter()
            .filter(|l| {
                l.window == FIVE_HOUR
                    || l.window == WEEKLY
                    || match l.resets_at {
                        Some(at) => at > now,
                        None => now.saturating_sub(l.updated_at) < WEEK_MS,
                    }
            })
            .cloned()
            .collect()
    }
}

/// 5-hour, weekly, then the others by name.
fn rank(l: &PlanLimit) -> (u8, String) {
    let first = match l.window.as_str() {
        FIVE_HOUR => 0,
        WEEKLY => 1,
        _ => 2,
    };
    (first, l.label.clone())
}

/// A window's name and label from its length.
pub fn window_of_length(minutes: u64) -> (String, String) {
    match minutes {
        300 => (FIVE_HOUR.into(), "5-hour limit".into()),
        10_080 => (WEEKLY.into(), "Weekly limit".into()),
        m if m % 1440 == 0 => (format!("{m}m"), format!("{}-day limit", m / 1440)),
        m if m % 60 == 0 => (format!("{m}m"), format!("{}-hour limit", m / 60)),
        m => (format!("{m}m"), format!("{m}-minute limit")),
    }
}

/// Claude's names for its windows → ours.
pub(crate) fn claude_window(kind: &str) -> Option<(String, String)> {
    let (window, label) = match kind {
        "five_hour" => (FIVE_HOUR, "5-hour limit"),
        "seven_day" => (WEEKLY, "Weekly limit"),
        "seven_day_overage_included" => (
            "weekly_overage_included",
            "Weekly limit, extra usage included",
        ),
        "overage" => ("extra_usage", "Extra usage"),
        // A model's own weekly limit: `seven_day_opus`, `seven_day_sonnet`…
        k => {
            let model = k.strip_prefix("seven_day_")?;
            if model.is_empty() || !model.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            let mut name = model.replace('_', " ");
            if let Some(c) = name.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            return Some((format!("weekly_{model}"), format!("Weekly limit, {name}")));
        }
    };
    Some((window.into(), label.into()))
}

/// The limits in a Claude Code `rate_limit_event`'s `rate_limit_info`: every
/// window it tracks with the share used (`unifiedWindows`: 5-hour and
/// weekly), and the one limiting now with its status (`rateLimitType`,
/// `status`, `utilization`, `resetsAt`) — a model's own weekly limit comes
/// only that way. Shares are fractions there, reset times unix seconds.
pub fn from_claude(info: &Value, now: u64) -> Vec<PlanLimit> {
    let percent = |v: &Value| {
        v.get("utilization")
            .and_then(Value::as_f64)
            .filter(|u| u.is_finite() && *u >= 0.0)
            .map(|u| u * 100.0)
    };
    let resets = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_u64)
            .filter(|s| *s > 0)
            .map(|s| s.saturating_mul(1000))
    };
    let mut out: Vec<PlanLimit> = Vec::new();
    if let Some(windows) = info.get("unifiedWindows").and_then(Value::as_object) {
        for (kind, v) in windows {
            let Some((window, label)) = claude_window(kind) else {
                continue;
            };
            out.push(PlanLimit {
                window,
                label,
                used_percent: percent(v),
                status: None,
                resets_at: resets(v, "resetsAt"),
                updated_at: now,
            });
        }
    }
    let status = match info.get("status").and_then(Value::as_str) {
        Some("allowed") => Some(LimitStatus::Ok),
        Some("allowed_warning") => Some(LimitStatus::Warning),
        Some("rejected") => Some(LimitStatus::Reached),
        _ => None,
    };
    if let Some((window, label)) = info
        .get("rateLimitType")
        .and_then(Value::as_str)
        .and_then(claude_window)
    {
        let (used, at) = (percent(info), resets(info, "resetsAt"));
        match out.iter_mut().find(|l| l.window == window) {
            Some(l) => {
                l.status = status;
                l.used_percent = l.used_percent.or(used);
                l.resets_at = l.resets_at.or(at);
            }
            None => out.push(PlanLimit {
                window,
                label,
                used_percent: used,
                status,
                resets_at: at,
                updated_at: now,
            }),
        }
    }
    out.sort_by_key(rank);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_790_000_000_000;

    fn limit(window: &str, used: f64, resets_at: u64, at: u64) -> PlanLimit {
        let label = if window == FIVE_HOUR {
            "5-hour limit"
        } else {
            "Weekly limit"
        };
        PlanLimit {
            window: window.into(),
            label: label.into(),
            used_percent: Some(used),
            status: None,
            resets_at: Some(resets_at),
            updated_at: at,
        }
    }

    #[test]
    fn claude_reports_every_window_and_the_limiting_ones_status() {
        // As Claude Code 2.1 sends it: both windows, the weekly one limiting.
        let info = json!({
            "status": "allowed_warning", "resetsAt": 1_790_400_000u64, "rateLimitType": "seven_day",
            "utilization": 0.91, "isUsingOverage": false,
            "unifiedWindows": {
                "five_hour": {"utilization": 0.42, "resetsAt": 1_790_010_000u64},
                "seven_day": {"utilization": 0.9, "resetsAt": 1_790_400_000u64}
            }
        });
        let l = from_claude(&info, NOW);
        type Seen<'a> = (&'a str, Option<u64>, Option<LimitStatus>, Option<u64>);
        let got: Vec<Seen> = l
            .iter()
            .map(|l| {
                (
                    l.window.as_str(),
                    l.used_percent.map(|p| p.round() as u64),
                    l.status,
                    l.resets_at,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("five_hour", Some(42), None, Some(1_790_010_000_000)),
                (
                    "weekly",
                    Some(90),
                    Some(LimitStatus::Warning),
                    Some(1_790_400_000_000)
                ),
            ]
        );
        assert!(l.iter().all(|l| l.updated_at == NOW));
        // A model's own weekly limit, reached; only a status and a time.
        let l = from_claude(
            &json!({"status": "rejected", "resetsAt": 1_790_100_000u64, "rateLimitType": "seven_day_opus"}),
            NOW,
        );
        assert_eq!(l.len(), 1);
        assert_eq!(
            (l[0].window.as_str(), l[0].label.as_str()),
            ("weekly_opus", "Weekly limit, Opus")
        );
        assert_eq!(
            (l[0].used_percent, l[0].status),
            (None, Some(LimitStatus::Reached))
        );
        // Older Claude Code: the limiting window alone.
        let l = from_claude(
            &json!({"status": "rejected", "resetsAt": 1_790_001_000u64, "rateLimitType": "five_hour"}),
            NOW,
        );
        assert_eq!(l[0].window, FIVE_HOUR);
        assert_eq!(l[0].status, Some(LimitStatus::Reached));
        // Nothing usable: nothing.
        assert!(from_claude(&json!({"status": "allowed"}), NOW).is_empty());
        assert!(from_claude(&json!({"rateLimitType": "seven_day_../x"}), NOW).is_empty());
    }

    #[test]
    fn windows_are_named_by_their_length() {
        assert_eq!(window_of_length(300).0, FIVE_HOUR);
        assert_eq!(window_of_length(10_080).1, "Weekly limit");
        assert_eq!(window_of_length(1440).1, "1-day limit");
        assert_eq!(
            window_of_length(180),
            ("180m".into(), "3-hour limit".into())
        );
        assert_eq!(window_of_length(45).1, "45-minute limit");
    }

    #[test]
    fn kept_merged_and_saved_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plan-limits.json");
        let limits = PlanLimits::load(file.clone());
        assert!(limits.get("claude-code", NOW).is_empty());
        let reset5 = NOW + 3_600_000;
        let reset7 = NOW + 3 * 86_400_000;
        limits.record(
            "claude-code",
            vec![
                limit(WEEKLY, 30.0, reset7, NOW),
                limit(FIVE_HOUR, 10.0, reset5, NOW),
            ],
            false,
        );
        // A model's own limit joins them; the others stay.
        let mut opus = limit("weekly_opus", 100.0, NOW + 86_400_000, NOW + 1000);
        opus.label = "Weekly limit, Opus".into();
        opus.status = Some(LimitStatus::Reached);
        limits.record("claude-code", vec![opus], false);
        let windows = |l: &PlanLimits, at: u64| -> Vec<String> {
            l.get("claude-code", at)
                .into_iter()
                .map(|l| l.window)
                .collect()
        };
        assert_eq!(windows(&limits, NOW), [FIVE_HOUR, WEEKLY, "weekly_opus"]);
        // ChatGPT sends all of its windows each time.
        limits.record(
            "openai-codex",
            vec![limit(FIVE_HOUR, 5.0, reset5, NOW)],
            true,
        );
        limits.record(
            "openai-codex",
            vec![limit("180m", 7.0, reset5, NOW + 5)],
            true,
        );
        assert_eq!(
            limits
                .get("openai-codex", NOW)
                .iter()
                .map(|l| l.window.as_str())
                .collect::<Vec<_>>(),
            ["180m"]
        );
        // After a restart: the same.
        let again = PlanLimits::load(file);
        assert_eq!(windows(&again, NOW), [FIVE_HOUR, WEEKLY, "weekly_opus"]);
        assert_eq!(again.get("claude-code", NOW)[0].used_percent, Some(10.0));
        // Once a model's own limit has reset it goes; 5-hour and weekly stay,
        // their time telling they have reset since.
        assert_eq!(windows(&again, NOW + 2 * 86_400_000), [FIVE_HOUR, WEEKLY]);
    }

    #[test]
    fn fresher_times_alone_are_saved_at_most_once_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plan-limits.json");
        let limits = PlanLimits::load(file.clone());
        let saved = || -> u64 {
            let v: BTreeMap<String, Vec<PlanLimit>> =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            v["openai-codex"][0].updated_at
        };
        let reset = NOW + 3_600_000;
        limits.record(
            "openai-codex",
            vec![limit(FIVE_HOUR, 5.0, reset, NOW)],
            true,
        );
        assert_eq!(saved(), NOW);
        limits.record(
            "openai-codex",
            vec![limit(FIVE_HOUR, 5.0, reset, NOW + 1000)],
            true,
        );
        assert_eq!(saved(), NOW, "only the time changed");
        assert_eq!(limits.get("openai-codex", NOW)[0].updated_at, NOW + 1000);
        limits.record(
            "openai-codex",
            vec![limit(FIVE_HOUR, 6.0, reset, NOW + 2000)],
            true,
        );
        assert_eq!(saved(), NOW + 2000, "the share moved");
        limits.record(
            "openai-codex",
            vec![limit(FIVE_HOUR, 6.0, reset, NOW + 70_000)],
            true,
        );
        assert_eq!(saved(), NOW + 70_000, "a minute later");
    }
}
