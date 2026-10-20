//! How long to keep asking a model's server again after it fails.
//!
//! A chat somebody watches gives up after about a minute (hosted APIs) or a
//! few minutes (a server on the user's own network, which may be waking up):
//! the screen says "retrying in 8s" and the person decides. Work nobody is
//! watching (scheduled jobs, helpers, bots' work, a chat in YOLO mode, a chat
//! whose user wrote last long ago) keeps asking for [`PATIENCE`], with
//! jittered pauses: an incident at the provider of 5 to 20 minutes would
//! otherwise end the night's work with an error nobody reads until morning.
//!
//! This module is only the schedule. The clock, the random number and the
//! sleeping (which Stop cancels) belong to the caller, so the schedule is
//! tested without waiting.

use std::time::Duration;

use tessera_proto::{PermissionMode, SessionInfo, SessionKind};
use tokio::time::Instant;

use super::{ErrorKind, LlmError};

/// How long unattended work keeps trying after the first failure.
pub const PATIENCE: Duration = Duration::from_secs(30 * 60);

/// A chat turn this long after the user's last message counts as unattended.
pub const AWAY_AFTER: Duration = Duration::from_secs(15 * 60);

/// A model server of the user's own that answers the same HTTP error this
/// many times in a row won't be mended by waiting (a model that fails to load
/// answers 500 every time): unattended work stops, instead of asking a
/// single-slot server for 30 minutes.
pub const SAME_ERROR_LIMIT: u32 = 5;

/// The longest pause of a watched turn (a server's `Retry-After` is cut here).
const WATCHED_PAUSE_MAX: Duration = Duration::from_secs(120);

/// The longest pause of an unattended turn, whether the schedule or the
/// server's own `Retry-After` asks for it.
const UNATTENDED_PAUSE_MAX: Duration = Duration::from_secs(10 * 60);

/// What backoff doubles up to: 2, 4, 8 … seconds.
const WATCHED_BACKOFF_MAX: Duration = Duration::from_secs(60);
const UNATTENDED_BACKOFF_MAX: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Somebody may be watching: a short, plain schedule, a message with
    /// the attempt number.
    Watched { max_attempts: u32 },
    /// Nobody is: [`PATIENCE`] of jittered tries.
    Unattended,
}

/// One chat step's failures so far.
#[derive(Debug, Clone)]
pub struct Backoff {
    mode: Mode,
    attempt: u32,
    first_failure: Option<Instant>,
}

/// What to do about a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pause {
    /// Wait this long, then ask again.
    pub wait: Duration,
    /// Which retry this is (the first is 1).
    pub attempt: u32,
    /// Since the first failure (the wait to come not counted).
    pub elapsed: Duration,
}

impl Backoff {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            attempt: 0,
            first_failure: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Since the first failure.
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.first_failure
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
    }

    /// The pause before the next try after a failure at `now`, or `None` when
    /// the budget is spent. `retry_after` is what the server asked for;
    /// `unit` is a random number from 0 up to (not including) 1 for the
    /// jitter of unattended work. A watched turn is deterministic.
    pub fn next(
        &mut self,
        now: Instant,
        retry_after: Option<Duration>,
        unit: f64,
    ) -> Option<Pause> {
        let first = *self.first_failure.get_or_insert(now);
        let elapsed = now.saturating_duration_since(first);
        let unit = unit.clamp(0.0, 1.0);
        match self.mode {
            Mode::Watched { max_attempts } => {
                if self.attempt >= max_attempts {
                    return None;
                }
                self.attempt += 1;
                let wait = retry_after
                    .unwrap_or_else(|| doubling(self.attempt, WATCHED_BACKOFF_MAX))
                    .min(WATCHED_PAUSE_MAX);
                Some(Pause {
                    wait,
                    attempt: self.attempt,
                    elapsed,
                })
            }
            Mode::Unattended => {
                let left = PATIENCE.checked_sub(elapsed).filter(|l| !l.is_zero())?;
                // Asked to wait longer than the patience has left (a weekly
                // usage limit): asking again meanwhile would only be refused.
                if retry_after.is_some_and(|asked| asked > left) {
                    return None;
                }
                self.attempt += 1;
                // Half to all of the doubling pause, so many helpers failing
                // together don't all come back at the same second.
                let backoff =
                    doubling(self.attempt, UNATTENDED_BACKOFF_MAX).mul_f64(0.5 + 0.5 * unit);
                let wait = match retry_after {
                    // The server's word is a floor; a little on top spreads
                    // the callers that were told the same.
                    Some(asked) => asked.mul_f64(1.0 + 0.1 * unit).max(backoff),
                    None => backoff,
                };
                // The last try is made when the patience ends, not after it.
                let wait = wait.min(UNATTENDED_PAUSE_MAX).min(left);
                Some(Pause {
                    wait,
                    attempt: self.attempt,
                    elapsed,
                })
            }
        }
    }
}

/// 2, 4, 8 … seconds for attempt 1, 2, 3 …, at most `max`.
fn doubling(attempt: u32, max: Duration) -> Duration {
    Duration::from_secs(1u64 << attempt.clamp(1, 16)).min(max)
}

/// Whether nobody is likely watching a session's turn: a scheduled job, a
/// helper, a bot's work, a chat in YOLO mode, a harness helper, or a chat whose
/// user last wrote [`AWAY_AFTER`] ago. User-driven rooms are always watched:
/// their members answer in turn, so one failure must not hold up the room.
pub fn unattended(info: &SessionInfo, last_user_ms: Option<u64>, now_ms: u64) -> bool {
    if info.kind == SessionKind::Channel {
        // A bot-to-bot channel has `bot_id`; a user-driven room does not.
        return info.bot_id.is_some();
    }
    let kind = matches!(info.kind, SessionKind::Job | SessionKind::Subagent);
    if kind
        || info.internal
        || info.working_for.is_some()
        || info.permission == PermissionMode::Auto
    {
        return true;
    }
    last_user_ms.is_some_and(|t| user_is_away(t, now_ms))
}

/// An error that passes by waiting: a server still loading a model (503),
/// overloaded (529), a limit (429), nothing answering (a PC asleep), a
/// connection that dropped. Other HTTP errors repeat until somebody acts.
pub fn passes_by_waiting(e: &LlmError) -> bool {
    let m = e.message.to_ascii_lowercase();
    let loading = m
        .split_once(": ")
        .map_or(m.as_str(), |(_, detail)| detail)
        .trim();
    let loading = ["loading model", "model is loading", "model loading"]
        .iter()
        .any(|p| loading.starts_with(p))
        && !["error", "fail", "out of memory", "unable", "cannot"]
            .iter()
            .any(|p| loading.contains(p));
    e.unreachable
        || e.kind == ErrorKind::RateLimited
        || !m.starts_with("http ")
        || m.starts_with("http 503")
        || m.starts_with("http 529")
        || m.contains("overloaded")
        || loading
}

/// How many times in a row a server's failures said the same thing.
#[derive(Debug, Default)]
pub struct SameError {
    last: Option<String>,
    count: u32,
}

impl SameError {
    /// Count a failure; true when it has been the same error long enough
    /// that waiting won't help (`own`: a server on the user's own network;
    /// a hosted API's incident is waited out).
    pub fn futile(&mut self, e: &LlmError, own: bool) -> bool {
        if !own || passes_by_waiting(e) {
            self.last = None;
            self.count = 0;
            return false;
        }
        if self.last.as_deref() == Some(e.message.as_str()) {
            self.count += 1;
        } else {
            self.last = Some(e.message.clone());
            self.count = 1;
        }
        self.count >= SAME_ERROR_LIMIT
    }

    pub fn count(&self) -> u32 {
        self.count
    }
}

/// Whether a chat step's pauses should be counted from long ago: the user's
/// last message is older than [`AWAY_AFTER`].
pub fn user_is_away(last_user_message_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(last_user_message_ms) >= AWAY_AFTER.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// The pauses a mode takes while every try fails, each pause fully spent,
    /// with `unit` as the random number.
    fn run_out(mut b: Backoff, retry_after: Option<Duration>, unit: f64) -> (Vec<Pause>, Duration) {
        let start = Instant::now();
        let mut now = start;
        let mut out = Vec::new();
        while let Some(p) = b.next(now, retry_after, unit) {
            now += p.wait;
            out.push(p);
            assert!(out.len() < 1000, "never ends");
        }
        (out, now - start)
    }

    #[test]
    fn a_watched_turn_keeps_its_short_schedule() {
        // Hosted APIs: 5 tries after 2, 4, 8, 16, 32 s; about a minute.
        let (pauses, total) = run_out(Backoff::new(Mode::Watched { max_attempts: 5 }), None, 0.7);
        let waits: Vec<u64> = pauses.iter().map(|p| p.wait.as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32]);
        assert_eq!(total, secs(62));
        assert_eq!(pauses.last().unwrap().attempt, 5);
        // A server on the user's network: 10 tries, none over 60 s.
        let (pauses, total) = run_out(Backoff::new(Mode::Watched { max_attempts: 10 }), None, 0.1);
        let waits: Vec<u64> = pauses.iter().map(|p| p.wait.as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60, 60, 60, 60]);
        assert_eq!(total, secs(362));
        // With a fallback waiting: 2 tries.
        let (pauses, _) = run_out(Backoff::new(Mode::Watched { max_attempts: 2 }), None, 0.5);
        assert_eq!(pauses.len(), 2);
    }

    #[test]
    fn a_watched_turn_obeys_retry_after_up_to_two_minutes() {
        let mut b = Backoff::new(Mode::Watched { max_attempts: 5 });
        let now = Instant::now();
        assert_eq!(b.next(now, Some(secs(7)), 0.9).unwrap().wait, secs(7));
        assert_eq!(b.next(now, Some(secs(0)), 0.9).unwrap().wait, secs(0));
        assert_eq!(b.next(now, Some(secs(300)), 0.9).unwrap().wait, secs(120));
    }

    #[test]
    fn an_unattended_turn_keeps_trying_for_the_whole_patience() {
        for unit in [0.0, 0.5, 0.999] {
            let (pauses, total) = run_out(Backoff::new(Mode::Unattended), None, unit);
            // The last try is made when the patience ends, never after it.
            assert!(total <= PATIENCE, "{unit}: {total:?}");
            assert!(total >= PATIENCE - secs(1), "{unit}: {total:?}");
            // Tens of tries rather than 5, so a 10-minute incident is survived.
            assert!(pauses.len() >= 14, "{unit}: {}", pauses.len());
            for p in &pauses {
                assert!(p.wait <= secs(120), "{p:?}");
            }
            // Pauses grow like a watched turn's, then stay at 1 to 2 minutes.
            assert!(
                pauses[0].wait >= secs(1) && pauses[0].wait <= secs(2),
                "{:?}",
                pauses[0]
            );
            let longest = pauses.iter().map(|p| p.wait).max().unwrap();
            assert!(longest >= secs(60) && longest <= secs(120), "{longest:?}");
        }
    }

    #[test]
    fn unattended_pauses_are_jittered_within_bounds() {
        // Half to all of the doubling pause, whatever the random number.
        for attempt in 1..=12u32 {
            let full = doubling(attempt, UNATTENDED_BACKOFF_MAX).as_secs_f64();
            let mut seen = Vec::new();
            for unit in [0.0, 0.25, 0.5, 0.75, 0.9999] {
                let mut b = Backoff::new(Mode::Unattended);
                b.attempt = attempt - 1;
                let p = b.next(Instant::now(), None, unit).unwrap();
                let w = p.wait.as_secs_f64();
                assert!(
                    w >= full * 0.5 - 1e-9 && w <= full + 1e-9,
                    "{attempt} {unit} {w}"
                );
                seen.push(w);
            }
            assert!(
                seen.windows(2).all(|w| w[0] <= w[1]),
                "monotonic in the draw: {seen:?}"
            );
            assert!(seen[0] < seen[4], "the draw matters: {seen:?}");
        }
        // An out-of-range draw is clamped, not trusted.
        let p = Backoff::new(Mode::Unattended)
            .next(Instant::now(), None, 7.0)
            .unwrap();
        assert!(p.wait <= secs(2));
    }

    #[test]
    fn unattended_retry_after_is_a_floor() {
        let now = Instant::now();
        // Asked for 30 s: waits 30 to 33 s, not the 1 to 2 s of the schedule.
        let p = Backoff::new(Mode::Unattended)
            .next(now, Some(secs(30)), 0.0)
            .unwrap();
        assert_eq!(p.wait, secs(30));
        let p = Backoff::new(Mode::Unattended)
            .next(now, Some(secs(30)), 1.0)
            .unwrap();
        assert_eq!(p.wait, secs(33));
        // Longer than the schedule's own pause late in an incident: still its word.
        let mut b = Backoff::new(Mode::Unattended);
        b.attempt = 10;
        let p = b.next(now, Some(secs(200)), 0.0).unwrap();
        assert_eq!(p.wait, secs(200));
        // Never over ten minutes.
        let p = Backoff::new(Mode::Unattended)
            .next(now, Some(secs(900)), 0.0)
            .unwrap();
        assert_eq!(p.wait, secs(600));
        // Asked to wait longer than all the patience: give up at once.
        assert!(
            Backoff::new(Mode::Unattended)
                .next(now, Some(secs(31 * 60)), 0.0)
                .is_none()
        );
        // Later on, what is asked must fit in what is left of the patience.
        let start = Instant::now();
        let mut b = Backoff::new(Mode::Unattended);
        b.next(start, None, 0.0).unwrap();
        assert_eq!(
            b.next(start + secs(25 * 60), Some(secs(4 * 60)), 0.0)
                .unwrap()
                .wait,
            secs(240)
        );
        assert!(
            b.next(start + secs(25 * 60), Some(secs(8 * 60)), 0.0)
                .is_none()
        );
        // Zero means "now", not nothing at all: the schedule's pause stays.
        let p = Backoff::new(Mode::Unattended)
            .next(now, Some(secs(0)), 0.0)
            .unwrap();
        assert_eq!(p.wait, secs(1));
    }

    #[test]
    fn the_budget_counts_from_the_first_failure_in_real_time() {
        // Tries that themselves take long (timeouts) use up the patience too.
        let start = Instant::now();
        let mut b = Backoff::new(Mode::Unattended);
        assert!(b.next(start, None, 0.5).is_some());
        // 29 minutes later: one last short wait, then nothing.
        let late = start + secs(29 * 60);
        let p = b.next(late, None, 0.5).unwrap();
        assert!(p.wait <= secs(60), "{p:?}");
        assert_eq!(p.elapsed, secs(29 * 60));
        assert!(b.next(start + PATIENCE, None, 0.5).is_none());
        assert!(b.next(start + PATIENCE + secs(5), None, 0.5).is_none());
        assert_eq!(b.elapsed(start + secs(90)), secs(90));
    }

    #[test]
    fn away_means_the_users_last_message_is_old() {
        let now = 10_000_000_000u64;
        assert!(!user_is_away(now - 14 * 60_000, now));
        assert!(user_is_away(now - 15 * 60_000, now));
        assert!(user_is_away(0, now));
        // A clock that went back is not "away".
        assert!(!user_is_away(now + 5, now));
    }

    fn info(kind: SessionKind) -> SessionInfo {
        use tessera_proto::*;
        SessionInfo {
            id: "s".into(),
            title: "T".into(),
            kind,
            cwd: "/tmp".into(),
            model: ModelRef {
                provider: "p".into(),
                model: "m".into(),
            },
            thinking: ThinkingLevel::Medium,
            permission: PermissionMode::Default,
            status: SessionStatus::Idle,
            parent_id: None,
            bot_id: None,
            members: Vec::new(),
            created_at: 1,
            updated_at: 1,
            last_seq: 0,
            usage: Usage::default(),
            context: ContextStats {
                tokens: 0,
                window: 1000,
                compactions: 0,
                source: None,
            },
            pinned: false,
            archived: false,
            preview: String::new(),
            task: None,
            private: false,
            steps: 0,
            internal: false,
            fast: false,
            daybreak: false,
            revision: 0,
            helper_limits: None,
            helper_queued: false,
            subagents: true,
            parent_call: None,
            helper_owner: None,
            role: None,
            lean: false,
            working_for: None,
        }
    }

    #[test]
    fn who_is_not_watched() {
        let now = 10_000_000_000u64;
        let recent = Some(now - 60_000);
        let old = Some(now - AWAY_AFTER.as_millis() as u64);
        // Jobs and helpers always, whatever the user last wrote.
        for kind in [SessionKind::Job, SessionKind::Subagent] {
            assert!(unattended(&info(kind), recent, now), "{kind:?}");
            assert!(unattended(&info(kind), None, now), "{kind:?}");
        }
        // A chat is watched while its user is around, and not once they left.
        let chat = info(SessionKind::Chat);
        assert!(!unattended(&chat, recent, now));
        assert!(!unattended(&chat, None, now));
        assert!(unattended(&chat, old, now));
        assert!(!unattended(
            &chat,
            Some(now - AWAY_AFTER.as_millis() as u64 + 1),
            now
        ));
        // YOLO mode, a harness helper, a bot's work handed over by another chat.
        let mut yolo = info(SessionKind::Chat);
        yolo.permission = PermissionMode::Auto;
        assert!(unattended(&yolo, recent, now));
        let mut internal = info(SessionKind::Chat);
        internal.internal = true;
        assert!(unattended(&internal, recent, now));
        let mut handed = info(SessionKind::Chat);
        handed.working_for = Some(tessera_proto::WorkingFor {
            session_id: "other".into(),
            call_id: "c".into(),
            since: 1,
            from_seq: 0,
        });
        assert!(unattended(&handed, recent, now));
        // A room the user types in is watched (its members answer in turn: one
        // failing member must not hold the room for half an hour); a
        // conversation between bots is not.
        let room = info(SessionKind::Channel);
        assert!(!unattended(&room, recent, now));
        assert!(!unattended(&room, None, now));
        assert!(!unattended(&room, old, now), "rooms stay watched");
        let mut auto_room = room;
        auto_room.permission = PermissionMode::Auto;
        assert!(!unattended(&auto_room, recent, now));
        assert!(!unattended(&auto_room, old, now));
        let mut dm = info(SessionKind::Channel);
        dm.bot_id = Some("bot".into());
        assert!(unattended(&dm, recent, now));
    }

    #[test]
    fn errors_that_waiting_will_not_mend() {
        let http = |status, text: &str| LlmError::from_http(status, text, None);
        let mut gone = LlmError::new(ErrorKind::Transient, "network error: connection refused");
        gone.unreachable = true;
        // What passes.
        for e in [
            http(503, "model is loading"),
            http(529, "busy"),
            http(500, "the server is overloaded"),
            http(429, "slow down"),
            gone,
            LlmError::cut_off(),
            LlmError::new(ErrorKind::Transient, "network error: timed out"),
        ] {
            assert!(passes_by_waiting(&e), "{e}");
        }
        // What repeats until somebody acts.
        for e in [http(500, "failed to load model"), http(502, "bad gateway")] {
            assert!(!passes_by_waiting(&e), "{e}");
        }
        // Five times the same, on the user's own server.
        let e = http(500, r#"{"error":{"message":"failed to load model"}}"#);
        let mut same = SameError::default();
        for i in 1..SAME_ERROR_LIMIT {
            assert!(!same.futile(&e, true), "{i}");
        }
        assert!(same.futile(&e, true));
        assert_eq!(same.count(), SAME_ERROR_LIMIT);
        // Not on a hosted API, not for a different error in between, not for
        // one that passes. A new streak gets its own five attempts.
        let mut same = SameError::default();
        assert!((0..9).all(|_| !same.futile(&e, false)));
        let mut same = SameError::default();
        let other = http(500, r#"{"error":{"message":"out of memory"}}"#);
        for _ in 0..4 {
            assert!(!same.futile(&e, true));
        }
        assert!(!same.futile(&other, true));
        assert_eq!(same.count(), 1);
        for _ in 0..4 {
            assert!(!same.futile(&e, true));
        }
        assert!(same.futile(&e, true));
        let busy = http(503, "loading model");
        assert!((0..9).all(|_| !same.futile(&busy, true)));
        assert_eq!(same.count(), 0);
        for _ in 0..4 {
            assert!(!same.futile(&e, true));
        }
        assert!(same.futile(&e, true));
    }

    #[test]
    fn loading_progress_is_not_a_loading_failure() {
        for message in ["loading model", "model is loading", "model loading (45%)"] {
            let e = LlmError::from_http(500, message, None);
            assert!(passes_by_waiting(&e), "{message}");
        }
        for message in [
            "error loading model: out of memory",
            "failed loading model",
            "loading model failed",
            "loading model: out of memory",
        ] {
            let e = LlmError::from_http(500, message, None);
            assert!(!passes_by_waiting(&e), "{message}");
            let mut same = SameError::default();
            for _ in 1..SAME_ERROR_LIMIT {
                assert!(!same.futile(&e, true));
            }
            assert!(same.futile(&e, true));
        }
    }
}
