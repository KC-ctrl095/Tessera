//! OpenAI Responses API — used for api.openai.com and for ChatGPT
//! subscriptions through the Codex backend (`chatgpt.com/backend-api/codex`).

use serde_json::{Value, json};
use tessera_proto::{AssistantPart, ContentPart, PlanLimit, StopReason, ThinkingLevel, Usage};
use tokio_util::sync::CancellationToken;

use super::openai_chat::tool_args_string;
use super::sse::SseReader;
use super::{Assembler, EventTx, LlmError, LlmRequest, LlmResponse, Msg, image_data_url};

pub const OPAQUE_TAG: &str = "openai-responses";

pub struct Endpoint<'a> {
    pub http: &'a reqwest::Client,
    pub base_url: &'a str,
    pub headers: &'a [(String, String)],
    pub reasoning: bool,
    pub vision: bool,
    /// The ChatGPT Codex backend: no `max_output_tokens`.
    pub codex: bool,
    /// Priority processing (fast mode), already checked against the model.
    pub fast: bool,
    /// Explicit program resolved from the caller's model catalog.
    pub cyber: Option<&'a str>,
    /// Told the plan's usage limits the ChatGPT backend sends with a reply.
    pub limits: Option<&'a (dyn Fn(Vec<PlanLimit>) + Sync)>,
}

pub async fn stream(
    ep: &Endpoint<'_>,
    req: &LlmRequest,
    tx: &EventTx,
    cancel: &CancellationToken,
) -> Result<LlmResponse, LlmError> {
    let body = build_body(req, ep);
    let url = format!("{}/responses", ep.base_url.trim_end_matches('/'));
    let mut rb = ep
        .http
        .post(&url)
        .header("accept", "text/event-stream")
        .json(&body);
    for (k, v) in ep.headers {
        rb = rb.header(k, v);
    }
    if let Some((k, v)) = routing_hint(req, ep) {
        rb = rb.header(k, v);
    }
    let resp = tokio::select! {
        r = rb.send() => r.map_err(LlmError::network)?,
        _ = cancel.cancelled() => return Ok(done(Assembler::default(), Usage::default(), StopReason::Aborted)),
    };
    super::check_redirect(&resp)?;
    // A refusal for a reached limit carries them too.
    if let Some(keep) = ep.limits {
        let limits = codex_limits(resp.headers(), crate::util::now_ms());
        if !limits.is_empty() {
            keep(limits);
        }
    }
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let ra = super::retry_after(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        return Err(LlmError::from_http(status, &text, ra));
    }

    let mut reader = SseReader::new(resp);
    let mut asm = Assembler::default();
    // item id → assembler slot
    let mut slots: Vec<(String, usize)> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = None;
    let mut ended = false;

    loop {
        let ev = tokio::select! {
            ev = reader.next() => ev?,
            _ = cancel.cancelled() => return Ok(done(asm, usage, StopReason::Aborted)),
        };
        let Some(ev) = ev else { break };
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            continue;
        };
        let ty = v
            .get("type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or("");
        let item_id = || {
            v.get("item_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let slot_of = |slots: &Vec<(String, usize)>, id: &str| {
            slots.iter().find(|(i, _)| i == id).map(|(_, s)| *s)
        };
        match ty {
            "response.output_item.added" => {
                let item = &v["item"];
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                        let s = asm.tool_start(tx, call_id, name);
                        slots.push((id, s));
                    }
                    Some("reasoning") => {
                        let s = asm.begin_thinking_shown(tx);
                        slots.push((id, s));
                    }
                    Some("message") => {
                        let s = asm.begin_text();
                        slots.push((id, s));
                    }
                    _ => {}
                }
            }
            "response.output_text.delta" => {
                let d = v.get("delta").and_then(Value::as_str).unwrap_or("");
                match slot_of(&slots, &item_id()) {
                    Some(s) => asm.text_at(tx, s, d),
                    None => asm.text(tx, d),
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let d = v.get("delta").and_then(Value::as_str).unwrap_or("");
                match slot_of(&slots, &item_id()) {
                    Some(s) => asm.thinking_at(tx, s, d),
                    None => asm.thinking(tx, d),
                }
            }
            "response.reasoning_summary_part.added" => {
                // Separate successive summary paragraphs.
                if v.get("summary_index").and_then(Value::as_u64).unwrap_or(0) > 0
                    && let Some(s) = slot_of(&slots, &item_id())
                {
                    asm.thinking_at(tx, s, "\n\n");
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(s) = slot_of(&slots, &item_id()) {
                    asm.tool_args(tx, s, v.get("delta").and_then(Value::as_str).unwrap_or(""));
                }
            }
            "response.output_item.done" => {
                let item = &v["item"];
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        if let (Some(s), Some(args)) = (
                            slot_of(&slots, id),
                            item.get("arguments").and_then(Value::as_str),
                        ) {
                            asm.tool_set_args(s, args);
                        }
                    }
                    Some("reasoning") => {
                        let mut replay = item.clone();
                        if let Some(o) = replay.as_object_mut() {
                            o.remove("id");
                            o.remove("status");
                        }
                        let opaque = json!({ "provider": OPAQUE_TAG, "item": replay });
                        match slot_of(&slots, id) {
                            Some(s) => asm.set_opaque_at(s, opaque),
                            None => asm.set_thinking_opaque(opaque),
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" | "response.done" => {
                let r = &v["response"];
                if let Some(u) = r.get("usage") {
                    usage = parse_usage(u);
                }
                stop = Some(match (ty, r.get("status").and_then(Value::as_str)) {
                    ("response.incomplete", _) | (_, Some("incomplete")) => StopReason::Length,
                    (_, Some("completed")) => StopReason::Stop,
                    _ => StopReason::Error,
                });
                ended = true;
                break;
            }
            "response.failed" | "error" => {
                let err = v
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .or_else(|| v.get("error"))
                    .unwrap_or(&v);
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("response failed");
                let code = err.get("code").and_then(Value::as_str).unwrap_or("");
                let status = if code.contains("rate_limit") || code.contains("usage_limit") {
                    429
                } else if code.contains("context_length") {
                    400
                } else if code.contains("server") || code.is_empty() {
                    500
                } else {
                    400
                };
                let body =
                    json!({"error": {"message": format!("{msg} {code}").trim().to_string()}})
                        .to_string();
                return Err(LlmError::from_http(status, &body, None));
            }
            _ => {}
        }
    }
    if !ended {
        return Err(LlmError::cut_off());
    }
    let mut stop = stop.unwrap_or(StopReason::Error);
    if asm.has_tool_calls() && stop == StopReason::Stop {
        stop = StopReason::ToolUse;
    }
    Ok(done(asm, usage, stop))
}

fn done(asm: Assembler, usage: Usage, stop: StopReason) -> LlmResponse {
    LlmResponse {
        content: asm.finish(),
        usage,
        stop,
        allow_tool_repair: matches!(stop, StopReason::Stop | StopReason::ToolUse),
    }
}

fn parse_usage(u: &Value) -> Usage {
    let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cached = u
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Usage {
        input: input.saturating_sub(cached),
        output: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_read: cached,
        cache_write: 0,
        cost_usd: None,
    }
}

fn effort(level: ThinkingLevel, model: &str) -> Option<&'static str> {
    Some(match level {
        ThinkingLevel::Auto => return None,
        // gpt-5.1+ accept "none"; the original gpt-5 family only "minimal".
        ThinkingLevel::Off => {
            if model.starts_with("gpt-5.") || model.starts_with("gpt-6") {
                "none"
            } else {
                "minimal"
            }
        }
        other => crate::thinking::effort_word(other),
    })
}

pub fn build_body(req: &LlmRequest, ep: &Endpoint<'_>) -> Value {
    let mut input: Vec<Value> = Vec::new();
    for msg in &req.messages {
        match msg {
            Msg::User(parts) => {
                input.push(json!({"role": "user", "content": user_parts(parts, ep)}))
            }
            Msg::Assistant(parts) => {
                for p in parts {
                    match p {
                        AssistantPart::Text { text } if !text.is_empty() => input.push(json!({
                            "type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        })),
                        AssistantPart::Thinking { opaque: Some(o), .. } if o.get("provider").and_then(Value::as_str) == Some(OPAQUE_TAG) => {
                            if let Some(item) = o.get("item") {
                                input.push(item.clone());
                            }
                        }
                        AssistantPart::ToolCall { id, name, arguments } => input.push(json!({
                            "type": "function_call", "call_id": id, "name": name, "arguments": tool_args_string(arguments)
                        })),
                        _ => {}
                    }
                }
            }
            Msg::ToolResult {
                call_id,
                content,
                is_error,
                ..
            } => {
                let mut text = tessera_proto::plain_text(content);
                if *is_error && !text.starts_with("Error") {
                    text = format!("Error: {text}");
                }
                input.push(
                    json!({"type": "function_call_output", "call_id": call_id, "output": text}),
                );
                let images: Vec<Value> = content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Image { mime, data } if ep.vision => Some(
                            json!({"type": "input_image", "image_url": image_data_url(mime, data)}),
                        ),
                        _ => None,
                    })
                    .collect();
                if !images.is_empty() {
                    let mut c = vec![
                        json!({"type": "input_text", "text": "Image returned by the tool call above:"}),
                    ];
                    c.extend(images);
                    input.push(json!({"role": "user", "content": c}));
                }
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "instructions": req.system,
        "input": input,
        "stream": true,
        "store": false,
        "tool_choice": if req.no_tool_calls && !req.tools.is_empty() { "none" } else { "auto" },
        "parallel_tool_calls": true,
        "prompt_cache_key": req.cache_key,
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.parameters, "strict": false}))
            .collect();
    }
    if ep.reasoning {
        body["reasoning"] = match effort(req.thinking, &req.model) {
            Some(e) => json!({"effort": e, "summary": "auto"}),
            None => json!({"summary": "auto"}),
        };
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if let (Some(max), false) = (req.max_output, ep.codex) {
        body["max_output_tokens"] = json!(max);
    }
    if ep.fast {
        body["service_tier"] = json!("priority");
    }
    if let Some(program) = ep.cyber {
        body["access_programs"] = json!({"cyber": program});
    }
    body
}

/// The plan's usage windows the ChatGPT backend reports with each reply, as
/// the Codex CLI reads them: `x-codex-primary-*` (the short one, 5 hours) and
/// `x-codex-secondary-*` (the week), each `used-percent`, `window-minutes`
/// and `reset-at` (unix seconds; older backends: `reset-after-seconds`).
pub fn codex_limits(headers: &reqwest::header::HeaderMap, now: u64) -> Vec<PlanLimit> {
    let mut out = Vec::new();
    for which in ["primary", "secondary"] {
        let h = |k: &str| {
            headers
                .get(format!("x-codex-{which}-{k}"))
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
        };
        let Some(used) = h("used-percent")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|u| u.is_finite())
        else {
            continue;
        };
        let number = |k: &str| {
            h(k).and_then(|v| v.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n >= 0.0)
        };
        let minutes = number("window-minutes")
            .map(|m| m.round() as u64)
            .filter(|m| *m > 0);
        let resets_at = number("reset-at")
            .filter(|s| *s > 0.0)
            // Seconds, or already milliseconds.
            .map(|s| if s < 1e11 { s * 1000.0 } else { s } as u64)
            .or_else(|| number("reset-after-seconds").map(|s| now + (s * 1000.0) as u64));
        let (window, label) = match minutes {
            Some(m) => crate::limits::window_of_length(m),
            None if which == "primary" => crate::limits::window_of_length(300),
            None => crate::limits::window_of_length(10_080),
        };
        if out.iter().any(|l: &PlanLimit| l.window == window) {
            continue;
        }
        out.push(PlanLimit {
            window,
            label,
            used_percent: Some(used.max(0.0)),
            status: None,
            resets_at,
            updated_at: now,
        });
    }
    out
}

/// The ChatGPT backend routes fast mode by header as well (as Codex does).
pub fn routing_hint(req: &LlmRequest, ep: &Endpoint<'_>) -> Option<(&'static str, String)> {
    (ep.fast && ep.codex).then(|| {
        (
            "x-codex-routing-hint",
            format!("model={};tier=priority", req.model),
        )
    })
}

fn user_parts(parts: &[ContentPart], ep: &Endpoint<'_>) -> Value {
    Value::Array(
        parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => json!({"type": "input_text", "text": text}),
                // As the Codex CLI sends them, on both endpoints.
                ContentPart::Image { mime, data } if ep.vision => {
                    json!({"type": "input_image", "image_url": image_data_url(mime, data)})
                }
                ContentPart::Image { .. } => json!({"type": "input_text", "text": "[image omitted: this model does not accept images]"}),
            })
            .collect(),
    )
}

/// Reasoning levels (effort, description) and the default one.
pub type CodexLevels = (Vec<(String, String)>, Option<String>);

/// A model the Codex backend lists.
pub struct CodexModel {
    pub id: String,
    pub context: Option<u64>,
    /// Reasoning levels with descriptions, and the default, when listed
    /// (`supported_reasoning_levels`, `default_reasoning_level`).
    pub levels: Option<CodexLevels>,
    /// Whether `input_modalities` include images (absent: assumed, as the
    /// Codex CLI does).
    pub vision: bool,
    pub cyber_access_programs: Vec<String>,
}

/// Model list from the Codex backend.
pub async fn codex_models(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
) -> Result<Vec<CodexModel>, LlmError> {
    let mut rb = http
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .query(&[("client_version", "1.0.0")])
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
    Ok(parse_codex_models(&v))
}

fn parse_codex_models(v: &Value) -> Vec<CodexModel> {
    let list = v
        .get("models")
        .or_else(|| v.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    list.iter()
        .filter(|m| {
            m.get("visibility")
                .and_then(Value::as_str)
                .is_none_or(|s| s != "hide")
        })
        .filter_map(|m| {
            let id = m
                .get("slug")
                .or_else(|| m.get("id"))
                .and_then(Value::as_str)?
                .to_string();
            let ctx = ["context_window", "max_context_window", "context_length"]
                .iter()
                .find_map(|k| m.get(*k).and_then(Value::as_u64));
            let levels = m
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|l| match l {
                            Value::String(e) => Some((e.clone(), String::new())),
                            _ => Some((
                                l.get("effort")?.as_str()?.to_string(),
                                l.get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            )),
                        })
                        .collect::<Vec<_>>()
                });
            let default = m
                .get("default_reasoning_level")
                .and_then(Value::as_str)
                .map(str::to_string);
            let vision = m
                .get("input_modalities")
                .and_then(Value::as_array)
                .is_none_or(|a| a.iter().any(|v| v.as_str() == Some("image")));
            Some(CodexModel {
                id,
                context: ctx,
                levels: levels.filter(|l| !l.is_empty()).map(|l| (l, default)),
                vision,
                cyber_access_programs: cyber_access_programs(m),
            })
        })
        .collect()
}

/// Caller-specific selections. Unknown future programs do not imply Blue access.
fn cyber_access_programs(model: &Value) -> Vec<String> {
    model["available_access_programs"]["cyber"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|p| matches!(*p, "standard" | "daybreak_blue" | "daybreak_red"))
        .map(str::to_string)
        .collect()
}

/// The model's caller-specific catalog lists Blue (Codex's `ModelAccessPrograms`).
pub fn daybreak_available(programs: &[String]) -> bool {
    programs.iter().any(|p| p == "daybreak_blue")
}

/// Codex's per-turn selection (`codex-rs/tui/src/daybreak.rs`), without
/// changing the chosen model: a model the catalog lists no programs for keeps
/// the server's own treatment; a listed one must offer Blue to turn it on.
pub fn cyber_program(programs: &[String], enabled: bool) -> Result<Option<&'static str>, LlmError> {
    if programs.is_empty() {
        return Ok(None);
    }
    if enabled {
        if daybreak_available(programs) {
            Ok(Some("daybreak_blue"))
        } else {
            Err(LlmError::new(
                super::ErrorKind::Fatal,
                "OpenAI doesn't offer Daybreak Blue for this model. Choose another model or turn Daybreak Blue off in chat options.",
            ))
        }
    } else {
        Ok(programs
            .iter()
            .any(|p| p == "standard")
            .then_some("standard"))
    }
}

#[cfg(test)]
#[path = "responses_daybreak_tests.rs"]
mod daybreak_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ErrorKind, LlmEvent};

    pub(super) fn request(model: &str) -> LlmRequest {
        LlmRequest {
            model: model.into(),
            system: "s".into(),
            messages: vec![Msg::User(vec![ContentPart::text("hi")])],
            tools: vec![],
            thinking: ThinkingLevel::Auto,
            max_output: None,
            cache_key: "k".into(),
            fast: true,
            daybreak: false,
            no_tool_calls: false,
        }
    }

    #[test]
    fn images_reach_the_chatgpt_backend_as_the_codex_cli_sends_them() {
        let http = reqwest::Client::new();
        let ep = Endpoint {
            http: &http,
            base_url: "https://chatgpt.com/backend-api/codex",
            headers: &[],
            reasoning: true,
            vision: true,
            codex: true,
            fast: false,
            cyber: None,
            limits: None,
        };
        let mut req = request("gpt-5.5");
        req.messages = vec![
            Msg::User(vec![
                ContentPart::text("what is this?"),
                ContentPart::Image {
                    mime: "image/png".into(),
                    data: "iVBORw0KGgo=".into(),
                },
            ]),
            Msg::Assistant(vec![AssistantPart::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: json!({"path": "shot.png"}),
            }]),
            Msg::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                content: vec![
                    ContentPart::text("Image shot.png:"),
                    ContentPart::Image {
                        mime: "image/jpeg".into(),
                        data: "/9j/4AAQ".into(),
                    },
                ],
                is_error: false,
            },
        ];
        let body = build_body(&req, &ep);
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            input[0]["content"][1],
            json!({"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="})
        );
        // A tool's image follows its output as a user message.
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(
            input[3]["content"][1],
            json!({"type": "input_image", "image_url": "data:image/jpeg;base64,/9j/4AAQ"})
        );
        // A model without vision gets a note instead.
        let blind = build_body(
            &req,
            &Endpoint {
                vision: false,
                ..ep
            },
        );
        let text = blind.to_string();
        assert!(!text.contains("input_image") && text.contains("image omitted"));
    }

    #[test]
    fn codex_models_say_whether_they_take_images() {
        let list = parse_codex_models(&json!({"models": [
            {"slug": "gpt-5.5", "context_window": 272000},
            {"slug": "codex-text", "input_modalities": ["text"]},
            {"slug": "gpt-6", "input_modalities": ["text", "image"], "visibility": "list"},
            {"slug": "hidden", "visibility": "hide"}
        ]}));
        let vision: Vec<(&str, bool)> = list.iter().map(|m| (m.id.as_str(), m.vision)).collect();
        assert_eq!(
            vision,
            [("gpt-5.5", true), ("codex-text", false), ("gpt-6", true)]
        );
        assert_eq!(list[0].context, Some(272_000));
    }

    #[test]
    fn fast_mode_asks_for_priority_processing() {
        let http = reqwest::Client::new();
        let ep = |codex, fast| Endpoint {
            http: &http,
            base_url: "https://api.example.com/v1",
            headers: &[],
            reasoning: true,
            vision: true,
            codex,
            fast,
            cyber: None,
            limits: None,
        };
        let req = request("gpt-5.5");
        for codex in [false, true] {
            let on = build_body(&req, &ep(codex, true));
            assert_eq!(on["service_tier"], "priority");
            let off = build_body(&req, &ep(codex, false));
            assert!(off.get("service_tier").is_none(), "{off}");
            assert!(routing_hint(&req, &ep(codex, false)).is_none());
        }
        // The ChatGPT backend also routes by header, like Codex.
        assert_eq!(
            routing_hint(&req, &ep(true, true)),
            Some((
                "x-codex-routing-hint",
                "model=gpt-5.5;tier=priority".to_string()
            ))
        );
        assert!(routing_hint(&req, &ep(false, true)).is_none());
    }

    /// Serve one SSE body on a local port; the URL of its `/v1`.
    async fn serve(body: String) -> String {
        serve_with(200, Vec::new(), body).await
    }

    /// …with this status and these headers.
    async fn serve_with(
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: String,
    ) -> String {
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post(move || {
                let (body, headers) = (body.clone(), headers.clone());
                async move {
                    let mut resp = axum::response::IntoResponse::into_response(body);
                    *resp.status_mut() = axum::http::StatusCode::from_u16(status).unwrap();
                    let h = resp.headers_mut();
                    h.insert("content-type", "text/event-stream".parse().unwrap());
                    for (k, v) in headers {
                        h.insert(k, v.parse().unwrap());
                    }
                    resp
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}/v1")
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("data: {e}\n\n")).collect()
    }

    async fn run(events: &[Value]) -> (Result<LlmResponse, LlmError>, Vec<LlmEvent>) {
        let url = serve(sse(events)).await;
        let http = reqwest::Client::new();
        let ep = Endpoint {
            http: &http,
            base_url: &url,
            headers: &[],
            reasoning: true,
            vision: false,
            codex: true,
            fast: false,
            cyber: None,
            limits: None,
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let r = stream(&ep, &request("gpt-6"), &tx, &CancellationToken::new()).await;
        drop(tx);
        let mut seen = Vec::new();
        while let Some(e) = rx.recv().await {
            seen.push(e);
        }
        (r, seen)
    }

    #[tokio::test]
    async fn hidden_reasoning_shows_as_thinking_at_once_and_cut_streams_are_errors() {
        let start = [
            json!({"type": "response.output_item.added", "item": {"id": "rs_1", "type": "reasoning"}}),
            json!({"type": "response.output_item.added", "item": {"id": "msg_1", "type": "message"}}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "delta": "Hello"}),
        ];
        let done = json!({"type": "response.completed", "response": {"status":"completed", "usage": {"input_tokens": 5, "output_tokens": 2}}});
        let (r, seen) = run(&[start[0].clone(), start[1].clone(), start[2].clone(), done]).await;
        assert!(
            matches!(seen.first(), Some(LlmEvent::Thinking { text, .. }) if text.is_empty()),
            "the reasoning block is announced before any text"
        );
        assert_eq!(r.unwrap().stop, StopReason::Stop);
        // The same reply without its end: retried, never taken as the answer.
        let (r, _) = run(&start).await;
        let e = r.unwrap_err();
        assert_eq!(e.kind, ErrorKind::Transient);
        assert!(
            e.message.contains("before the reply finished"),
            "{}",
            e.message
        );
    }

    #[test]
    fn chatgpt_plan_limits_are_read_from_the_headers() {
        let now = 1_790_000_000_000;
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in [
            ("x-codex-primary-used-percent", "42.5"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1790003600"),
            ("x-codex-secondary-used-percent", "7"),
            ("x-codex-secondary-window-minutes", "10080"),
            ("x-codex-secondary-reset-after-seconds", "86400"),
            ("x-codex-routing-hint", "ignored"),
        ] {
            h.insert(k, v.parse().unwrap());
        }
        let l = codex_limits(&h, now);
        let got: Vec<(&str, &str, Option<f64>, Option<u64>)> = l
            .iter()
            .map(|l| {
                (
                    l.window.as_str(),
                    l.label.as_str(),
                    l.used_percent,
                    l.resets_at,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "five_hour",
                    "5-hour limit",
                    Some(42.5),
                    Some(1_790_003_600_000)
                ),
                ("weekly", "Weekly limit", Some(7.0), Some(now + 86_400_000)),
            ]
        );
        assert!(l.iter().all(|l| l.updated_at == now && l.status.is_none()));
        // Other lengths are named by theirs; a window without its length is
        // the usual one; nonsense is skipped.
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in [
            ("x-codex-primary-used-percent", "3"),
            ("x-codex-secondary-used-percent", "12"),
            ("x-codex-secondary-window-minutes", "1440"),
        ] {
            h.insert(k, v.parse().unwrap());
        }
        let l = codex_limits(&h, now);
        assert_eq!(
            l.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(),
            ["5-hour limit", "1-day limit"]
        );
        assert_eq!(l[0].resets_at, None);
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-codex-primary-used-percent", "lots".parse().unwrap());
        assert!(codex_limits(&h, now).is_empty());
        assert!(codex_limits(&reqwest::header::HeaderMap::new(), now).is_empty());
    }

    #[tokio::test]
    async fn chatgpt_plan_limits_come_with_replies_and_refusals() {
        let headers = vec![
            ("x-codex-primary-used-percent", "61"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-secondary-used-percent", "100"),
            ("x-codex-secondary-window-minutes", "10080"),
        ];
        let done = json!({"type": "response.completed", "response": {"status":"completed", "usage": {"input_tokens": 5, "output_tokens": 2}}});
        let ok = serve_with(200, headers.clone(), sse(&[done])).await;
        let refused = serve_with(
            429,
            headers,
            r#"{"error": {"type": "usage_limit_reached", "message": "The usage limit has been reached", "resets_in_seconds": 600}}"#.into(),
        )
        .await;
        let http = reqwest::Client::new();
        for (url, codex) in [(&ok, true), (&refused, true), (&ok, false)] {
            let seen = parking_lot::Mutex::new(Vec::new());
            let keep = |l: Vec<PlanLimit>| seen.lock().extend(l);
            let ep = Endpoint {
                http: &http,
                base_url: url,
                headers: &[],
                reasoning: true,
                vision: false,
                codex,
                fast: false,
                cyber: None,
                limits: codex.then_some(&keep as &(dyn Fn(Vec<PlanLimit>) + Sync)),
            };
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let r = stream(&ep, &request("gpt-6"), &tx, &CancellationToken::new()).await;
            assert_eq!(r.is_ok(), url == &ok, "{:?}", r.err());
            let seen: Vec<(String, Option<f64>)> = seen
                .into_inner()
                .into_iter()
                .map(|l| (l.window, l.used_percent))
                .collect();
            if codex {
                assert_eq!(
                    seen,
                    [
                        ("five_hour".to_string(), Some(61.0)),
                        ("weekly".to_string(), Some(100.0))
                    ]
                );
            } else {
                assert!(seen.is_empty());
            }
        }
    }

    #[test]
    fn tools_stay_listed_when_no_call_is_allowed() {
        let http = reqwest::Client::new();
        let ep = Endpoint {
            http: &http,
            base_url: "https://api.openai.com/v1",
            headers: &[],
            reasoning: true,
            vision: true,
            codex: false,
            fast: false,
            cyber: None,
            limits: None,
        };
        let mut req = request("gpt-5.5");
        req.tools = vec![crate::llm::ToolSpec {
            name: "read".into(),
            description: "d".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let open = build_body(&req, &ep);
        assert_eq!(open["tool_choice"], "auto");
        req.no_tool_calls = true;
        let quiet = build_body(&req, &ep);
        assert_eq!(quiet["tool_choice"], "none");
        assert_eq!(quiet["tools"], open["tools"]);
    }
}
