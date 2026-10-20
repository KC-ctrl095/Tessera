//! One persisted terminal cause and boundary for every helper/bot attempt.
use super::{Inner, Outcome, Rt};
use crate::store::{Attempt, AttemptOutcome};
use tessera_proto::{AssistantPart, Entry, EntryBody, StopReason};
use tokio_util::sync::CancellationToken;

pub(super) fn notes(outcome: Option<AttemptOutcome>, partial: Option<String>) -> String {
    let line = match outcome {
        Some(AttemptOutcome::Completed) => return partial.unwrap_or_default(),
        Some(AttemptOutcome::Failed) => super::FAILED_REPLY_NOTICE,
        Some(AttemptOutcome::Stopped) => {
            "This attempt was stopped before it finished; these are its last notes, not a final report."
        }
        Some(AttemptOutcome::TimedOut) => "This attempt timed out before it finished.",
        Some(AttemptOutcome::Interrupted) => {
            "This attempt was interrupted by a server restart before it finished."
        }
        Some(AttemptOutcome::Exhausted) => {
            "This attempt exhausted its budget without a final answer."
        }
        None => {
            "This attempt has no confirmed terminal result; these are working notes, not a final report."
        }
    };
    match partial.filter(|s| !s.trim().is_empty()) {
        Some(text) => format!("{line}\n\nPartial notes, not a final reply or report:\n\n{text}"),
        None => line.into(),
    }
}

pub(super) fn last_text(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| !crate::tools::meta::is_protocol_entry(e))
        .find_map(|e| {
            let EntryBody::Assistant { content, .. } = &e.body else {
                return None;
            };
            let text = content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        })
}

pub(super) fn history(entries: &[Entry], attempt: Option<&Attempt>) -> String {
    let boundary = boundary(entries, attempt);
    let current: Vec<_> = entries
        .iter()
        .filter(|e| e.seq > boundary && e.seq <= attempt.and_then(|a| a.to_seq).unwrap_or(u64::MAX))
        .cloned()
        .collect();
    history_with_text(entries, attempt, last_text(&current))
}

fn boundary(entries: &[Entry], attempt: Option<&Attempt>) -> u64 {
    attempt.map_or_else(
        || {
            entries
                .iter()
                .rev()
                .find(|e| {
                    matches!(
                        &e.body,
                        EntryBody::Assistant {
                            stop: StopReason::Stop,
                            ..
                        }
                    ) && last_text(std::slice::from_ref(e)).is_some()
                })
                .map_or(0, |e| e.seq)
        },
        |a| a.from_seq,
    )
}

pub(super) fn status(outcome: AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Completed => "finished",
        AttemptOutcome::Failed => "failed",
        AttemptOutcome::Stopped => "stopped",
        AttemptOutcome::TimedOut => "timed out",
        AttemptOutcome::Exhausted => "out of steps",
        AttemptOutcome::Interrupted => "interrupted by restart",
    }
}

fn history_with_text(
    entries: &[Entry],
    attempt: Option<&Attempt>,
    report: Option<String>,
) -> String {
    let boundary = boundary(entries, attempt);
    let mut text = notes(attempt.and_then(|a| a.outcome), report);
    if let Some(cause) = attempt
        .filter(|a| a.outcome == Some(AttemptOutcome::Failed))
        .and_then(|a| a.safe_cause.as_deref())
    {
        text.insert_str(
            super::FAILED_REPLY_NOTICE.len(),
            &format!("\n\nFailed: {cause}"),
        );
    }
    for e in entries.iter().rev().filter(|e| e.seq <= boundary) {
        if matches!(
            &e.body,
            EntryBody::Assistant {
                stop: StopReason::Stop,
                ..
            }
        ) && let Some(report) = last_text(std::slice::from_ref(e))
        {
            text.push_str(&format!(
                "\n\n## Earlier report (historical), assistant entry {}\n\n{report}",
                e.seq
            ));
        }
    }
    text
}

impl Inner {
    pub(super) fn formatted_attempt(&self, rt: &Rt, report: String) -> String {
        let st = rt.st.lock();
        let attempt = st.internal.attempts.last().cloned();
        let id = st.info.id.clone();
        drop(st);
        let entries = self.store.entries(&id, 0).unwrap_or_default();
        history_with_text(&entries, attempt.as_ref(), Some(report))
    }

    pub(super) fn latest_attempt_text(&self, rt: &Rt) -> Option<String> {
        let st = rt.st.lock();
        let boundary = st.internal.attempts.last()?.from_seq;
        let id = st.info.id.clone();
        drop(st);
        last_text(&self.store.entries(&id, boundary).ok()?)
    }
    pub(super) fn begin_attempt(&self, rt: &Rt) -> Option<String> {
        let mut st = rt.st.lock();
        if st.internal.subagent.is_none() && st.bot_depth == 0 {
            return None;
        }
        // Setup may append notices. They do not start another attempt.
        if let Some(a) = st
            .internal
            .attempts
            .last_mut()
            .filter(|a| a.outcome.is_none())
        {
            if a.id.is_empty() {
                a.id = crate::util::new_id();
            }
            return Some(a.id.clone());
        }
        let id = crate::util::new_id();
        let from_seq = st.info.last_seq;
        st.internal.attempts.push(Attempt {
            id: id.clone(),
            safe_cause: None,
            from_seq,
            to_seq: None,
            outcome: None,
        });
        drop(st);
        self.save(rt);
        Some(id)
    }

    pub(super) fn attempt_result(
        &self,
        rt: &Rt,
        attempt_id: Option<&str>,
        outcome: &Outcome,
    ) -> Option<AttemptOutcome> {
        let attempt_id = attempt_id?;
        let (id, attempt) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                st.internal
                    .attempts
                    .iter()
                    .find(|a| a.id == attempt_id)
                    .cloned(),
            )
        };
        let attempt = attempt?;
        let entries = self
            .store
            .entries(&id, attempt.from_seq)
            .unwrap_or_default();
        let completed = entries
            .iter()
            .rev()
            .find(|e| !crate::tools::meta::is_protocol_entry(e))
            .is_some_and(|e| {
                matches!(
                    &e.body,
                    EntryBody::Assistant {
                        stop: StopReason::Stop,
                        ..
                    }
                ) && last_text(std::slice::from_ref(e)).is_some()
            });
        let result = match outcome {
            Outcome::Failed(_) => AttemptOutcome::Failed,
            Outcome::Aborted => AttemptOutcome::Stopped,
            Outcome::Done if completed => AttemptOutcome::Completed,
            Outcome::Done => AttemptOutcome::Exhausted,
        };
        Some(result)
    }

    #[cfg(test)]
    pub(super) fn end_attempt(&self, rt: &Rt, attempt_id: Option<&str>, outcome: &Outcome) {
        if let Some(id) = attempt_id
            && let Some(result) = self.attempt_result(rt, Some(id), outcome)
        {
            self.set_attempt_outcome(rt, id, result, None);
        }
    }

    pub(super) fn set_attempt_outcome(
        &self,
        rt: &Rt,
        id: &str,
        result: AttemptOutcome,
        safe_cause: Option<String>,
    ) -> bool {
        let mut st = rt.st.lock();
        let end = st.info.last_seq;
        let changed = st
            .internal
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .is_some_and(|a| a.finish(result, end, safe_cause));
        drop(st);
        if changed {
            self.save(rt);
        }
        changed
    }

    pub(super) fn watch_attempt(
        self: &std::sync::Arc<Self>,
        rt: std::sync::Arc<Rt>,
        cancel: CancellationToken,
        hard: Option<std::time::Instant>,
    ) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            match hard {
                Some(hard) => tokio::time::sleep_until(hard.into()).await,
                None => std::future::pending::<()>().await,
            }
            me.timeout_attempt(&rt, &cancel);
        })
    }

    /// The watchdog records its own cause before waking the cancelled run.
    pub(super) fn timeout_attempt(&self, rt: &Rt, cancel: &CancellationToken) {
        if cancel.is_cancelled() {
            return;
        }
        let id = rt
            .st
            .lock()
            .internal
            .attempts
            .last()
            .filter(|a| a.outcome.is_none())
            .map(|a| a.id.clone());
        if let Some(id) = id
            && self.set_attempt_outcome(rt, &id, AttemptOutcome::TimedOut, None)
        {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_transition_is_write_once_including_end_and_safe_cause() {
        let mut a = Attempt {
            id: "once".into(),
            safe_cause: None,
            from_seq: 10,
            to_seq: None,
            outcome: None,
        };
        assert!(a.finish(
            AttemptOutcome::Failed,
            12,
            Some("the working folder changed.".into())
        ));
        assert!(!a.finish(AttemptOutcome::Completed, 99, None));
        assert_eq!(a.outcome, Some(AttemptOutcome::Failed));
        assert_eq!(a.to_seq, Some(12));
        assert_eq!(a.safe_cause.as_deref(), Some("the working folder changed."));
        assert_eq!(status(AttemptOutcome::Exhausted), "out of steps");
    }

    #[test]
    fn legacy_unfinished_notes_are_unconfirmed_after_last_success() {
        let said = |seq, text: &str, stop| Entry {
            seq,
            ts: 0,
            body: EntryBody::Assistant {
                content: vec![AssistantPart::Text { text: text.into() }],
                model: tessera_proto::ModelRef {
                    provider: "p".into(),
                    model: "m".into(),
                },
                stop,
                usage: Default::default(),
                author: None,
                error: None,
            },
        };
        let entries = [
            said(1, "Older success.", StopReason::Stop),
            said(2, "Latest unfinished notes.", StopReason::Error),
        ];
        let text = history(&entries, None);
        assert!(text.starts_with("This attempt has no confirmed terminal result"));
        assert!(
            text.split("## Earlier")
                .next()
                .unwrap()
                .contains("Latest unfinished notes.")
        );
        assert!(
            !text
                .split("## Earlier")
                .next()
                .unwrap()
                .contains("Older success.")
        );
        assert!(text.contains("Earlier report (historical)"));
    }

    #[test]
    fn terminal_boundaries_and_causes_survive_persistence_and_share_one_label() {
        for outcome in [
            AttemptOutcome::Completed,
            AttemptOutcome::Failed,
            AttemptOutcome::Stopped,
            AttemptOutcome::TimedOut,
            AttemptOutcome::Exhausted,
            AttemptOutcome::Interrupted,
        ] {
            let a = Attempt {
                id: "roundtrip".into(),
                safe_cause: None,
                from_seq: 42,
                to_seq: Some(45),
                outcome: Some(outcome),
            };
            let saved = serde_json::to_string(&a).unwrap();
            let restored: Attempt = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored.from_seq, 42);
            assert_eq!(restored.outcome, Some(outcome));
            let text = notes(restored.outcome, Some("partial".into()));
            assert_eq!(text == "partial", outcome == AttemptOutcome::Completed);
            if outcome != AttemptOutcome::Completed {
                assert!(text.contains("Partial notes, not a final reply or report:"));
            }
        }
    }
}
