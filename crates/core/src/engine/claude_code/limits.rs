//! Subscription rejections use the public stream, never account files or guessed reset times.

use std::time::Duration;

use serde_json::Value;
use tessera_proto::{PermissionMode, SessionInfo, SessionKind};
use tokio::time::Instant;

const MAX_WAIT: Duration = Duration::from_secs(6 * 3600);
const MAX_RETRIES: u32 = 3;
const RESET_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub(super) struct Hit {
    pub reset_ms: Option<u64>,
    credits: bool,
    window: &'static str,
    /// Canonical provider type, distinct from the plain presentation label.
    limit_type: String,
}

impl Hit {
    pub fn event(info: &Value) -> Option<Self> {
        (info.get("status").and_then(Value::as_str) == Some("rejected")).then(|| Self {
            reset_ms: info
                .get("resetsAt")
                .and_then(Value::as_u64)
                .and_then(|n| n.checked_mul(1000))
                .filter(|n| reset_time(*n).is_some()),
            credits: info.get("errorCode").and_then(Value::as_str) == Some("credits_required"),
            window: window(info),
            limit_type: limit_type(info),
        })
    }

    /// Older CLIs can omit the event. Recognize only plan-specific text, not
    /// context/output/tool limits. The text is not a reliable reset timestamp.
    pub fn result(text: &str) -> Option<Self> {
        let lower = text.trim().to_ascii_lowercase();
        (lower.starts_with("you've hit your limit")
            || lower.starts_with("you have hit your limit")
            || lower.starts_with("you've reached your usage limit")
            || lower.starts_with("claude usage limit reached"))
        .then_some(Self {
            reset_ms: None,
            credits: false,
            window: "usage",
            limit_type: "usage".into(),
        })
    }

    /// Hash identity rather than persisting account paths or private chat ids.
    pub fn notification_window(&self, chat: &str, account: &str) -> Option<(String, u64)> {
        let reset = self.reset_ms?;
        let identity = serde_json::to_vec(&(chat, account, &self.limit_type, reset)).ok()?;
        Some((crate::util::sha256_hex(&identity), reset))
    }
    pub fn notice(&self, waiting: bool) -> String {
        if self.credits {
            return "Your Claude plan needs usage credits before it can continue. Check your plan in Claude, or choose another model.".into();
        }
        let mut text = format!("Your Claude plan's {} limit is reached.", self.window);
        match self.reset_ms.and_then(reset_time) {
            Some(at) => text.push_str(&format!(" It resets at {at}.")),
            None => text.push_str(" Claude Code didn't report a reset time."),
        }
        text.push_str(if waiting {
            " I'll continue then; Stop cancels the wait."
        } else {
            " Send a message later to continue, or choose another model."
        });
        text
    }
}

fn window(info: &Value) -> &'static str {
    match info
        .get("rateLimitType")
        .and_then(Value::as_str)
        .and_then(crate::limits::claude_window)
        .map(|(id, _)| id)
        .as_deref()
    {
        Some(crate::limits::FIVE_HOUR) => "5-hour",
        Some(id) if id == crate::limits::WEEKLY || id.starts_with("weekly_") => "weekly",
        _ => "usage",
    }
}

fn limit_type(info: &Value) -> String {
    let Some(raw) = info
        .get("rateLimitType")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        })
    else {
        return "usage".into();
    };
    // Reuse the plan-limit parser's known set/model validation rather than
    // collapsing every seven_day type into the display label "weekly".
    match crate::limits::claude_window(raw) {
        Some((id, _)) => format!("known:{id}"),
        None => format!("raw:{raw}"),
    }
}

fn reset_time(ms: u64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

pub(super) fn warning(info: &Value) -> Option<String> {
    if info.get("status").and_then(Value::as_str) != Some("allowed_warning") {
        return None;
    }
    let used = info
        .get("utilization")
        .and_then(Value::as_f64)
        .filter(|u| u.is_finite() && (0.0..=1.0).contains(u))
        .map(|u| format!(" You've used {}%.", (u * 100.0).round() as u64))
        .unwrap_or_default();
    let when = info
        .get("resetsAt")
        .and_then(Value::as_u64)
        .and_then(|n| n.checked_mul(1000))
        .and_then(reset_time)
        .map(|at| format!(" It resets at {at}."))
        .unwrap_or_default();
    Some(format!(
        "Your Claude plan's {} limit is nearly reached.{used}{when}",
        window(info)
    ))
}

/// Same attendance rule as provider retries (T3), independent of that branch.
/// A fresh chat/room is watched; scheduled jobs, helpers, bot hand-offs and
/// YOLO are not. Harness-authored notes don't make somebody present.
pub(super) fn unattended(info: &SessionInfo, last_user_ms: Option<u64>, now_ms: u64) -> bool {
    matches!(info.kind, SessionKind::Job | SessionKind::Subagent)
        || (info.kind == SessionKind::Channel && info.bot_id.is_some())
        || info.internal
        || info.working_for.is_some()
        || info.permission == PermissionMode::Auto
        || last_user_ms.is_some_and(|at| now_ms.saturating_sub(at) >= 15 * 60_000)
}

#[derive(Default)]
pub(super) struct Waits {
    first: Option<Instant>,
    retries: u32,
}

impl Waits {
    pub fn next(
        &mut self,
        hit: &Hit,
        unattended: bool,
        now_ms: u64,
        now: Instant,
    ) -> Option<Duration> {
        if !unattended || hit.credits || self.retries >= MAX_RETRIES {
            return None;
        }
        let at = hit.reset_ms?;
        let first = *self.first.get_or_insert(now);
        let remaining = MAX_WAIT.checked_sub(now.saturating_duration_since(first))?;
        let wait = Duration::from_millis(at.saturating_sub(now_ms)) + RESET_GRACE;
        if wait > remaining {
            return None;
        }
        self.retries += 1;
        Some(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn public_events_and_result_only_fallback_stay_honest() {
        let hit = Hit::event(
            &json!({"status":"rejected", "resetsAt":1791820800,"rateLimitType":"five_hour"}),
        )
        .unwrap();
        assert_eq!(hit.reset_ms, Some(1791820800000));
        assert!(hit.notice(false).contains("2026-10-12T16:00:00Z"));
        assert!(!hit.notice(false).contains("I'll continue"));
        assert!(hit.notice(true).contains("Stop cancels"));
        for value in [json!(-1), json!("soon"), json!(u64::MAX), Value::Null] {
            let hit = Hit::event(&json!({"status":"rejected","resetsAt":value})).unwrap();
            assert!(hit.reset_ms.is_none());
            assert!(hit.notice(false).contains("didn't report"));
        }
        assert!(Hit::event(&json!({"status":"allowed"})).is_none());
        assert!(
            Hit::result("You've hit your limit · resets 2pm (America/Chicago)")
                .unwrap()
                .reset_ms
                .is_none()
        );
        for text in [
            "context limit reached",
            "output token limit",
            "HTTP 429",
            "max turns limit",
            "invalid OAuth token",
        ] {
            assert!(Hit::result(text).is_none(), "{text}");
        }
    }

    #[test]
    fn credits_do_not_retry_even_with_a_reset() {
        let hit = Hit::event(
            &json!({"status":"rejected", "errorCode":"credits_required", "resetsAt":100}),
        )
        .unwrap();
        assert!(hit.notice(false).contains("needs usage credits"));
        assert!(
            Waits::default()
                .next(&hit, true, 100_000, Instant::now())
                .is_none()
        );
    }

    #[test]
    fn waits_are_one_total_budget_and_three_retries_not_three_six_hour_waits() {
        let start = Instant::now();
        let mut waits = Waits::default();
        let mut hit = Hit {
            reset_ms: Some(3 * 3_600_000),
            credits: false,
            window: "usage",
            limit_type: "usage".into(),
        };
        assert!(waits.next(&hit, false, 0, start).is_none());
        assert_eq!(
            waits.next(&hit, true, 0, start),
            Some(Duration::from_secs(3 * 3600 + 5))
        );
        hit.reset_ms = Some(6 * 3_600_000);
        assert!(
            waits
                .next(
                    &hit,
                    true,
                    3 * 3_600_000,
                    start + Duration::from_secs(3 * 3600 + 5)
                )
                .is_none()
        );
        let hit = Hit {
            reset_ms: Some(0),
            credits: false,
            window: "usage",
            limit_type: "usage".into(),
        };
        assert!(waits.next(&hit, true, 1, start).is_some());
        assert!(waits.next(&hit, true, 1, start).is_some());
        assert!(waits.next(&hit, true, 1, start).is_none());
        assert!(
            Waits::default()
                .next(
                    &Hit {
                        reset_ms: None,
                        ..hit
                    },
                    true,
                    0,
                    start
                )
                .is_none()
        );
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    #[test]
    fn claude_limit_identity_changes_only_for_chat_account_type_or_reset() {
        let hit = Hit::event(&serde_json::json!({
            "status":"rejected", "resetsAt":1_800_000_000u64, "rateLimitType":"five_hour"
        }))
        .unwrap();
        let first = hit.notification_window("chat", "account").unwrap();
        assert_eq!(
            first,
            hit.clone().notification_window("chat", "account").unwrap()
        );
        assert!(!first.0.contains("account"));
        assert_ne!(first, hit.notification_window("other", "account").unwrap());
        assert_ne!(first, hit.notification_window("chat", "other").unwrap());
        assert_ne!(
            first,
            Hit {
                limit_type: "known:weekly".into(),
                ..hit.clone()
            }
            .notification_window("chat", "account")
            .unwrap()
        );
        assert_ne!(
            first,
            Hit {
                reset_ms: Some(first.1 + 1),
                ..hit.clone()
            }
            .notification_window("chat", "account")
            .unwrap()
        );
        assert!(
            Hit {
                reset_ms: None,
                ..hit
            }
            .notification_window("chat", "account")
            .is_none()
        );
        let window = |kind| {
            Hit::event(&serde_json::json!({
                "status":"rejected", "resetsAt":1_800_000_000u64, "rateLimitType":kind
            }))
            .unwrap()
        };
        let weekly = window("seven_day");
        let opus = window("seven_day_opus");
        assert_eq!(weekly.window, opus.window, "same display label");
        assert_ne!(
            weekly.notification_window("chat", "account"),
            opus.notification_window("chat", "account")
        );
        for raw in [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "seven_day_overage_included",
            "overage",
        ] {
            let canonical = crate::limits::claude_window(raw).unwrap().0;
            assert_eq!(window(raw).limit_type, format!("known:{canonical}"));
        }
        assert_eq!(window("future_window").limit_type, "raw:future_window");
        assert_ne!(
            window("future_window").notification_window("chat", "account"),
            window("other_future_window").notification_window("chat", "account")
        );
        assert_ne!(
            window("weekly").limit_type,
            weekly.limit_type,
            "unknown cannot alias known"
        );
        for invalid in ["", "seven_day_../x", "weekly limit", "bad\nmetadata"] {
            assert_eq!(window(invalid).limit_type, "usage");
        }
    }
}
