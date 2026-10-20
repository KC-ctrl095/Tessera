//! Conservative title handling: an uncertain answer leaves the local title intact.

use tessera_proto::AssistantPart;

/// Providers keep thinking separate from answer text. Only the last answer
/// block is a title candidate, never a reasoning block or earlier commentary.
pub(super) fn final_answer(parts: &[AssistantPart]) -> String {
    parts
        .iter()
        .rev()
        .find_map(|part| match part {
            AssistantPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn fallback(first: &str) -> String {
    let words = first
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ");
    crate::util::one_line(&words, 60)
}

pub(super) fn clean(answer: &str) -> Option<String> {
    let decorations = ['"', '\'', '“', '”', '‘', '’', '*', '#', '`'];
    let mut title = answer.trim().trim_matches(decorations).trim();
    if let Some((label, rest)) = title.split_once(':')
        && matches!(
            label.trim().to_ascii_lowercase().as_str(),
            "title" | "chat title" | "conversation title"
        )
    {
        title = rest.trim();
    }
    title = title.trim_matches(decorations).trim();
    // A label on its own line is fine, but multiple answer lines are ambiguous.
    if title.lines().count() != 1 {
        return None;
    }
    let title = title.trim_end_matches(['.', '!', '?']).trim();
    let lower = title.to_lowercase();
    let reasoning = [
        "we need",
        "we should",
        "we can",
        "i need",
        "i should",
        "i think",
        "i will",
        "i'll",
        "let me",
        "let's",
        "maybe",
        "perhaps",
        "the user",
        "the title",
        "a title",
        "this title",
        "here is",
        "here's",
        "sure",
        "final answer",
        "analysis",
        "reasoning",
        "thinking",
    ];
    if title.chars().count() < 3
        || title.chars().count() > 80
        || title.split_whitespace().count() > 10
        || title.contains(['\n', '\r', '<', '>'])
        // Several sentences are reasoning, not a title; a dot inside a word
        // ("Node.js", "example.com") is fine.
        || title
            .split_whitespace()
            .rev()
            .skip(1)
            .any(|w| w.ends_with(['.', '!', '?', ';']))
        || title.chars().any(char::is_control)
        || reasoning.iter().any(|prefix| {
            lower == *prefix
                || lower.strip_prefix(prefix).is_some_and(|rest| {
                    rest.starts_with(|c: char| c.is_whitespace() || c == ':' || c == ',')
                })
        })
    {
        return None;
    }
    Some(title.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_strips_quotes_labels_and_punctuation() {
        for answer in [
            "\"Unify Claude and Codex\"",
            "Title: Unify Claude and Codex",
            "\"Title: Unify Claude and Codex\"",
            "Chat title: ‘Unify Claude and Codex’",
            "Title:\nUnify Claude and Codex",
            "Unify Claude and Codex.",
        ] {
            assert_eq!(
                clean(answer).as_deref(),
                Some("Unify Claude and Codex"),
                "{answer}"
            );
        }
    }

    #[test]
    fn titles_with_dots_inside_words_stay() {
        assert_eq!(clean("Node.js Setup").as_deref(), Some("Node.js Setup"));
        assert_eq!(
            clean("Fix example.com DNS").as_deref(),
            Some("Fix example.com DNS")
        );
        assert_eq!(clean("Why is CI slow?").as_deref(), Some("Why is CI slow"));
        assert_eq!(clean("Plan it. Then write the title"), None);
    }

    #[test]
    fn title_rejects_leaked_reasoning_and_ambiguous_answers() {
        let first = "Unify Claude and Codex helpers across all providers please";
        for answer in [
            "We need title 3-7 words. Maybe \"Unify Clau…",
            "We need a short title",
            "Maybe Unify Claude and Codex",
            "I think Unify Claude and Codex",
            "Unify providers. Keep helpers consistent.",
            "Unify Claude and Codex\nKeep helpers consistent",
            "<think>Choose a title</think>Unify Claude and Codex",
            "One two three four five six seven eight nine ten eleven",
            "",
            "   ",
            "Title:",
        ] {
            assert_eq!(clean(answer), None, "{answer}");
            assert_eq!(
                clean(answer).unwrap_or_else(|| fallback(first)),
                "Unify Claude and Codex helpers across all"
            );
        }
    }

    #[test]
    fn title_uses_only_final_text_not_thinking_or_commentary() {
        let thinking = AssistantPart::Thinking {
            text: "We need title 3-7 words. Maybe \"Unify Clau…".into(),
            opaque: None,
        };
        let parts = vec![
            AssistantPart::Text {
                text: "Let me choose a title".into(),
            },
            thinking.clone(),
            AssistantPart::Text {
                text: "Title: Unify Claude and Codex".into(),
            },
        ];
        assert_eq!(
            clean(&final_answer(&parts)).as_deref(),
            Some("Unify Claude and Codex")
        );
        assert_eq!(final_answer(&[thinking]), "");
    }

    #[test]
    fn title_fallback_is_short_and_handles_unicode() {
        assert_eq!(fallback("  Sort\n the photos  "), "Sort the photos");
        let long = "写真".repeat(100);
        assert!(fallback(&long).chars().count() <= 61);
    }
}
