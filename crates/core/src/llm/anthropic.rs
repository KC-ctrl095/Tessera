//! Anthropic Messages API.

use serde_json::{Value, json};
use tessera_proto::{AssistantPart, ContentPart, StopReason, ThinkingLevel, Usage};
use tokio_util::sync::CancellationToken;

use super::sse::SseReader;
use super::{Assembler, ErrorKind, EventTx, LlmError, LlmRequest, LlmResponse, Msg};

pub const OPAQUE_TAG: &str = "anthropic";

pub struct Endpoint<'a> {
    pub http: &'a reqwest::Client,
    pub base_url: &'a str,
    pub headers: &'a [(String, String)],
    pub max_output: u64,
}

/// Models before the 4.6 generation take `budget_tokens` thinking and no effort.
fn legacy_thinking(model: &str) -> bool {
    [
        "claude-3",
        "haiku-4-5",
        "sonnet-4-5",
        "opus-4-5",
        "opus-4-1",
        "opus-4-0",
        "sonnet-4-0",
        "claude-opus-4-2025",
        "claude-sonnet-4-2025",
    ]
    .iter()
    .any(|p| model.contains(p))
}

pub async fn stream(
    ep: &Endpoint<'_>,
    req: &LlmRequest,
    tx: &EventTx,
    cancel: &CancellationToken,
) -> Result<LlmResponse, LlmError> {
    let mut body = build_body(req, ep.max_output);
    let url = format!(
        "{}/v1/messages",
        ep.base_url.trim_end_matches('/').trim_end_matches("/v1")
    );
    let mut stripped = false;
    let resp = loop {
        let mut rb = ep
            .http
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        for (k, v) in ep.headers {
            rb = rb.header(k, v);
        }
        let resp = tokio::select! {
            r = rb.send() => r.map_err(LlmError::network)?,
            _ = cancel.cancelled() => return Ok(LlmResponse { content: vec![], usage: Usage::default(), stop: StopReason::Aborted, allow_tool_repair: false }),
        };
        super::check_redirect(&resp)?;
        if resp.status().is_success() {
            break resp;
        }
        let status = resp.status().as_u16();
        let ra = super::retry_after(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        // Recent Claude models refuse reasoning replayed into a history that
        // changed since it was written ("bound to a different conversation"):
        // the conversation goes once more without any (Anthropic's advice).
        if status == 400 && !stripped && refused_thinking(&text) {
            tracing::warn!("Claude refused replayed reasoning; sending the request without it");
            stripped = true;
            body = build_body(&without_thinking(req), ep.max_output);
            continue;
        }
        return Err(LlmError::from_http(status, &text, ra));
    };

    let mut reader = SseReader::new(resp);
    let mut asm = Assembler::default();
    let mut slots: Vec<(u64, usize, &'static str)> = Vec::new();
    let mut signatures: Vec<(usize, String)> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = StopReason::Error;
    let mut refusal = false;
    let mut ended = false;

    loop {
        let ev = tokio::select! {
            ev = reader.next() => ev?,
            _ = cancel.cancelled() => return Ok(LlmResponse { content: asm.finish(), usage, stop: StopReason::Aborted, allow_tool_repair: false }),
        };
        let Some(ev) = ev else { break };
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            continue;
        };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        let slot = |slots: &Vec<(u64, usize, &'static str)>| {
            slots
                .iter()
                .find(|(i, _, _)| *i == idx)
                .map(|(_, s, k)| (*s, *k))
        };
        match ty {
            "message_start" => {
                let u = &v["message"]["usage"];
                usage.input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                usage.cache_read = u
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                usage.cache_write = u
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            "content_block_start" => {
                let b = &v["content_block"];
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => slots.push((idx, asm.begin_text(), "text")),
                    Some("thinking") => slots.push((idx, asm.begin_thinking_shown(tx), "thinking")),
                    Some("redacted_thinking") => {
                        let data = b.get("data").cloned().unwrap_or(Value::Null);
                        asm.push_opaque_thinking(
                            String::new(),
                            json!({"provider": OPAQUE_TAG, "redacted": data}),
                        );
                    }
                    Some("tool_use") => {
                        let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                        slots.push((idx, asm.tool_start(tx, id, name), "tool"));
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let Some((s, _)) = slot(&slots) else { continue };
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        asm.text_at(tx, s, d.get("text").and_then(Value::as_str).unwrap_or(""))
                    }
                    Some("thinking_delta") => asm.thinking_at(
                        tx,
                        s,
                        d.get("thinking").and_then(Value::as_str).unwrap_or(""),
                    ),
                    Some("signature_delta") => {
                        let sig = d.get("signature").and_then(Value::as_str).unwrap_or("");
                        match signatures.iter_mut().find(|(i, _)| *i == s) {
                            Some((_, acc)) => acc.push_str(sig),
                            None => signatures.push((s, sig.to_string())),
                        }
                    }
                    Some("input_json_delta") => asm.tool_args(
                        tx,
                        s,
                        d.get("partial_json").and_then(Value::as_str).unwrap_or(""),
                    ),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some((s, "thinking")) = slot(&slots)
                    && let Some((_, sig)) = signatures.iter().find(|(i, _)| *i == s)
                {
                    asm.set_opaque_at(s, json!({"provider": OPAQUE_TAG, "signature": sig}));
                }
            }
            "message_delta" => {
                // Cumulative counts; input and cache counts appear here when
                // they changed during the response (server tools).
                let u = &v["usage"];
                for (key, slot) in [
                    ("output_tokens", &mut usage.output),
                    ("input_tokens", &mut usage.input),
                    ("cache_read_input_tokens", &mut usage.cache_read),
                    ("cache_creation_input_tokens", &mut usage.cache_write),
                ] {
                    if let Some(n) = u.get(key).and_then(Value::as_u64) {
                        *slot = n;
                    }
                }
                ended |= v["delta"].get("stop_reason").is_some_and(|r| !r.is_null());
                match v["delta"].get("stop_reason").and_then(Value::as_str) {
                    Some("tool_use") => stop = StopReason::ToolUse,
                    Some("max_tokens") => stop = StopReason::Length,
                    Some("refusal") => {
                        refusal = true;
                        stop = StopReason::Error;
                    }
                    Some("end_turn" | "stop_sequence") => stop = StopReason::Stop,
                    _ => stop = StopReason::Error,
                }
            }
            "message_stop" => {
                ended = true;
                break;
            }
            "error" => {
                let e = &v["error"];
                let msg = e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("stream error")
                    .to_string();
                let kind = match e.get("type").and_then(Value::as_str) {
                    Some("overloaded_error" | "api_error") => ErrorKind::Transient,
                    Some("rate_limit_error") => ErrorKind::RateLimited,
                    _ if msg.contains("prompt is too long") => ErrorKind::ContextOverflow,
                    _ => ErrorKind::Fatal,
                };
                return Err(LlmError::new(kind, msg));
            }
            _ => {}
        }
    }
    if !ended {
        return Err(LlmError::cut_off());
    }
    let mut content = asm.finish();
    if refusal {
        content.push(AssistantPart::Text {
            text: "[The model declined to continue this request.]".into(),
        });
    }
    Ok(LlmResponse {
        content,
        usage,
        stop,
        allow_tool_repair: matches!(stop, StopReason::Stop | StopReason::ToolUse),
    })
}

/// A 400 about a replayed `thinking` block's signature (e.g. "Invalid
/// `signature` in `thinking` block. The block is bound to a different
/// conversation.").
fn refused_thinking(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("signature") && b.contains("thinking")
}

/// `req` with every reasoning block left out (text and tool calls stay).
fn without_thinking(req: &LlmRequest) -> LlmRequest {
    let mut r = req.clone();
    for m in &mut r.messages {
        if let Msg::Assistant(parts) = m {
            parts.retain(|p| !matches!(p, AssistantPart::Thinking { .. }));
        }
    }
    r
}

pub fn build_body(req: &LlmRequest, max_output: u64) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    let push = |messages: &mut Vec<Value>, role: &str, mut blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        if let Some(last) = messages.last_mut()
            && last["role"] == role
        {
            // Merge consecutive same-role messages; tool results must lead.
            let arr = last["content"].as_array_mut().unwrap();
            arr.append(&mut blocks);
            return;
        }
        messages.push(json!({"role": role, "content": blocks}));
    };

    for msg in &req.messages {
        match msg {
            Msg::User(parts) => push(
                &mut messages,
                "user",
                parts.iter().map(content_block).collect(),
            ),
            Msg::Assistant(parts) => {
                let blocks = parts
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
                        AssistantPart::Text { .. } => None,
                        AssistantPart::Thinking { text, opaque: Some(o) } if o.get("provider").and_then(Value::as_str) == Some(OPAQUE_TAG) => {
                            if let Some(data) = o.get("redacted") {
                                Some(json!({"type": "redacted_thinking", "data": data}))
                            } else {
                                o.get("signature").map(|sig| json!({"type": "thinking", "thinking": text, "signature": sig}))
                            }
                        }
                        AssistantPart::Thinking { .. } => None,
                        AssistantPart::ToolCall { id, name, arguments } => {
                            let input = if arguments.get("__invalid_arguments").is_some() { json!({}) } else { arguments.clone() };
                            Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
                        }
                    })
                    .collect();
                push(&mut messages, "assistant", blocks);
            }
            Msg::ToolResult {
                call_id,
                content,
                is_error,
                ..
            } => {
                let mut inner: Vec<Value> = content.iter().map(content_block).collect();
                if inner.is_empty() {
                    inner.push(json!({"type": "text", "text": "(no output)"}));
                }
                push(
                    &mut messages,
                    "user",
                    vec![
                        json!({"type": "tool_result", "tool_use_id": call_id, "content": inner, "is_error": is_error}),
                    ],
                );
            }
        }
    }

    let max_tokens = req.max_output.unwrap_or(max_output).max(1024);
    // Prompt caching: a breakpoint after the tools and the (frozen) system
    // prompt keeps that prefix cached whatever happens later, and the
    // automatic one follows the end of the conversation.
    let mut system = json!({"type": "text", "text": req.system});
    if !req.system.is_empty() {
        system["cache_control"] = json!({"type": "ephemeral"});
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "system": [system],
        "messages": messages,
        "stream": true,
        "cache_control": {"type": "ephemeral"},
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters, "eager_input_streaming": true}))
            .collect();
        if req.no_tool_calls {
            // Keeps the tools and system prompt cached (removing the tools
            // would not).
            body["tool_choice"] = json!({"type": "none"});
        }
    }
    apply_thinking(&mut body, &req.model, req.thinking, max_tokens);
    body
}

/// Each Claude generation takes thinking differently: budgets on older
/// models, adaptive thinking + effort on current ones, and only some current
/// models can switch thinking off (Sonnet 5.5 via `between_tools`).
fn apply_thinking(body: &mut Value, model: &str, level: ThinkingLevel, max_tokens: u64) {
    if level == ThinkingLevel::Auto {
        return; // provider defaults
    }
    if legacy_thinking(model) {
        let budget = match level {
            ThinkingLevel::Auto | ThinkingLevel::Off => 0,
            ThinkingLevel::Minimal | ThinkingLevel::Low => 4_000,
            ThinkingLevel::Medium => 10_000,
            ThinkingLevel::High => 24_000,
            ThinkingLevel::XHigh | ThinkingLevel::Max => 48_000,
        };
        if budget > 0 {
            let budget = budget.min(max_tokens.saturating_sub(1024)).max(1024);
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        }
        return;
    }
    if level == ThinkingLevel::Off {
        if model.contains("sonnet-5-5") {
            body["thinking"] = json!({"type": "between_tools"});
            body["output_config"] = json!({"effort": "medium"});
        } else if model.contains("opus-4-6")
            || model.contains("sonnet-4-6")
            || model.contains("opus-4-7")
            || model.contains("opus-4-8")
        {
            body["thinking"] = json!({"type": "disabled"});
        } else {
            // Models that cannot disable thinking: fastest available setting.
            body["output_config"] = json!({"effort": "low"});
        }
        return;
    }
    let effort = match level {
        ThinkingLevel::Minimal => "low",
        other => crate::thinking::effort_word(other),
    };
    body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
    body["output_config"] = json!({"effort": effort});
}

fn content_block(p: &ContentPart) -> Value {
    match p {
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::Image { mime, data } => {
            json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}})
        }
    }
}

/// A model in `GET /v1/models`.
pub struct ListedModel {
    pub id: String,
    pub name: Option<String>,
    pub context: Option<u64>,
    pub max_output: Option<u64>,
    /// Takes images (`capabilities.image_input`), when it says.
    pub vision: Option<bool>,
}

/// `GET /v1/models`, newest first.
pub async fn list_models(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
) -> Result<Vec<ListedModel>, LlmError> {
    let url = format!(
        "{}/v1/models?limit=100",
        base_url.trim_end_matches('/').trim_end_matches("/v1")
    );
    let mut rb = http
        .get(url)
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(10));
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.map_err(LlmError::network)?;
    super::check_redirect(&resp)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(LlmError::network)?;
    if !(200..300).contains(&status) {
        return Err(LlmError::from_http(status, &text, None));
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(v.get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(ListedModel {
                        id: m.get("id")?.as_str()?.to_string(),
                        name: m
                            .get("display_name")
                            .and_then(Value::as_str)
                            .filter(|n| !n.is_empty())
                            .map(str::to_string),
                        context: m.get("max_input_tokens").and_then(Value::as_u64),
                        max_output: m.get("max_tokens").and_then(Value::as_u64),
                        vision: m
                            .pointer("/capabilities/image_input/supported")
                            .and_then(Value::as_bool),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thinking(model: &str, level: ThinkingLevel) -> Value {
        let mut body = json!({});
        apply_thinking(&mut body, model, level, 64_000);
        body
    }

    /// Each family gets the request shape its API accepts (a wrong one is a 400).
    #[test]
    fn thinking_bodies_match_each_model() {
        use ThinkingLevel::*;
        // Auto leaves the provider default alone.
        assert_eq!(thinking("claude-opus-5-5", Auto), json!({}));
        // Levels are adaptive thinking plus effort, with readable summaries.
        let b = thinking("claude-opus-5-5", XHigh);
        assert_eq!(
            b["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(b["output_config"]["effort"], "xhigh");
        assert_eq!(
            thinking("claude-fable-5-1", Max)["output_config"]["effort"],
            "max"
        );
        assert_eq!(
            thinking("claude-sonnet-5-5", Minimal)["output_config"]["effort"],
            "low"
        );
        // Sonnet 5.5 rejects `disabled`: off is `between_tools` at effort ≤ high, nothing else beside it.
        let b = thinking("claude-sonnet-5-5", Off);
        assert_eq!(b["thinking"], json!({"type": "between_tools"}));
        assert_eq!(b["output_config"]["effort"], "medium");
        // Opus/Sonnet 4.6–4.8 can disable thinking outright.
        assert_eq!(
            thinking("claude-opus-4-8", Off)["thinking"],
            json!({"type": "disabled"})
        );
        assert_eq!(
            thinking("claude-sonnet-4-6", Off)["thinking"],
            json!({"type": "disabled"})
        );
        // Models that always think fall back to their cheapest effort.
        let b = thinking("claude-opus-5-5", Off);
        assert!(b.get("thinking").is_none());
        assert_eq!(b["output_config"]["effort"], "low");
        // Older models use explicit budgets that fit under max_tokens.
        assert_eq!(
            thinking("claude-haiku-4-5", Medium)["thinking"],
            json!({"type": "enabled", "budget_tokens": 10_000})
        );
        assert_eq!(thinking("claude-haiku-4-5", Off), json!({}));
        let mut small = json!({});
        apply_thinking(&mut small, "claude-haiku-4-5", High, 8_000);
        assert_eq!(small["thinking"]["budget_tokens"], 6_976);
    }

    #[test]
    fn merges_tool_results_and_replays_signed_thinking() {
        let req = LlmRequest {
            model: "claude-opus-5-5".into(),
            system: "s".into(),
            messages: vec![
                Msg::User(vec![ContentPart::text("go")]),
                Msg::Assistant(vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        opaque: Some(json!({"provider": "anthropic", "signature": "sig"})),
                    },
                    AssistantPart::Thinking {
                        text: "foreign".into(),
                        opaque: None,
                    },
                    AssistantPart::ToolCall {
                        id: "a".into(),
                        name: "x".into(),
                        arguments: json!({}),
                    },
                    AssistantPart::ToolCall {
                        id: "b".into(),
                        name: "x".into(),
                        arguments: json!({}),
                    },
                ]),
                Msg::ToolResult {
                    call_id: "a".into(),
                    name: "x".into(),
                    content: vec![ContentPart::text("1")],
                    is_error: false,
                },
                Msg::ToolResult {
                    call_id: "b".into(),
                    name: "x".into(),
                    content: vec![],
                    is_error: true,
                },
                Msg::User(vec![ContentPart::text("steer")]),
            ],
            tools: vec![],
            thinking: ThinkingLevel::High,
            max_output: None,
            cache_key: "k".into(),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let b = build_body(&req, 64000);
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(
            m[1]["content"].as_array().unwrap().len(),
            3,
            "foreign thinking dropped"
        );
        assert_eq!(m[1]["content"][0]["signature"], "sig");
        let last = m[2]["content"].as_array().unwrap();
        assert_eq!(last.len(), 3);
        assert_eq!(last[0]["type"], "tool_result");
        assert_eq!(last[2]["type"], "text");
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "high");
    }

    /// Reasoning Claude refuses (the history changed since it was written)
    /// costs one retry without it, not the turn.
    #[tokio::test]
    async fn refused_reasoning_is_left_out_and_the_request_sent_again() {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<Value>>> = Default::default();
        let log = seen.clone();
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let log = log.clone();
                async move {
                    let n = {
                        let mut s = log.lock().unwrap();
                        s.push(body);
                        s.len()
                    };
                    if n == 1 {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation."}}"#,
                        )
                            .into_response();
                    }
                    let events: String = [
                        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}}),
                        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}}),
                        json!({"type": "content_block_stop", "index": 0}),
                        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
                        json!({"type": "message_stop"}),
                    ]
                    .iter()
                    .map(|e| format!("data: {e}\n\n"))
                    .collect();
                    ([("content-type", "text/event-stream")], events).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let ep = Endpoint {
            http: &http,
            base_url: &base,
            headers: &[],
            max_output: 8_000,
        };
        let req = LlmRequest {
            model: "claude-opus-5-5".into(),
            system: "s".into(),
            messages: vec![
                Msg::User(vec![ContentPart::text("go")]),
                Msg::Assistant(vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        opaque: Some(json!({"provider": "anthropic", "signature": "sig"})),
                    },
                    AssistantPart::Text {
                        text: "done".into(),
                    },
                ]),
                Msg::User(vec![ContentPart::text("next")]),
            ],
            tools: vec![],
            thinking: ThinkingLevel::High,
            max_output: None,
            cache_key: "k".into(),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let r = stream(&ep, &req, &tx, &CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(&r.content[0], AssistantPart::Text { text } if text == "ok"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0]["messages"][1]["content"][0]["type"], "thinking");
        // Sent again with the words and calls, without the reasoning.
        assert_eq!(
            seen[1]["messages"][1]["content"],
            json!([{"type": "text", "text": "done"}])
        );
        assert!(refused_thinking("Invalid `signature` in `thinking` block"));
        assert!(!refused_thinking("prompt is too long"));
    }

    /// No calls allowed: the tools stay (their cache and the system
    /// prompt's hold), with `tool_choice: none`.
    #[test]
    fn tools_stay_listed_when_no_call_is_allowed() {
        let mut req = LlmRequest {
            model: "claude-opus-5-5".into(),
            system: "sys".into(),
            messages: vec![Msg::User(vec![ContentPart::text("hi")])],
            tools: vec![crate::llm::ToolSpec {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            thinking: ThinkingLevel::High,
            max_output: None,
            cache_key: "k".into(),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let open = build_body(&req, 64000);
        assert!(open.get("tool_choice").is_none());
        req.no_tool_calls = true;
        let quiet = build_body(&req, 64000);
        assert_eq!(quiet["tool_choice"], json!({"type": "none"}));
        assert_eq!(quiet["tools"], open["tools"]);
        assert_eq!(quiet["thinking"], open["thinking"]);
    }
}
