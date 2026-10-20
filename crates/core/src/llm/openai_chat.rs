//! OpenAI Chat Completions wire format — llama.cpp, Ollama, LM Studio, vLLM,
//! OpenRouter, Groq, DeepSeek, Gemini's compatibility endpoint, and more.

use serde_json::{Value, json};
use tessera_proto::{AssistantPart, ContentPart, StopReason, ThinkingLevel, Usage};
use tokio_util::sync::CancellationToken;

use super::sse::SseReader;
use super::{
    Assembler, ErrorKind, EventTx, LlmError, LlmRequest, LlmResponse, Msg, ThinkTagSplitter,
    image_data_url,
};

pub struct Endpoint<'a> {
    pub http: &'a reqwest::Client,
    pub base_url: &'a str,
    pub headers: &'a [(String, String)],
    pub style: crate::thinking::ChatStyle,
    pub vision: bool,
    pub replay: crate::config::ReasoningReplay,
    /// What the model's profile adds (see `profiles.rs`).
    pub wire: &'a Wire,
}

/// What a request carries beside the conversation for a model's profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Wire {
    /// Sampling values, under the names this server takes.
    pub sampling: Vec<(&'static str, Value)>,
    /// One tool call per reply (`parallel_tool_calls: false`).
    pub single_calls: bool,
    /// Tool call ids as 9 letters and digits (Mistral's templates refuse others).
    pub mistral_ids: bool,
}

/// Which sampling fields a server takes beyond OpenAI's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// llama.cpp: `top_k`, `min_p`, `repeat_penalty`.
    Llama,
    /// vLLM, SGLang and OpenRouter: `top_k`, `min_p`, `repetition_penalty`.
    Extended,
    /// LM Studio: `top_k`, `repeat_penalty`.
    LmStudio,
    /// Anything else (Ollama's `/v1`, other hosted APIs): `temperature`,
    /// `top_p` and `presence_penalty` only, which every one takes.
    Standard,
}

/// `s` as request fields for a server of `dialect` (values it can't take
/// are left out rather than risk a refused request).
pub fn sampling_fields(
    s: &crate::profiles::Sampling,
    dialect: Dialect,
) -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = Vec::new();
    let mut put = |k: &'static str, v: Option<Value>| {
        if let Some(v) = v {
            out.push((k, v));
        }
    };
    put("temperature", s.temperature.map(Value::from));
    put("top_p", s.top_p.map(Value::from));
    put("presence_penalty", s.presence_penalty.map(Value::from));
    let (top_k, min_p, repeat) = match dialect {
        Dialect::Llama => (true, true, Some("repeat_penalty")),
        Dialect::Extended => (true, true, Some("repetition_penalty")),
        Dialect::LmStudio => (true, false, Some("repeat_penalty")),
        Dialect::Standard => (false, false, None),
    };
    if top_k {
        put("top_k", s.top_k.map(Value::from));
    }
    if min_p {
        put("min_p", s.min_p.map(Value::from));
    }
    if let Some(k) = repeat {
        put(k, s.repetition_penalty.map(Value::from));
    }
    out
}

/// Add what `w` asks to a request body built by [`build_body`].
pub fn apply_wire(body: &mut Value, w: &Wire) {
    for (k, v) in &w.sampling {
        body[*k] = v.clone();
    }
    if w.single_calls && body.get("parallel_tool_calls").is_some() {
        body["parallel_tool_calls"] = Value::Bool(false);
    }
    if w.mistral_ids
        && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
    {
        for m in messages {
            if let Some(Value::String(id)) = m.get_mut("tool_call_id") {
                *id = crate::profiles::mistral_id(id);
            }
            for call in m
                .get_mut("tool_calls")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if let Some(Value::String(id)) = call.get_mut("id") {
                    *id = crate::profiles::mistral_id(id);
                }
            }
        }
    }
}

pub async fn stream(
    ep: &Endpoint<'_>,
    req: &LlmRequest,
    tx: &EventTx,
    cancel: &CancellationToken,
) -> Result<LlmResponse, LlmError> {
    let mut body = build_body(req, ep.style, ep.vision, ep.replay);
    apply_wire(&mut body, ep.wire);
    if super::openrouter::is_openrouter(ep.base_url) {
        super::openrouter::add_caching(&mut body, &req.model, &req.cache_key);
    }
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));
    let mut rb = ep.http.post(&url).json(&body);
    for (k, v) in ep.headers {
        rb = rb.header(k, v);
    }
    let resp = tokio::select! {
        r = rb.send() => r.map_err(LlmError::network)?,
        _ = cancel.cancelled() => return Ok(aborted(Assembler::default())),
    };
    super::check_redirect(&resp)?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let ra = super::retry_after(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        return Err(LlmError::from_http(status, &text, ra));
    }

    let mut reader = SseReader::new(resp);
    let mut asm = Assembler::default();
    let mut splitter = ThinkTagSplitter::default();
    // Chat Completions identifies tool calls by `index`; map to assembler slots.
    let mut tool_slots: Vec<(u64, usize)> = Vec::new();
    // Calls by id, for servers that leave out `index`.
    let mut slot_ids: Vec<(String, u64)> = Vec::new();
    let mut usage = Usage::default();
    let mut finish: Option<String> = None;
    let mut invalid_finish = false;
    let mut done = false;

    loop {
        let ev = tokio::select! {
            ev = reader.next() => ev?,
            _ = cancel.cancelled() => { splitter.flush(&mut asm, tx); return Ok(aborted(asm)); }
        };
        let Some(ev) = ev else { break };
        let data = ev.data.trim();
        if data == "[DONE]" {
            done = true;
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(err) = chunk.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider error")
                .to_string();
            let status = err.get("code").and_then(Value::as_u64).unwrap_or(500) as u16;
            return Err(LlmError::from_http(
                status,
                &json!({"error": {"message": msg}}).to_string(),
                None,
            ));
        }
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            usage = parse_usage(u);
        }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
            continue;
        };
        if let Some(reason) = choice.get("finish_reason") {
            if let Some(fr) = reason.as_str() {
                finish = Some(fr.to_string());
            } else if !reason.is_null() {
                invalid_finish = true;
            }
        }
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = delta.get(key).and_then(Value::as_str) {
                asm.thinking(tx, r);
            }
        }
        if let Some(c) = delta.get("content").and_then(Value::as_str) {
            splitter.feed(&mut asm, tx, c);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            splitter.flush(&mut asm, tx);
            for (pos, call) in calls.iter().enumerate() {
                let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                let idx = match call.get("index").and_then(Value::as_u64) {
                    Some(i) => i,
                    // No index (some servers send each call in its own
                    // chunk): a new id starts a new call, pieces without one
                    // continue the latest.
                    None if !id.is_empty() => match slot_ids.iter().find(|(s, _)| s == id) {
                        Some((_, i)) => *i,
                        None => NO_INDEX + slot_ids.len() as u64,
                    },
                    None => tool_slots.last().map_or(pos as u64, |(i, _)| *i),
                };
                let f = call.get("function");
                let slot = match tool_slots.iter().find(|(i, _)| *i == idx) {
                    Some((_, s)) => *s,
                    None => {
                        let name = f
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let s = asm.tool_start(tx, id, name);
                        tool_slots.push((idx, s));
                        if !id.is_empty() {
                            slot_ids.push((id.to_string(), idx));
                        }
                        s
                    }
                };
                if let Some(args) = f.and_then(|f| f.get("arguments")).and_then(Value::as_str) {
                    asm.tool_args(tx, slot, args);
                }
            }
        }
    }
    // Servers end with a finish reason, `[DONE]`, or both; neither means
    // the connection dropped mid-reply.
    if !done && finish.is_none() {
        return Err(LlmError::cut_off());
    }
    splitter.flush(&mut asm, tx);
    let has_tools = asm.has_tool_calls();
    let allow_tool_repair = !invalid_finish
        && matches!(
            finish.as_deref(),
            Some("stop" | "tool_calls" | "function_call")
        );
    let stop = match finish.as_deref().filter(|_| !invalid_finish) {
        Some("length") => StopReason::Length,
        // Only known complete stops authorize repair. A DONE-only stream
        // still returns its text, but is not proof that tool arguments ended.
        Some("stop" | "tool_calls" | "function_call") => {
            if has_tools {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        }
        Some(_) => StopReason::Error,
        None if !invalid_finish => {
            if has_tools {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        }
        None => StopReason::Error,
    };
    Ok(LlmResponse {
        content: asm.finish(),
        usage,
        stop,
        allow_tool_repair,
    })
}

fn aborted(asm: Assembler) -> LlmResponse {
    LlmResponse {
        content: asm.finish(),
        usage: Usage::default(),
        stop: StopReason::Aborted,
        allow_tool_repair: false,
    }
}

pub(super) fn parse_usage(u: &Value) -> Usage {
    let prompt = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
    let detail = |k: &str| {
        u.get("prompt_tokens_details")
            .and_then(|d| d.get(k))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    // `prompt_tokens` counts all of them; OpenRouter also says what was
    // written to the cache (billed above the input price for Claude).
    let (cached, written) = (detail("cached_tokens"), detail("cache_write_tokens"));
    Usage {
        input: prompt.saturating_sub(cached).saturating_sub(written),
        output: u
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read: cached,
        cache_write: written,
        cost_usd: None,
    }
}

pub fn build_body(
    req: &LlmRequest,
    style: crate::thinking::ChatStyle,
    vision: bool,
    replay: crate::config::ReasoningReplay,
) -> Value {
    let mut messages = vec![json!({"role": "system", "content": req.system})];
    // Replaying all reasoning keeps the prompt append-only, so llama.cpp's KV
    // cache and provider prompt caches keep hitting (chat templates that drop
    // old reasoning do so themselves). `turn` limits it to the current tool loop.
    let last_user = req
        .messages
        .iter()
        .rposition(|m| matches!(m, Msg::User(_)))
        .unwrap_or(0);
    let mut pending_images: Vec<Value> = Vec::new();

    for (i, msg) in req.messages.iter().enumerate() {
        if !matches!(msg, Msg::ToolResult { .. }) && !pending_images.is_empty() {
            let mut content =
                vec![json!({"type": "text", "text": "Images returned by the tool calls above:"})];
            content.append(&mut pending_images);
            messages.push(json!({"role": "user", "content": content}));
        }
        match msg {
            Msg::User(parts) => {
                messages.push(json!({"role": "user", "content": user_content(parts, vision)}))
            }
            Msg::Assistant(parts) => {
                let mut text = String::new();
                let mut thinking = String::new();
                let mut calls = Vec::new();
                for p in parts {
                    match p {
                        AssistantPart::Text { text: t } => text.push_str(t),
                        AssistantPart::Thinking { text: t, .. } => thinking.push_str(t),
                        AssistantPart::ToolCall {
                            id,
                            name,
                            arguments,
                        } => calls.push(json!({
                            "id": id, "type": "function",
                            "function": {"name": name, "arguments": tool_args_string(arguments)}
                        })),
                    }
                }
                let mut m = json!({"role": "assistant"});
                m["content"] = if text.is_empty() && !calls.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                };
                if !calls.is_empty() {
                    m["tool_calls"] = Value::Array(calls);
                }
                let replay_this = match replay {
                    crate::config::ReasoningReplay::All => true,
                    crate::config::ReasoningReplay::Turn => i > last_user,
                    crate::config::ReasoningReplay::None => false,
                };
                if replay_this && !thinking.is_empty() {
                    m["reasoning_content"] = Value::String(thinking);
                }
                messages.push(m);
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
                if text.is_empty() {
                    text = "(no output)".into();
                }
                for p in content {
                    if let ContentPart::Image { mime, data } = p
                        && vision
                    {
                        pending_images.push(json!({"type": "image_url", "image_url": {"url": image_data_url(mime, data)}}));
                    }
                }
                messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": text}));
            }
        }
    }
    if !pending_images.is_empty() {
        let mut content =
            vec![json!({"type": "text", "text": "Images returned by the tool calls above:"})];
        content.append(&mut pending_images);
        messages.push(json!({"role": "user", "content": content}));
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
            .collect();
        body["parallel_tool_calls"] = Value::Bool(true);
        if req.no_tool_calls {
            body["tool_choice"] = json!("none");
        }
    }
    if let Some(max) = req.max_output {
        body["max_tokens"] = json!(max);
    }
    apply_thinking(&mut body, req.thinking, style);
    body
}

/// Reasoning settings in the dialect the endpoint understands.
fn apply_thinking(body: &mut Value, level: ThinkingLevel, style: crate::thinking::ChatStyle) {
    use crate::thinking::{ChatStyle, effort_word};
    if level == ThinkingLevel::Auto {
        return;
    }
    match style {
        ChatStyle::None => {}
        ChatStyle::Template { toggle, efforts } => {
            if level == ThinkingLevel::Off {
                if toggle {
                    body["chat_template_kwargs"] = json!({"enable_thinking": false});
                }
                return;
            }
            if toggle {
                body["chat_template_kwargs"] = json!({"enable_thinking": true});
            }
            if efforts {
                body["reasoning_effort"] = json!(effort_word(level));
            }
        }
        ChatStyle::OpenRouter => {
            body["reasoning"] = if level == ThinkingLevel::Off {
                json!({"enabled": false})
            } else {
                let e = match level {
                    ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
                    ThinkingLevel::Medium => "medium",
                    _ => "high",
                };
                json!({"effort": e})
            };
        }
        ChatStyle::Effort => body["reasoning_effort"] = json!(effort_word(level)),
    }
}

fn user_content(parts: &[ContentPart], vision: bool) -> Value {
    if parts.iter().all(|p| matches!(p, ContentPart::Text { .. })) {
        return Value::String(tessera_proto::plain_text(parts));
    }
    Value::Array(
        parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => json!({"type": "text", "text": text}),
                ContentPart::Image { mime, data } if vision => json!({"type": "image_url", "image_url": {"url": image_data_url(mime, data)}}),
                ContentPart::Image { .. } => json!({"type": "text", "text": "[image omitted: this model does not accept images]"}),
            })
            .collect(),
    )
}

/// A call's arguments as sent back in the history. Broken ones go back as
/// `{}` (as Anthropic's adapter does): a chat template that walks them as a
/// map would fail on the raw text, and the tool result already quotes it.
/// Slots for calls that came without an `index` (past any real one).
const NO_INDEX: u64 = 1 << 32;

pub fn tool_args_string(args: &Value) -> String {
    if args.get("__invalid_arguments").is_some() {
        return "{}".into();
    }
    args.to_string()
}

/// One model in `GET {base}/models`.
#[derive(Debug, Clone, Default)]
pub struct Listed {
    pub id: String,
    /// Context window, when the server reports it: what the list says, else
    /// the router's `--ctx-size`, else the model's trained length (which is
    /// not what a server runs it with).
    pub context: Option<u64>,
    /// llama.cpp router: the `--ctx-size` the model is launched with.
    pub launch_ctx: Option<u64>,
    /// In memory, so its `/props` can be read. False only when the server
    /// says otherwise (llama.cpp router mode): a router asked about a model
    /// it hasn't loaded loads it, evicting the active one.
    pub loaded: bool,
    /// llama.cpp serves it (its `/props` holds the chat template).
    pub llama: bool,
    /// Served by a server made for many requests at once (vLLM, SGLang).
    pub batching: bool,
    /// What the server says owns it (`llamacpp`, `vllm`, `library` for
    /// Ollama, `organization_owner` for LM Studio…).
    pub owner: String,
    /// llama.cpp router: the arguments the model runs with.
    pub args: Vec<String>,
}

impl Listed {
    /// The value after `flag` in the router's launch arguments.
    pub fn arg(&self, flags: &[&str]) -> Option<&str> {
        let i = self.args.iter().position(|a| flags.contains(&a.as_str()))?;
        self.args.get(i + 1).map(String::as_str)
    }

    /// The server's own reasoning default: `--reasoning-effort`, or `none`
    /// when it starts with thinking off.
    pub fn server_effort(&self) -> Option<&str> {
        if self
            .arg(&["-rea", "--reasoning"])
            .is_some_and(|v| matches!(v, "off" | "false" | "0"))
        {
            return Some("none");
        }
        self.arg(&["--reasoning-effort"])
            .filter(|e| *e != "default" && *e != "auto")
    }
}

/// `GET {base}/models` → (id, context window, loaded); see [`list_entries`].
pub async fn list_models(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
) -> Result<Vec<(String, Option<u64>, bool)>, LlmError> {
    Ok(list_entries(http, base_url, headers)
        .await?
        .into_iter()
        .map(|m| (m.id, m.context, m.loaded))
        .collect())
}

/// `GET {base}/models`, with what llama.cpp says about each model.
pub async fn list_entries(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
) -> Result<Vec<Listed>, LlmError> {
    let mut rb = http
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(8));
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    // No answer at all within the time (a computer asleep on the network)
    // counts as off, like a refused connection.
    let resp = rb.send().await.map_err(|e| {
        let silent = e.is_timeout();
        let mut err = LlmError::network(e);
        err.unreachable |= silent;
        err
    })?;
    super::check_redirect(&resp)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(LlmError::network)?;
    if !(200..300).contains(&status) {
        return Err(LlmError::from_http(status, &text, None));
    }
    let v: Value =
        serde_json::from_str(&text).map_err(|e| LlmError::new(ErrorKind::Fatal, e.to_string()))?;
    let list = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("name"))
                .and_then(Value::as_str)?
                .to_string();
            let launch_ctx = llama_router_ctx(m);
            let ctx = [
                "context_length",
                "context_window",
                "max_context_length",
                "max_model_len",
            ]
            .iter()
            .find_map(|k| m.get(*k).and_then(Value::as_u64))
            .or(launch_ctx)
            .or_else(|| {
                m.get("meta")
                    .and_then(|x| x.get("n_ctx_train"))
                    .and_then(Value::as_u64)
            });
            let status = m.get("status");
            // A router's explicit status must be loaded. Even though props
            // can read a sleeping child without waking it, leave it alone.
            let loaded = status.is_none()
                || status.and_then(|s| s.get("value")).and_then(Value::as_str) == Some("loaded");
            let args = status
                .and_then(|s| s.get("args"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let owner = m.get("owned_by").and_then(Value::as_str);
            Some(Listed {
                id,
                context: ctx,
                launch_ctx,
                loaded,
                llama: owner == Some("llamacpp"),
                batching: matches!(owner, Some("vllm" | "sglang")),
                owner: owner.unwrap_or_default().to_string(),
                args,
            })
        })
        .collect())
}

/// llama.cpp router mode lists launch args; pull `--ctx-size` when present and non-zero.
fn llama_router_ctx(m: &Value) -> Option<u64> {
    let args = m.get("status")?.get("args")?.as_array()?;
    let pos = args
        .iter()
        .position(|a| a.as_str() == Some("--ctx-size") || a.as_str() == Some("-c"))?;
    args.get(pos + 1)?
        .as_str()?
        .parse()
        .ok()
        .filter(|n: &u64| *n > 0)
}

/// llama.cpp's `/props` (root, not /v1): the loaded context size and the
/// chat template's thinking controls. Refresh the exact model's status just
/// before every targeted read (including probes and detached window reads).
/// `autoload=false` also closes the race after that check: upstream router
/// `server-models.cpp` validates it before GET /props is proxied, and only
/// calls ensure_model_ready when autoload is true. See docs/PROTOCOL.md.
pub async fn llama_props(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Option<(Option<u64>, Option<crate::thinking::TemplateCaps>)> {
    let fresh = list_entries(http, base_url, &[]).await.ok()?;
    if !fresh.iter().any(|m| m.id == model && m.loaded) {
        return None;
    }
    let root = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let resp = http
        .get(format!("{root}/props"))
        .query(&[("model", model), ("autoload", "false")])
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    super::check_redirect(&resp).ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    let ctx = v
        .get("default_generation_settings")
        .and_then(|d| d.get("n_ctx"))
        .and_then(Value::as_u64)
        .filter(|n| *n > 0);
    let caps = v
        .get("chat_template")
        .and_then(Value::as_str)
        .map(crate::thinking::parse_template);
    Some((ctx, caps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolSpec;

    #[test]
    fn body_shapes() {
        let req = LlmRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![
                Msg::User(vec![ContentPart::text("hi")]),
                Msg::Assistant(vec![
                    AssistantPart::Thinking {
                        text: "hmm".into(),
                        opaque: None,
                    },
                    AssistantPart::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: json!({"path": "a"}),
                    },
                ]),
                Msg::ToolResult {
                    call_id: "c1".into(),
                    name: "read".into(),
                    content: vec![
                        ContentPart::text("data"),
                        ContentPart::Image {
                            mime: "image/png".into(),
                            data: "AAA".into(),
                        },
                    ],
                    is_error: false,
                },
            ],
            tools: vec![ToolSpec {
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
        let b = build_body(
            &req,
            crate::thinking::ChatStyle::Template {
                toggle: true,
                efforts: true,
            },
            true,
            crate::config::ReasoningReplay::All,
        );
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[2]["content"], Value::Null);
        assert_eq!(msgs[2]["reasoning_content"], "hmm");
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"a"}"#
        );
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[4]["role"], "user");
        assert_eq!(b["reasoning_effort"], "high");
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], true);
        let mut off = req.clone();
        off.thinking = ThinkingLevel::Off;
        let b = build_body(
            &off,
            crate::thinking::ChatStyle::Template {
                toggle: true,
                efforts: true,
            },
            true,
            crate::config::ReasoningReplay::All,
        );
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert!(b.get("reasoning_effort").is_none());
        let mut auto = req.clone();
        auto.thinking = ThinkingLevel::Auto;
        let b = build_body(
            &auto,
            crate::thinking::ChatStyle::OpenRouter,
            true,
            crate::config::ReasoningReplay::All,
        );
        assert!(
            b.get("reasoning").is_none(),
            "auto leaves the server default"
        );
        let mut xhigh = req.clone();
        xhigh.thinking = ThinkingLevel::XHigh;
        let b = build_body(
            &xhigh,
            crate::thinking::ChatStyle::Template {
                toggle: true,
                efforts: true,
            },
            true,
            crate::config::ReasoningReplay::All,
        );
        assert_eq!(b["reasoning_effort"], "xhigh");
    }

    #[test]
    fn router_launch_args() {
        let l = |args: &[&str]| Listed {
            args: args.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        };
        let m = l(&[
            "--jinja",
            "--reasoning-effort",
            "high",
            "--model",
            "/m/q.gguf",
        ]);
        assert_eq!(m.server_effort(), Some("high"));
        assert_eq!(m.arg(&["-m", "--model"]), Some("/m/q.gguf"));
        assert_eq!(l(&["--reasoning", "off"]).server_effort(), Some("none"));
        assert_eq!(l(&["--reasoning-effort", "default"]).server_effort(), None);
        assert_eq!(l(&[]).server_effort(), None);
    }

    /// A profile's additions: sampling under each server's names (only what
    /// it takes), one call per reply, Mistral's 9-character call ids.
    #[test]
    fn profile_additions_per_server() {
        let s = crate::profiles::Sampling {
            temperature: Some(0.7),
            top_p: Some(0.8),
            top_k: Some(20),
            min_p: Some(0.0),
            presence_penalty: Some(1.5),
            repetition_penalty: Some(1.05),
        };
        let names = |d| {
            sampling_fields(&s, d)
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Dialect::Llama),
            [
                "temperature",
                "top_p",
                "presence_penalty",
                "top_k",
                "min_p",
                "repeat_penalty"
            ]
        );
        assert_eq!(
            names(Dialect::Extended),
            [
                "temperature",
                "top_p",
                "presence_penalty",
                "top_k",
                "min_p",
                "repetition_penalty"
            ]
        );
        assert_eq!(
            names(Dialect::LmStudio),
            [
                "temperature",
                "top_p",
                "presence_penalty",
                "top_k",
                "repeat_penalty"
            ]
        );
        assert_eq!(
            names(Dialect::Standard),
            ["temperature", "top_p", "presence_penalty"]
        );
        let req = LlmRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![
                Msg::User(vec![ContentPart::text("hi")]),
                Msg::Assistant(vec![AssistantPart::ToolCall {
                    id: "call_0_1a2b".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a"}),
                }]),
                Msg::ToolResult {
                    call_id: "call_0_1a2b".into(),
                    name: "read".into(),
                    content: vec![ContentPart::text("data")],
                    is_error: false,
                },
            ],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            thinking: ThinkingLevel::Auto,
            max_output: None,
            cache_key: "k".into(),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let style = crate::thinking::ChatStyle::None;
        let replay = crate::config::ReasoningReplay::All;
        let plain = build_body(&req, style, false, replay);
        let mut body = plain.clone();
        apply_wire(&mut body, &Wire::default());
        assert_eq!(body, plain, "nothing added by default");
        assert_eq!(body["parallel_tool_calls"], true);
        apply_wire(
            &mut body,
            &Wire {
                sampling: sampling_fields(&s, Dialect::Llama),
                single_calls: true,
                mistral_ids: true,
            },
        );
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["top_k"], 20);
        assert_eq!(body["repeat_penalty"], 1.05);
        assert_eq!(body["parallel_tool_calls"], false);
        let id = body["messages"][2]["tool_calls"][0]["id"].as_str().unwrap();
        assert_eq!(id.len(), 9);
        assert!(id.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_eq!(body["messages"][3]["tool_call_id"], id);
        // No tools: no `parallel_tool_calls` to turn off.
        let mut bare = req.clone();
        bare.tools.clear();
        let mut body = build_body(&bare, style, false, replay);
        apply_wire(
            &mut body,
            &Wire {
                single_calls: true,
                ..Default::default()
            },
        );
        assert!(body.get("parallel_tool_calls").is_none());
    }

    /// No calls allowed: the same tools, `tool_choice: none` (llama.cpp
    /// then renders the same prompt and keeps its cache).
    #[test]
    fn tools_stay_listed_when_no_call_is_allowed() {
        let mut req = LlmRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![Msg::User(vec![ContentPart::text("hi")])],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            thinking: ThinkingLevel::Auto,
            max_output: None,
            cache_key: "k".into(),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let body = |r: &LlmRequest| {
            build_body(
                r,
                crate::thinking::ChatStyle::None,
                false,
                crate::config::ReasoningReplay::All,
            )
        };
        let open = body(&req);
        assert!(open.get("tool_choice").is_none());
        req.no_tool_calls = true;
        let quiet = body(&req);
        assert_eq!(quiet["tool_choice"], "none");
        assert_eq!(quiet["tools"], open["tools"]);
        assert_eq!(quiet["messages"], open["messages"]);
    }
}
