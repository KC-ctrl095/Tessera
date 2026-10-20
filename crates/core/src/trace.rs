//! One content-free line per turn in the server log, so a failure can be
//! diagnosed without reading anyone's chat: which model, how many steps,
//! which tools with their outcomes and time, approvals, errors by class and
//! tokens. Never message text, tool arguments or output, error text, file
//! names or titles: only identifiers, counts and classes.
//!
//! ```text
//! turn session=01m4… kind=chat model=openai/gpt-x outcome=done secs=42.1 steps=7
//!   tools=bash:4/1/0:12.3s,read:3/0/0:0.2s approvals=allowed:1,declined:1
//!   errors=overloaded:1 tokens=in:12345,out:2345,cache_read:10000,cache_write:0 compactions=0
//! ```
//! (one line). A tool's `ok/error/not_run:seconds`: runs that succeeded,
//! runs that failed, and calls that never ran (refused, declined, stopped or
//! cut off before they started), then the time its runs took.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use parking_lot::Mutex;
use tessera_proto::{Entry, EntryBody, NoticeLevel, SessionKind, Usage};

/// Approval outcomes per session since its turn began.
fn approvals() -> &'static Mutex<HashMap<String, BTreeMap<&'static str, u32>>> {
    static A: OnceLock<Mutex<HashMap<String, BTreeMap<&'static str, u32>>>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// Count an approval outcome (`approval::Outcome::name`) for `session`.
pub(crate) fn approval(session: &str, outcome: &'static str) {
    let mut all = approvals().lock();
    // Sessions whose turn never ends here (a helper deleted mid-turn) must
    // not pile up.
    if all.len() > 4096 && !all.contains_key(session) {
        all.clear();
    }
    *all.entry(session.to_string())
        .or_default()
        .entry(outcome)
        .or_default() += 1;
}

/// How a turn ended.
pub(crate) enum TurnEnd<'a> {
    Done,
    Stopped,
    Failed(&'a str),
}

/// Log the line for a turn of `session` whose entries are `entries`.
pub(crate) fn turn(
    session: &str,
    kind: SessionKind,
    private: bool,
    entries: &[Entry],
    took: std::time::Duration,
    end: TurnEnd,
) {
    let approvals = approvals().lock().remove(session).unwrap_or_default();
    let line = turn_line(
        if private { "private" } else { session },
        kind,
        entries,
        took,
        &end,
        &approvals,
    );
    tracing::info!("{line}");
}

fn turn_line(
    session: &str,
    kind: SessionKind,
    entries: &[Entry],
    took: std::time::Duration,
    end: &TurnEnd,
    approvals: &BTreeMap<&'static str, u32>,
) -> String {
    let mut models: Vec<String> = Vec::new();
    let mut steps = 0u32;
    let mut tokens = Usage::default();
    // name → (ok, error, not run, milliseconds)
    let mut tools: BTreeMap<String, (u32, u32, u32, u64)> = BTreeMap::new();
    let mut errors: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut compactions = 0u32;
    for e in entries {
        match &e.body {
            EntryBody::Assistant {
                model,
                usage,
                error,
                ..
            } => {
                steps += 1;
                let m = identifier(&format!("{}/{}", model.provider, model.model));
                if !models.contains(&m) {
                    models.push(m);
                }
                tokens.input += usage.input;
                tokens.output += usage.output;
                tokens.cache_read += usage.cache_read;
                tokens.cache_write += usage.cache_write;
                if let Some(err) = error {
                    *errors.entry(error_class(err)).or_default() += 1;
                }
            }
            EntryBody::ToolResult {
                tool,
                is_error,
                details,
                ..
            } => {
                let t = tools.entry(identifier(tool)).or_default();
                let ran = details
                    .as_ref()
                    .and_then(|d| d.get("duration_ms"))
                    .and_then(|v| v.as_u64());
                match ran {
                    None => t.2 += 1,
                    Some(ms) => {
                        if *is_error {
                            t.1 += 1;
                        } else {
                            t.0 += 1;
                        }
                        t.3 += ms;
                    }
                }
            }
            EntryBody::Compaction { .. } => compactions += 1,
            EntryBody::Notice {
                level: NoticeLevel::Error,
                ..
            } => *errors.entry("notice").or_default() += 1,
            _ => {}
        }
    }
    let outcome = match end {
        TurnEnd::Done => "done".to_string(),
        TurnEnd::Stopped => "stopped".to_string(),
        TurnEnd::Failed(e) => {
            let class = error_class(e);
            *errors.entry(class).or_default() += 1;
            format!("failed:{class}")
        }
    };
    let list = |items: Vec<String>| {
        if items.is_empty() {
            "-".to_string()
        } else {
            items.join(",")
        }
    };
    format!(
        "turn session={} kind={} model={} outcome={outcome} secs={:.1} steps={steps} tools={} approvals={} errors={} tokens=in:{},out:{},cache_read:{},cache_write:{} compactions={compactions}",
        identifier(session),
        identifier(&format!("{kind:?}").to_ascii_lowercase()),
        if models.is_empty() {
            "-".to_string()
        } else {
            models.join("+")
        },
        took.as_secs_f64(),
        list(
            tools
                .iter()
                .map(|(n, (ok, err, not, ms))| format!(
                    "{n}:{ok}/{err}/{not}:{:.1}s",
                    *ms as f64 / 1000.0
                ))
                .collect()
        ),
        list(approvals.iter().map(|(k, v)| format!("{k}:{v}")).collect()),
        list(errors.iter().map(|(k, v)| format!("{k}:{v}")).collect()),
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
    )
}

/// The kind of an error, from its text (which itself is never logged).
fn error_class(text: &str) -> &'static str {
    let t = text.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["overloaded", " 529"]) {
        "overloaded"
    } else if has(&[
        "rate limit",
        "rate_limit",
        "too many requests",
        " 429",
        "quota",
        "usage limit",
    ]) {
        "rate_limit"
    } else if has(&[
        "context length",
        "context window",
        "too long",
        "maximum context",
        "exceeds",
    ]) {
        "context_length"
    } else if has(&["timed out", "timeout", "deadline"]) {
        "timeout"
    } else if has(&[
        " 401",
        " 403",
        "unauthorized",
        "forbidden",
        "api key",
        "sign in",
        "signed out",
        "authentication",
    ]) {
        "auth"
    } else if has(&[
        "connect",
        "network",
        "dns",
        "unreachable",
        "reset by peer",
        "broken pipe",
    ]) {
        "network"
    } else if has(&[
        " 500",
        " 502",
        " 503",
        " 504",
        "server error",
        "internal error",
        "bad gateway",
        "service unavailable",
    ]) {
        "server"
    } else if has(&["stopped", "cancel"]) {
        "stopped"
    } else {
        "other"
    }
}

/// Letters, digits and `_-:./` only, bounded: a model or tool name.
fn identifier(s: &str) -> String {
    s.chars()
        .take(96)
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-:./".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tessera_proto::{AssistantPart, ContentPart, ModelRef, StopReason};

    fn entry(seq: u64, body: EntryBody) -> Entry {
        Entry { seq, ts: 0, body }
    }

    #[test]
    fn a_turn_line_holds_counts_and_classes_never_content() {
        let secret = "testpw2-very-private";
        let model = ModelRef {
            provider: "openai".into(),
            model: "gpt-x".into(),
        };
        let usage = |i, o, c| Usage {
            input: i,
            output: o,
            cache_read: c,
            cache_write: 0,
            cost_usd: None,
        };
        let entries = vec![
            entry(
                1,
                EntryBody::User {
                    content: vec![ContentPart::text(format!("my note says {secret}"))],
                    author: None,
                },
            ),
            entry(
                2,
                EntryBody::Assistant {
                    content: vec![AssistantPart::ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        arguments: json!({"command": format!("echo {secret}")}),
                    }],
                    model: model.clone(),
                    usage: usage(1000, 50, 800),
                    stop: StopReason::ToolUse,
                    error: None,
                    author: None,
                },
            ),
            entry(
                3,
                EntryBody::ToolResult {
                    call_id: "c1".into(),
                    tool: "bash".into(),
                    content: vec![ContentPart::text(secret)],
                    is_error: false,
                    artifact: None,
                    details: Some(json!({"duration_ms": 1500})),
                },
            ),
            entry(
                4,
                EntryBody::ToolResult {
                    call_id: "c2".into(),
                    tool: "bash".into(),
                    content: vec![ContentPart::text("Not performed: refused")],
                    is_error: true,
                    artifact: None,
                    details: None,
                },
            ),
            entry(
                5,
                EntryBody::Assistant {
                    content: vec![],
                    model,
                    usage: usage(1200, 10, 1000),
                    stop: StopReason::Error,
                    error: Some(format!("503 Service Unavailable: {secret}")),
                    author: None,
                },
            ),
            entry(
                6,
                EntryBody::Notice {
                    level: NoticeLevel::Error,
                    text: secret.into(),
                    learned: vec![],
                },
            ),
        ];
        let approvals = BTreeMap::from([("allowed", 1), ("declined", 2)]);
        let line = turn_line(
            "01chat",
            SessionKind::Chat,
            &entries,
            std::time::Duration::from_millis(4200),
            &TurnEnd::Failed(&format!("The model is overloaded ({secret})")),
            &approvals,
        );
        assert!(
            !line.contains("testpw2") && !line.contains("private"),
            "{line}"
        );
        assert_eq!(
            line,
            "turn session=01chat kind=chat model=openai/gpt-x outcome=failed:overloaded secs=4.2 steps=2 tools=bash:1/0/1:1.5s approvals=allowed:1,declined:2 errors=notice:1,overloaded:1,server:1 tokens=in:2200,out:60,cache_read:1800,cache_write:0 compactions=0"
        );
    }

    #[test]
    fn approvals_are_counted_per_session_until_its_turn_ends() {
        approval("trace-test-a", "allowed");
        approval("trace-test-a", "expired");
        approval("trace-test-b", "declined");
        let a = approvals().lock().remove("trace-test-a").unwrap();
        assert_eq!(a, BTreeMap::from([("allowed", 1), ("expired", 1)]));
        assert!(approvals().lock().remove("trace-test-b").is_some());
        // An empty turn still makes a line, with dashes.
        let line = turn_line(
            "s",
            SessionKind::Subagent,
            &[],
            std::time::Duration::ZERO,
            &TurnEnd::Stopped,
            &BTreeMap::new(),
        );
        assert!(
            line.contains("kind=subagent model=- outcome=stopped"),
            "{line}"
        );
        assert!(line.contains("tools=- approvals=- errors=-"), "{line}");
    }
}
