//! Shared source markers and narrow old-server completion fallbacks.
use crate::{AssistantPart, Entry, EntryBody, UserNotification};

/// Persisted on acknowledgements, not on anything the agent tells the user.
pub const CHECK_AUTHOR: &str = "harness:completion";

/// A narrow legacy fallback: meaningful answers such as "✓ Tests passed"
/// are content, not protocol.
pub fn is_check_ack(text: &str) -> bool {
    let t = text.trim();
    if !t.chars().any(|c| matches!(c, '✓' | '✔' | '✅')) || t.contains('\n') {
        return false;
    }
    let words = t
        .trim_matches(|c: char| {
            matches!(c, '✓' | '✔' | '✅' | '.' | '!' | ' ') || c.is_whitespace()
        })
        .to_ascii_lowercase();
    matches!(
        words.as_str(),
        "" | "done" | "checked" | "ok" | "okay" | "acknowledged"
    )
}

pub fn check_ack_parts(content: &[AssistantPart]) -> bool {
    let text: String = content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    is_check_ack(&text)
        && content
            .iter()
            .all(|p| !matches!(p, AssistantPart::ToolCall { name, .. } if name != "task"))
}

/// Internal protocol stays in model history, never in a client or a report.
pub fn is_protocol_entry(e: &Entry) -> bool {
    is_protocol_body(&e.body)
}

pub fn is_protocol_body(body: &EntryBody) -> bool {
    match body {
        EntryBody::Assistant {
            content, author, ..
        } => {
            author.as_deref() == Some(CHECK_AUTHOR)
                || (check_ack_parts(content)
                    && !content
                        .iter()
                        .any(|p| matches!(p, AssistantPart::ToolCall { .. })))
        }
        EntryBody::User { content, author } => {
            author.as_deref() == Some("harness")
                && crate::plain_text(content)
                    .trim_start()
                    .starts_with("[Completion check]")
        }
        _ => false,
    }
}

/// Old servers may already have queued the mark as a notification.
pub fn is_protocol_notification(n: &UserNotification) -> bool {
    is_check_ack(&n.title) || is_check_ack(&n.body)
}
