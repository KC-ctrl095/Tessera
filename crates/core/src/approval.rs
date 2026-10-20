//! Honest refusal causes shared by native tools and the Claude Code bridge.

use tessera_proto::ApprovalDecision;

pub(crate) const WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Allowed(ApprovalDecision),
    Declined,
    /// A helper's approval, declined by the chat that started it.
    DeclinedByParent,
    Expired,
    Dismissed,
    Rule(String),
    Stopped,
}

impl Outcome {
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Allowed(_))
    }

    pub fn decision(&self) -> ApprovalDecision {
        match self {
            Self::Allowed(d) => *d,
            _ => ApprovalDecision::Deny,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Allowed(_) => "allowed",
            Self::Declined | Self::DeclinedByParent => "declined",
            Self::Expired => "expired",
            Self::Dismissed => "dismissed",
            Self::Rule(_) => "rule",
            Self::Stopped => "stopped",
        }
    }

    pub fn message(&self, tool: &str) -> String {
        self.message_in(tool, false)
    }

    /// What the agent is told; a helper (`helper`) has no chat to ask in,
    /// it reports to the chat that started it.
    pub fn message_in(&self, tool: &str, helper: bool) -> String {
        if helper {
            let report =
                "Carry on with other work, and say in your report what still needs approval.";
            match self {
                Self::Declined => {
                    return format!(
                        "The user declined this {tool} action. Don't retry it; {}",
                        report.to_lowercase()
                    );
                }
                Self::DeclinedByParent => {
                    return format!(
                        "The chat that started you declined this {tool} action. Don't retry it; {}",
                        report.to_lowercase()
                    );
                }
                Self::Expired => {
                    return format!(
                        "Not approved: nobody answered the {tool} approval within 15 minutes, so the action did not run. Don't wait for it. {report}"
                    );
                }
                Self::Dismissed => {
                    return format!(
                        "Not approved: the {tool} approval was closed without an answer, so the action did not run. {report}"
                    );
                }
                Self::Rule(why) => {
                    return format!(
                        "Not performed: {why} Carry on with other allowed work, and say in your report what would need to change."
                    );
                }
                _ => {}
            }
        }
        match self {
            Self::Allowed(_) => String::new(),
            Self::Declined => format!("The user declined this {tool} action. Don't retry it; carry on with other work or ask in the chat."),
            Self::DeclinedByParent => format!("The chat that started you declined this {tool} action. Don't retry it; carry on with other work."),
            Self::Expired => format!("Not approved yet: nobody answered the {tool} approval in time. The action did not run. Carry on with other work, or ask in the chat."),
            Self::Dismissed => format!("Not approved yet: the {tool} approval was closed without an answer. The action did not run. Carry on with other work, or ask in the chat."),
            Self::Rule(why) => format!("Not performed: {why} Carry on with other allowed work, or ask in the chat what needs to change."),
            Self::Stopped => "The user stopped this turn. End the turn; don't retry the action. A new message after Resume can start new work.".into(),
        }
    }
}

pub(crate) struct Answer {
    pub outcome: Outcome,
    pub who: String,
}

impl Answer {
    pub fn rule(outcome: Outcome) -> Self {
        Self {
            outcome,
            who: "rule".into(),
        }
    }
}

/// Even a reply queued just before expiry is not authority once consumed
/// after the deadline. Stop remains Stop rather than masquerading as expiry.
fn before_deadline(answer: Answer, deadline: tokio::time::Instant) -> Answer {
    if tokio::time::Instant::now() >= deadline && answer.outcome != Outcome::Stopped {
        Answer::rule(Outcome::Expired)
    } else {
        answer
    }
}

pub(crate) async fn wait_answer(
    rx: tokio::sync::oneshot::Receiver<Answer>,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: tokio::time::Instant,
) -> Answer {
    let answer = tokio::select! {
        biased;
        d = rx => d.unwrap_or_else(|_| Answer::rule(Outcome::Dismissed)),
        _ = cancel.cancelled() => Answer::rule(Outcome::Dismissed),
        _ = tokio::time::sleep_until(deadline) => Answer::rule(Outcome::Expired),
    };
    before_deadline(answer, deadline)
}

// Only identifiers cross the audit boundary, never a summary, rule text,
// command, arguments or permission key (all of those can contain secrets).
fn identifier(s: &str) -> String {
    s.chars()
        .take(96)
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-:.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn audit(session: &str, tool: &str, answer: &Answer) {
    let line = audit_line(session, tool, answer);
    tracing::info!("{line}");
    crate::trace::approval(session, answer.outcome.name());
}

fn audit_line(session: &str, tool: &str, answer: &Answer) -> String {
    format!(
        "approval time={} session={} tool={} outcome={} answered_by={}",
        crate::util::now_ms(),
        identifier(session),
        identifier(tool),
        answer.outcome.name(),
        identifier(&answer.who)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_are_distinct_and_only_stop_ends_the_turn() {
        let causes = [
            Outcome::Declined,
            Outcome::Expired,
            Outcome::Dismissed,
            Outcome::Rule("This chat is read-only; the user can change its mode.".into()),
            Outcome::Stopped,
            Outcome::DeclinedByParent,
        ];
        let messages: Vec<_> = causes.iter().map(|c| c.message("bash")).collect();
        for (i, message) in messages.iter().enumerate() {
            assert!(!message.is_empty());
            assert!(!message.contains("STOP what you are doing"));
            assert_eq!(message.contains("End the turn"), i == 4);
            // A helper is never told to ask in a chat it doesn't have.
            let helper = causes[i].message_in("bash", true);
            assert!(!helper.contains("STOP what you are doing"));
            assert!(!helper.contains("ask in the chat"), "{helper}");
            assert!(!messages[..i].contains(message));
            let line: serde_json::Value = serde_json::from_str(
                &crate::claude_code::permission_line("r", false, &serde_json::Value::Null, message),
            )
            .unwrap();
            assert_eq!(line["response"]["response"]["message"], *message);
            assert_eq!(line["response"]["response"]["interrupt"], false);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_ready_answer_and_expiry_cannot_race_into_an_allow() {
        for decision in [
            ApprovalDecision::Allow,
            ApprovalDecision::AllowSession,
            ApprovalDecision::Deny,
        ] {
            let deadline = tokio::time::Instant::now() + WAIT;
            let (tx, rx) = tokio::sync::oneshot::channel();
            // The response branch is deliberately first/ready, as in the
            // original biased select. It must still lose at consumption.
            tx.send(Answer {
                outcome: if decision == ApprovalDecision::Deny {
                    Outcome::Declined
                } else {
                    Outcome::Allowed(decision)
                },
                who: "fixture".into(),
            })
            .ok()
            .unwrap();
            tokio::time::advance(WAIT).await;
            let answer =
                wait_answer(rx, &tokio_util::sync::CancellationToken::new(), deadline).await;
            assert_eq!(answer.outcome, Outcome::Expired);
            assert_eq!(answer.who, "rule");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timely_answers_are_allowed_but_exact_deadline_is_expired() {
        let deadline = tokio::time::Instant::now() + WAIT;
        let answer = || Answer {
            outcome: Outcome::Allowed(ApprovalDecision::Allow),
            who: "fixture".into(),
        };
        assert!(before_deadline(answer(), deadline).outcome.allowed());
        tokio::time::advance(WAIT).await;
        assert_eq!(
            before_deadline(answer(), deadline).outcome,
            Outcome::Expired
        );
        assert_eq!(
            before_deadline(Answer::rule(Outcome::Stopped), deadline).outcome,
            Outcome::Stopped
        );
    }
    #[test]
    fn audit_never_includes_rule_text_or_arguments() {
        let answer = Answer {
            outcome: Outcome::Rule("command-with-private-token".into()),
            who: "device-123".into(),
        };
        let line = audit_line("chat-1", "bash", &answer);
        for field in [
            "time=",
            "session=chat-1",
            "tool=bash",
            "outcome=rule",
            "answered_by=device-123",
        ] {
            assert!(line.contains(field), "{line}");
        }
        assert!(!line.contains("private-token"));
        assert!(!audit_line("chat\n1", "bash", &answer).contains('\n'));
        for outcome in [
            Outcome::Allowed(ApprovalDecision::Allow),
            Outcome::Declined,
            Outcome::Expired,
            Outcome::Dismissed,
            Outcome::Stopped,
        ] {
            let name = outcome.name();
            assert!(
                audit_line("chat", "Bash", &Answer::rule(outcome.clone()))
                    .contains(&format!("outcome={name}"))
            );
        }
    }
}
