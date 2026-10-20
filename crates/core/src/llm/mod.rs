//! Provider-neutral model interface.
//!
//! Adapters translate `LlmRequest` to each wire format and stream back
//! `LlmEvent`s. The conversation is always append-only from the adapter's point
//! of view: history edits happen only at compaction boundaries, which keeps
//! prompt caches warm and satisfies providers that bind reasoning to the exact
//! prefix that produced it.

pub mod anthropic;
pub mod local;
pub mod openai_chat;
pub mod openrouter;
pub mod responses;
pub mod retry;
pub mod sse;
pub mod window;

use serde_json::Value;
use tessera_proto::{AssistantPart, ContentPart, StopReason, ThinkingLevel, Usage};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum Msg {
    User(Vec<ContentPart>),
    Assistant(Vec<AssistantPart>),
    ToolResult {
        call_id: String,
        name: String,
        content: Vec<ContentPart>,
        is_error: bool,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Msg>,
    pub tools: Vec<ToolSpec>,
    pub thinking: ThinkingLevel,
    pub max_output: Option<u64>,
    /// Stable per-conversation key for provider-side prompt caching.
    pub cache_key: String,
    /// Fast mode: priority processing, where the model offers it.
    pub fast: bool,
    /// Per-chat opt-in. OpenAI discovery must confirm access before dispatch.
    pub daybreak: bool,
    /// The model may not call any of `tools` this time (`tool_choice:
    /// none`): they stay listed as before, so the prompt cache holds. Only
    /// set for endpoints known to honour it (see
    /// `Providers::takes_tool_choice_none`).
    pub no_tool_calls: bool,
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    Text {
        index: u32,
        text: String,
    },
    Thinking {
        index: u32,
        text: String,
    },
    ToolCall {
        index: u32,
        id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Vec<AssistantPart>,
    pub usage: Usage,
    pub stop: StopReason,
    /// An affirmative provider end reason, not just a transport marker.
    /// Unconfirmed streams may still display text but cannot invent calls.
    pub allow_tool_repair: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Network hiccup, 5xx, overloaded: retry with backoff.
    Transient,
    RateLimited,
    /// Prompt exceeds the model window: compact and retry.
    ContextOverflow,
    Auth,
    /// Bad request or anything retrying will not fix.
    Fatal,
}

#[derive(Debug, Clone)]
pub struct LlmError {
    pub kind: ErrorKind,
    pub message: String,
    pub retry_after: Option<std::time::Duration>,
    /// Explicit subscription-plan reset in the error body, not merely a
    /// rate-limit Retry-After header. It has its own longer wait policy.
    pub plan_reset: bool,
    /// Nothing answered at the address (connection refused, no route, no
    /// such host): the server is off rather than failing.
    pub unreachable: bool,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for LlmError {}

impl LlmError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after: None,
            plan_reset: false,
            unreachable: false,
        }
    }

    /// The stream ended without the provider's end-of-reply marker: the
    /// connection dropped mid-reply. Retried, never taken as the answer (a
    /// cut-off tool call would run with half its arguments).
    pub fn cut_off() -> Self {
        Self::new(
            ErrorKind::Transient,
            "the connection closed before the reply finished",
        )
    }

    /// Classify an HTTP error response.
    pub fn from_http(status: u16, body: &str, retry_after: Option<std::time::Duration>) -> Self {
        let lower = body.to_ascii_lowercase();
        let overflow = [
            "context_length_exceeded",
            "maximum context length",
            "prompt is too long",
            "too many tokens",
            "context window",
            "exceeds the context",
            "input is too long",
            "exceed_context_size",
            "exceeds the available context size",
        ]
        .iter()
        .any(|p| lower.contains(p));
        let kind = if overflow {
            ErrorKind::ContextOverflow
        } else {
            match status {
                401 | 403 => ErrorKind::Auth,
                429 => ErrorKind::RateLimited,
                408 | 409 | 425 | 500..=599 => ErrorKind::Transient,
                _ if lower.contains("overloaded") => ErrorKind::Transient,
                _ => ErrorKind::Fatal,
            }
        };
        let detail =
            extract_error_message(body).unwrap_or_else(|| crate::util::truncate(body.trim(), 600));
        // A subscription's usage limit says when it resets (ChatGPT:
        // `resets_in_seconds` or `resets_at`), often hours away.
        let reset = (kind == ErrorKind::RateLimited)
            .then(|| limit_reset(body))
            .flatten();
        let plan_reset = reset.is_some()
            && serde_json::from_str::<Value>(body).ok().is_some_and(|v| {
                v.get("error")
                    .unwrap_or(&v)
                    .get("type")
                    .and_then(Value::as_str)
                    == Some("usage_limit_reached")
            });
        let retry_after = reset.or(retry_after);
        Self {
            kind,
            message: format!("HTTP {status}: {detail}"),
            retry_after,
            plan_reset,
            unreachable: false,
        }
    }

    pub fn network(e: reqwest::Error) -> Self {
        Self {
            unreachable: e.is_connect(),
            ..Self::new(ErrorKind::Transient, format!("network error: {e}"))
        }
    }
}

/// Provider redirects must not move credentials, prompts or non-loading reads
/// to another endpoint. Optional reads log the same plain error when skipped.
pub(super) fn check_redirect(resp: &reqwest::Response) -> Result<(), LlmError> {
    if resp.status().is_redirection() {
        let error = LlmError::new(
            ErrorKind::Fatal,
            format!(
                "Provider returned HTTP {} redirect. Tessera does not follow provider redirects. Set the provider address to its final endpoint.",
                resp.status().as_u16()
            ),
        );
        // No Location/URL here: they can contain credentials.
        tracing::warn!("{error}");
        return Err(error);
    }
    Ok(())
}

/// When a usage limit named in an error body resets, from now.
fn limit_reset(body: &str) -> Option<std::time::Duration> {
    let v: Value = serde_json::from_str(body).ok()?;
    let e = v.get("error").unwrap_or(&v);
    let secs = match e.get("resets_in_seconds").and_then(Value::as_u64) {
        Some(s) => s,
        None => {
            let at = e.get("resets_at").and_then(Value::as_u64)?;
            at.saturating_sub(crate::util::now_ms() / 1000)
        }
    };
    Some(std::time::Duration::from_secs(secs))
}

fn extract_error_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let e = v.get("error").unwrap_or(&v);
    e.get("message")
        .and_then(Value::as_str)
        .or_else(|| e.as_str())
        .or_else(|| v.get("detail").and_then(Value::as_str))
        .map(str::to_string)
}

/// What a server's `Retry-After` asks for: seconds, or a date (`Wed, 21 Oct
/// 2026 07:28:00 GMT`), at most ten minutes (`retry.rs` cuts it shorter for a
/// chat on the screen).
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let v = headers.get("retry-after")?.to_str().ok()?.trim();
    let secs = v
        .parse::<f64>()
        .ok()
        .or_else(|| {
            let at = time::OffsetDateTime::parse(v, &time::format_description::well_known::Rfc2822)
                .ok()?;
            Some((at - time::OffsetDateTime::now_utc()).as_seconds_f64())
        })
        // "NaN" parses as a number and would panic below.
        .filter(|s| s.is_finite())?;
    Some(std::time::Duration::from_secs_f64(secs.clamp(0.0, 600.0)))
}

pub type EventTx = mpsc::UnboundedSender<LlmEvent>;

/// Accumulates streamed parts into a final assistant message.
#[derive(Default)]
pub struct Assembler {
    parts: Vec<Part>,
}

enum Part {
    Text(String),
    Thinking(String, Option<Value>),
    Tool {
        id: String,
        name: String,
        args: String,
    },
}

impl Assembler {
    pub fn text(&mut self, tx: &EventTx, s: &str) {
        if s.is_empty() {
            return;
        }
        let index = match self.parts.last_mut() {
            Some(Part::Text(t)) => {
                t.push_str(s);
                self.parts.len() - 1
            }
            _ => {
                self.parts.push(Part::Text(s.to_string()));
                self.parts.len() - 1
            }
        };
        let _ = tx.send(LlmEvent::Text {
            index: index as u32,
            text: s.to_string(),
        });
    }

    pub fn thinking(&mut self, tx: &EventTx, s: &str) {
        if s.is_empty() {
            return;
        }
        let index = match self.parts.last_mut() {
            Some(Part::Thinking(t, _)) => {
                t.push_str(s);
                self.parts.len() - 1
            }
            _ => {
                self.parts.push(Part::Thinking(s.to_string(), None));
                self.parts.len() - 1
            }
        };
        let _ = tx.send(LlmEvent::Thinking {
            index: index as u32,
            text: s.to_string(),
        });
    }

    /// Start a new thinking block explicitly (providers with block boundaries).
    pub fn begin_thinking(&mut self) -> usize {
        self.parts.push(Part::Thinking(String::new(), None));
        self.parts.len() - 1
    }

    /// A reasoning block starts, told to clients at once: models whose
    /// reasoning is hidden (or summarised late) can think for minutes, and
    /// "Thinking · 4m" reads very differently from a silent chat.
    pub fn begin_thinking_shown(&mut self, tx: &EventTx) -> usize {
        let index = self.begin_thinking();
        let _ = tx.send(LlmEvent::Thinking {
            index: index as u32,
            text: String::new(),
        });
        index
    }

    /// Append to a specific thinking block.
    pub fn thinking_at(&mut self, tx: &EventTx, index: usize, s: &str) {
        if let Some(Part::Thinking(t, _)) = self.parts.get_mut(index) {
            t.push_str(s);
            let _ = tx.send(LlmEvent::Thinking {
                index: index as u32,
                text: s.to_string(),
            });
        }
    }

    pub fn set_opaque_at(&mut self, index: usize, opaque: Value) {
        if let Some(Part::Thinking(_, o)) = self.parts.get_mut(index) {
            *o = Some(opaque);
        }
    }

    /// Start a new text block explicitly; returns its index.
    pub fn begin_text(&mut self) -> usize {
        self.parts.push(Part::Text(String::new()));
        self.parts.len() - 1
    }

    pub fn text_at(&mut self, tx: &EventTx, index: usize, s: &str) {
        if let Some(Part::Text(t)) = self.parts.get_mut(index) {
            t.push_str(s);
            let _ = tx.send(LlmEvent::Text {
                index: index as u32,
                text: s.to_string(),
            });
        }
    }

    pub fn thinking_into_last(&mut self, tx: &EventTx, s: &str) {
        if let Some(Part::Thinking(t, _)) = self.parts.last_mut() {
            t.push_str(s);
            let index = self.parts.len() - 1;
            let _ = tx.send(LlmEvent::Thinking {
                index: index as u32,
                text: s.to_string(),
            });
        } else {
            self.thinking(tx, s);
        }
    }

    /// Attach opaque replay data to the most recent thinking block, creating
    /// an empty one when the provider sent none (e.g. hidden reasoning).
    pub fn set_thinking_opaque(&mut self, opaque: Value) {
        if let Some(Part::Thinking(_, o)) = self.parts.last_mut() {
            *o = Some(opaque);
        } else {
            self.parts.push(Part::Thinking(String::new(), Some(opaque)));
        }
    }

    pub fn push_opaque_thinking(&mut self, text: String, opaque: Value) {
        self.parts.push(Part::Thinking(text, Some(opaque)));
    }

    /// Begin a tool call; returns its part index.
    pub fn tool_start(&mut self, tx: &EventTx, id: &str, name: &str) -> usize {
        self.parts.push(Part::Tool {
            id: id.to_string(),
            name: name.to_string(),
            args: String::new(),
        });
        let index = self.parts.len() - 1;
        let _ = tx.send(LlmEvent::ToolCall {
            index: index as u32,
            id: id.into(),
            name: name.into(),
            arguments: String::new(),
        });
        index
    }

    pub fn tool_args(&mut self, tx: &EventTx, index: usize, delta: &str) {
        if let Some(Part::Tool { id, name, args }) = self.parts.get_mut(index) {
            args.push_str(delta);
            let _ = tx.send(LlmEvent::ToolCall {
                index: index as u32,
                id: id.clone(),
                name: name.clone(),
                arguments: delta.to_string(),
            });
        }
    }

    pub fn tool_set_args(&mut self, index: usize, full: &str) {
        if let Some(Part::Tool { args, .. }) = self.parts.get_mut(index) {
            *args = full.to_string();
        }
    }

    pub fn has_tool_calls(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Tool { .. }))
    }

    pub fn finish(self) -> Vec<AssistantPart> {
        let mut out = Vec::new();
        for p in self.parts {
            match p {
                Part::Text(t) => {
                    if !t.is_empty() {
                        out.push(AssistantPart::Text { text: t });
                    }
                }
                Part::Thinking(t, opaque) => {
                    if !t.is_empty() || opaque.is_some() {
                        out.push(AssistantPart::Thinking { text: t, opaque });
                    }
                }
                Part::Tool { id, name, args } => {
                    let arguments = parse_tool_args(&args);
                    let id = if id.is_empty() {
                        format!("call_{}", crate::util::new_id())
                    } else {
                        id
                    };
                    out.push(AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }
        out
    }
}

/// Parse streamed tool arguments. Invalid JSON is preserved under a marker key
/// so the tool layer can report a precise error to the model instead of
/// silently running with wrong input.
pub fn parse_tool_args(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return Value::Object(Default::default());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v @ Value::Object(_)) => v,
        Ok(other) => serde_json::json!({ "__invalid_arguments": other.to_string() }),
        Err(_) => serde_json::json!({ "__invalid_arguments": raw }),
    }
}

/// Split `<think>…</think>` spans out of content for servers that inline
/// reasoning into the text stream.
#[derive(Default)]
pub struct ThinkTagSplitter {
    in_think: bool,
    pending: String,
    seen_text: bool,
}

impl ThinkTagSplitter {
    pub fn feed(&mut self, asm: &mut Assembler, tx: &EventTx, chunk: &str) {
        self.pending.push_str(chunk);
        loop {
            if self.in_think {
                if let Some(end) = self.pending.find("</think>") {
                    let t = self.pending[..end].to_string();
                    asm.thinking(tx, &t);
                    self.pending.drain(..end + "</think>".len());
                    self.in_think = false;
                    continue;
                }
                let keep = partial_suffix(&self.pending, "</think>");
                let emit = self.pending[..self.pending.len() - keep].to_string();
                asm.thinking(tx, &emit);
                self.pending.drain(..self.pending.len() - keep);
                return;
            }
            // Only a leading <think> (possibly after whitespace) opens a block.
            if !self.seen_text {
                let trimmed = self.pending.trim_start();
                if trimmed.starts_with("<think>") {
                    let start = self.pending.len() - trimmed.len();
                    self.pending.drain(..start + "<think>".len());
                    self.in_think = true;
                    continue;
                }
                if "<think>".starts_with(trimmed) && !trimmed.is_empty() || trimmed.is_empty() {
                    return; // wait for more
                }
            }
            self.seen_text = true;
            let t = std::mem::take(&mut self.pending);
            asm.text(tx, &t);
            return;
        }
    }

    pub fn flush(&mut self, asm: &mut Assembler, tx: &EventTx) {
        let t = std::mem::take(&mut self.pending);
        if self.in_think {
            asm.thinking(tx, &t);
        } else {
            asm.text(tx, &t);
        }
    }
}

fn partial_suffix(s: &str, pat: &str) -> usize {
    (1..pat.len())
        .rev()
        .find(|&n| {
            s.len() >= n && s.is_char_boundary(s.len() - n) && pat.starts_with(&s[s.len() - n..])
        })
        .unwrap_or(0)
}

pub fn image_data_url(mime: &str, data: &str) -> String {
    format!("data:{mime};base64,{data}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_limits_say_when_they_reset() {
        let body = r#"{"error": {"type": "usage_limit_reached", "message": "You've hit your usage limit.", "resets_in_seconds": 7380}}"#;
        let e = LlmError::from_http(429, body, Some(std::time::Duration::from_secs(30)));
        assert_eq!(e.kind, ErrorKind::RateLimited);
        assert_eq!(e.retry_after, Some(std::time::Duration::from_secs(7380)));
        assert!(e.plan_reset);
        let e = LlmError::from_http(429, "slow down", Some(std::time::Duration::from_secs(3600)));
        assert!(!e.plan_reset, "a header is not a subscription reset");
        let e = LlmError::from_http(429, r#"{"error":{"resets_in_seconds":3600}}"#, None);
        assert!(
            !e.plan_reset,
            "a reset duration without a plan type is not a plan reset"
        );
        let at = crate::util::now_ms() / 1000 + 600;
        let body = format!(r#"{{"error": {{"type": "usage_limit_reached", "resets_at": {at}}}}}"#);
        let w = LlmError::from_http(429, &body, None)
            .retry_after
            .unwrap()
            .as_secs();
        assert!((598..=600).contains(&w), "{w}");
        // Other errors keep the header's hint.
        let e = LlmError::from_http(503, body.as_str(), Some(std::time::Duration::from_secs(3)));
        assert_eq!(e.retry_after, Some(std::time::Duration::from_secs(3)));
        assert!(!e.plan_reset);
    }

    #[test]
    fn retry_after_takes_seconds_or_a_date_and_never_panics() {
        let ra = |v: &str| {
            let mut h = reqwest::header::HeaderMap::new();
            h.insert("retry-after", v.parse().unwrap());
            retry_after(&h)
        };
        let secs = std::time::Duration::from_secs;
        assert_eq!(ra("7"), Some(secs(7)));
        assert_eq!(ra(" 2.5 "), Some(std::time::Duration::from_millis(2500)));
        assert_eq!(ra("0"), Some(secs(0)));
        assert_eq!(ra("-4"), Some(secs(0)));
        assert_eq!(ra("400"), Some(secs(400)));
        assert_eq!(ra("99999"), Some(secs(600)));
        // A date in the future, in the past, and something else.
        let fmt = time::format_description::well_known::Rfc2822;
        let soon = (time::OffsetDateTime::now_utc() + time::Duration::seconds(45))
            .format(&fmt)
            .unwrap()
            .replace("+0000", "GMT");
        let w = ra(&soon).unwrap().as_secs();
        assert!((43..=45).contains(&w), "{soon}: {w}");
        assert_eq!(ra("Wed, 21 Oct 2015 07:28:00 GMT"), Some(secs(0)));
        assert_eq!(ra("soon"), None);
        assert_eq!(ra("NaN"), None);
        assert_eq!(ra("inf"), None);
        assert_eq!(retry_after(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn think_tags_are_split() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut asm = Assembler::default();
        let mut sp = ThinkTagSplitter::default();
        for chunk in [
            "<th",
            "ink>plan",
            "ning</thi",
            "nk>\n\nHello ",
            "<think>not thinking</think>",
        ] {
            sp.feed(&mut asm, &tx, chunk);
        }
        sp.flush(&mut asm, &tx);
        let parts = asm.finish();
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], AssistantPart::Thinking { text, .. } if text == "planning"));
        assert!(
            matches!(&parts[1], AssistantPart::Text { text } if text.contains("Hello") && text.contains("<think>not"))
        );
    }

    #[test]
    fn error_classification() {
        assert_eq!(
            LlmError::from_http(
                400,
                r#"{"error":{"message":"This model's maximum context length is 8192"}}"#,
                None
            )
            .kind,
            ErrorKind::ContextOverflow
        );
        assert_eq!(
            LlmError::from_http(529, "overloaded", None).kind,
            ErrorKind::Transient
        );
        assert_eq!(LlmError::from_http(401, "", None).kind, ErrorKind::Auth);
        assert_eq!(LlmError::from_http(400, "bad", None).kind, ErrorKind::Fatal);
    }

    #[test]
    fn invalid_args_are_flagged() {
        assert!(
            parse_tool_args("{\"a\":")
                .get("__invalid_arguments")
                .is_some()
        );
        assert_eq!(parse_tool_args("")["x"], Value::Null);
    }
}
