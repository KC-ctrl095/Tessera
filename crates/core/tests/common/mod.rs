//! Scripted OpenAI-compatible mock model server for deterministic tests.

#![allow(dead_code)]

pub mod skill_cases;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

#[derive(Clone, Debug)]
pub enum Reply {
    Text(String),
    Tools(Vec<(String, Value)>),
    /// Respond after a delay (for abort tests).
    Slow(Duration, String),
    /// Tool calls preceded by the given reasoning.
    ThinkTools(String, Vec<(String, Value)>),
    /// Some text, then the connection drops (no finish reason, no `[DONE]`).
    Cut(String),
    /// Send partial text and leave the stream open until the test stops it.
    Hang(String),
    /// First text is visible, terminal success waits for deterministic admission.
    Gate(String, Arc<tokio::sync::Notify>),
    /// These stream chunks as they are (servers' quirks).
    Raw(Vec<Value>),
}

#[derive(Default)]
pub struct MockState {
    pub script: Mutex<VecDeque<Reply>>,
    pub requests: Mutex<Vec<Value>>,
    /// (Authorization header, raw multipart body) per transcription.
    pub transcriptions: Mutex<Vec<(Option<String>, String)>>,
    /// (Authorization header, JSON body) per speech request.
    pub speeches: Mutex<Vec<(Option<String>, Value)>>,
    /// Write bloated checkpoints (to exercise condensing).
    pub big_summaries: std::sync::atomic::AtomicBool,
    /// Milliseconds each checkpoint takes to write (a slow summariser).
    pub summary_delay_ms: std::sync::atomic::AtomicU64,
    /// Checkpoint requests answered so far.
    pub summaries_done: std::sync::atomic::AtomicUsize,
    /// What the mock was asked, in order: "agent", "summary", or
    /// "summary done" when a checkpoint was answered.
    pub timeline: Mutex<Vec<&'static str>>,
    /// Replies for sessions whose first user message contains the key
    /// (consumed in order), so concurrent sessions get deterministic answers.
    pub routes: Mutex<Vec<(String, Reply)>>,
    /// Model ids `/v1/models` lists besides `m` (as llama.cpp would).
    pub models: Mutex<Vec<String>>,
}

pub struct Mock {
    pub url: String,
    pub state: Arc<MockState>,
}

impl Mock {
    pub async fn start(script: Vec<Reply>) -> Self {
        let state = Arc::new(MockState {
            script: Mutex::new(script.into()),
            ..Default::default()
        });
        let app = Router::new()
            .route("/v1/models", get(models))
            .route("/v1/chat/completions", post(chat))
            .route("/v1/audio/transcriptions", post(transcribe))
            .route("/v1/audio/speech", post(speech))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{addr}/v1"),
            state,
        }
    }

    pub fn push(&self, r: Reply) {
        self.state.script.lock().push_back(r);
    }

    /// Drop the replies nobody asked for (a turn that ended early).
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.state.script.lock().clear();
    }

    pub fn route(&self, key: &str, r: Reply) {
        self.state.routes.lock().push((key.to_string(), r));
    }

    /// Main-agent requests (excludes title and summary calls).
    pub fn agent_requests(&self) -> Vec<Value> {
        self.state
            .requests
            .lock()
            .iter()
            .filter(|r| !is_utility(r))
            .cloned()
            .collect()
    }

    pub fn summary_requests(&self) -> usize {
        self.state
            .requests
            .lock()
            .iter()
            .filter(|r| checkpoint_request(r))
            .count()
    }
}

async fn models(State(st): State<Arc<MockState>>) -> axum::Json<Value> {
    let data: Vec<Value> = std::iter::once("m".to_string())
        .chain(st.models.lock().iter().cloned())
        .map(|id| json!({"id": id, "context_length": 100000, "owned_by": "llamacpp"}))
        .collect();
    axum::Json(json!({ "data": data }))
}

fn system_of(req: &Value) -> String {
    req["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_utility(req: &Value) -> bool {
    let s = system_of(req);
    s.starts_with("You write context checkpoints")
        || s.starts_with("You name conversations")
        || in_place_checkpoint(req)
}

/// A checkpoint asked of the chat's own model as its next reply (the
/// request ends with the checkpoint request; see `context::checkpoint_request`).
pub fn in_place_checkpoint(req: &Value) -> bool {
    req["messages"]
        .as_array()
        .and_then(|m| m.last())
        .is_some_and(|m| {
            m["role"] == "user"
                && m["content"]
                    .to_string()
                    .contains("[Context checkpoint request]")
        })
}

/// Requests for a checkpoint, either way.
fn checkpoint_request(req: &Value) -> bool {
    system_of(req).starts_with("You write context checkpoints") || in_place_checkpoint(req)
}

fn sse(chunks: Vec<Value>) -> impl IntoResponse {
    let mut body = String::new();
    for c in chunks {
        body.push_str(&format!("data: {c}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], body)
}

async fn transcribe(
    State(st): State<Arc<MockState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    st.transcriptions
        .lock()
        .push((auth, String::from_utf8_lossy(&body).to_string()));
    axum::Json(json!({"text": " hello from the mock "}))
}

/// Speech as OpenAI's `/audio/speech` gives it (MP3 bytes; fake here).
async fn speech(
    State(st): State<Arc<MockState>>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> impl axum::response::IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    st.speeches.lock().push((auth, body));
    (
        [("content-type", "audio/mpeg")],
        b"ID3 pretend mp3".to_vec(),
    )
}

async fn chat(
    State(st): State<Arc<MockState>>,
    axum::Json(req): axum::Json<Value>,
) -> axum::response::Response {
    st.requests.lock().push(req.clone());
    let system = if in_place_checkpoint(&req) {
        // Answered as a checkpoint, whichever way it was asked.
        "You write context checkpoints".to_string()
    } else {
        system_of(&req)
    };
    // Report roughly what a real server would, so the engine's token
    // calibration (and every compaction decision) matches reality.
    let prompt_tokens = (req.to_string().len() / 4).max(1) as u64;
    let usage = json!({"prompt_tokens": prompt_tokens, "completion_tokens": 20});
    if system.starts_with("You name conversations") {
        return sse(vec![json!({"choices": [{"index": 0, "delta": {"content": "Test Title"}, "finish_reason": "stop"}], "usage": usage})]).into_response();
    }
    if system.starts_with("You write context checkpoints") {
        st.timeline.lock().push("summary");
        let delay = st
            .summary_delay_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        st.summaries_done
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        st.timeline.lock().push("summary done");
    } else if !is_utility(&req) {
        st.timeline.lock().push("agent");
    }
    if system.starts_with("You write context checkpoints")
        && req.to_string().contains("has grown too long")
    {
        let short = "## Goal\nBuild the widget.\n\n## Constraints & Preferences\n- Keep it small.\n\n## Completed Work\n1–40. Ran the build steps — ok\n\n## Current State\nIn progress.\n\n## Key Decisions\nNone\n\n## Errors & Fixes\nNone\n\n## Open Questions & Blockers\nNone\n\n## Critical Details\n- marker-XYZ\n- condensed-marker\n\n## Next Steps\n1. Continue.";
        return sse(vec![json!({"choices": [{"index": 0, "delta": {"content": short}, "finish_reason": "stop"}], "usage": usage})]).into_response();
    }
    if system.starts_with("You write context checkpoints")
        && st.big_summaries.load(std::sync::atomic::Ordering::Relaxed)
    {
        let lines: String = (1..=1200)
            .map(|i| format!("{i}. Ran build step {i} on src/module_{i}.rs — ok\n"))
            .collect();
        let big = format!(
            "## Goal\nBuild the widget.\n\n## Constraints & Preferences\n- Keep it small.\n\n## Completed Work\n{lines}\n## Current State\nIn progress.\n\n## Key Decisions\nNone\n\n## Errors & Fixes\nNone\n\n## Open Questions & Blockers\nNone\n\n## Critical Details\n- marker-XYZ\n\n## Next Steps\n1. Continue."
        );
        return sse(vec![json!({"choices": [{"index": 0, "delta": {"content": big}, "finish_reason": "stop"}], "usage": usage})]).into_response();
    }
    if system.starts_with("You write context checkpoints") {
        let summary = "## Goal\nBuild the widget as requested by the user, keeping everything in the widgets directory.\n\n## Constraints & Preferences\n- Keep it small.\n\n## Completed Work\n1. Ran several commands — ok\n\n## Current State\nWork in progress; commands ran successfully.\n\n## Key Decisions\nNone\n\n## Errors & Fixes\nNone\n\n## Open Questions & Blockers\nNone\n\n## Critical Details\n- marker-XYZ\n\n## Next Steps\n1. Continue.";
        return sse(vec![json!({"choices": [{"index": 0, "delta": {"content": summary}, "finish_reason": "stop"}], "usage": usage})]).into_response();
    }
    let first_user = req["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "user"))
        .map(|m| m["content"].to_string())
        .unwrap_or_default();
    let routed = {
        let mut routes = st.routes.lock();
        routes
            .iter()
            .position(|(k, _)| first_user.contains(k.as_str()))
            .map(|i| routes.remove(i).1)
    };
    let reply = routed.unwrap_or_else(|| {
        st.script
            .lock()
            .pop_front()
            .unwrap_or(Reply::Text("done".into()))
    });
    match reply {
        Reply::Text(t) => sse(vec![
            json!({"choices": [{"index": 0, "delta": {"reasoning_content": "thinking about it"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": t}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": usage}),
        ])
        .into_response(),
        Reply::Slow(d, t) => {
            tokio::time::sleep(d).await;
            sse(vec![json!({"choices": [{"index": 0, "delta": {"content": t}, "finish_reason": "stop"}], "usage": usage})]).into_response()
        }
        Reply::Cut(t) => {
            let body = format!(
                "data: {}\n\n",
                json!({"choices": [{"index": 0, "delta": {"content": t}}]})
            );
            ([("content-type", "text/event-stream")], body).into_response()
        }
        Reply::Hang(t) => {
            use futures::StreamExt;
            let body = format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":t}}]}));
            let chunks = futures::stream::once(async move { Ok::<_, std::io::Error>(body) })
                .chain(futures::stream::pending());
            ([ ("content-type", "text/event-stream") ], axum::body::Body::from_stream(chunks))
                .into_response()
        }
        Reply::Gate(t, release) => {
            use futures::StreamExt;
            let first = format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":t}}]}));
            let chunks = futures::stream::once(async move { Ok::<_,std::io::Error>(first) })
                .chain(futures::stream::once(async move {
                    release.notified().await;
                    Ok::<_,std::io::Error>(format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})))
                }));
            ([("content-type","text/event-stream")],axum::body::Body::from_stream(chunks)).into_response()
        }
        Reply::Tools(calls) => tool_reply("I should use tools", calls, prompt_tokens),
        Reply::ThinkTools(reasoning, calls) => tool_reply(&reasoning, calls, prompt_tokens),
        Reply::Raw(chunks) => sse(chunks).into_response(),
    }
}

fn tool_reply(
    reasoning: &str,
    calls: Vec<(String, Value)>,
    prompt_tokens: u64,
) -> axum::response::Response {
    let usage = json!({"prompt_tokens": prompt_tokens, "completion_tokens": 20});
    let mut chunks =
        vec![json!({"choices": [{"index": 0, "delta": {"reasoning_content": reasoning}}]})];
    for (i, (name, args)) in calls.iter().enumerate() {
        let args = args.to_string();
        let (a, b) = args.split_at(args.len() / 2);
        chunks.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": i, "id": format!("call_{}_{}", i, rand_suffix()), "type": "function", "function": {"name": name, "arguments": a}}]}}]}));
        chunks.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": i, "function": {"arguments": b}}]}}]}));
    }
    chunks.push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": usage}));
    sse(chunks).into_response()
}

fn rand_suffix() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    )
}

pub struct Harness {
    pub engine: Engine,
    pub mock: Mock,
    /// Tessera's home (config + data), hidden from sandboxed commands.
    pub dir: tempfile::TempDir,
    /// The project the agent works in, outside Tessera's home.
    pub work: std::path::PathBuf,
    pub _work_dir: tempfile::TempDir,
}

pub async fn harness(script: Vec<Reply>, context_window: u64) -> Harness {
    harness_agent(script, context_window, "").await
}

/// [`harness`] with more `[agent]` settings (lines such as `max_steps = 3`).
pub async fn harness_agent(script: Vec<Reply>, context_window: u64, agent: &str) -> Harness {
    let mock = Mock::start(script).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        r#"default_model = "mock/m"
[agent]
review_interval = 0
{agent}
# The mock plays a model on this computer: tests of small model mode and the
# tool check (`small_mode.rs`) set up their own.
[tuning."*"]
small = "off"
check = false
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{}"
discover = false
# Several requests at once, like a hosted API (tests of a server that takes
# one at a time set up their own).
parallel = 4
[[provider.models]]
id = "m"
context_window = {context_window}
max_output = 2000
reasoning = true
"#,
        mock.url
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Harness {
        engine,
        mock,
        dir,
        work,
        _work_dir: work_dir,
    }
}

impl Harness {
    pub async fn session(&self) -> SessionInfo {
        let v = self
            .engine
            .handle(
                Request::CreateSession {
                    cwd: Some(self.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    pub async fn send(&self, id: &str, text: &str) {
        self.engine
            .handle(
                Request::Send {
                    session_id: id.into(),
                    content: vec![ContentPart::text(text)],
                    delivery: Delivery::Auto,
                    attachments: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
    }

    /// Wait for the session to finish its current run.
    pub async fn settle(&self, id: &str) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::time::timeout(Duration::from_secs(20), self.engine.wait_idle(id))
            .await
            .expect("session did not become idle");
    }

    pub async fn entries(&self, id: &str) -> Vec<Entry> {
        let v = self
            .engine
            .handle(
                Request::GetSession {
                    session_id: id.into(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap();
        let snap: SessionSnapshot = serde_json::from_value(v).unwrap();
        snap.entries
    }

    pub async fn info(&self, id: &str) -> SessionInfo {
        let v = self
            .engine
            .handle(
                Request::GetSession {
                    session_id: id.into(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap();
        let snap: SessionSnapshot = serde_json::from_value(v).unwrap();
        snap.info
    }
}

pub fn tool_results(entries: &[Entry]) -> Vec<(String, bool, String)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                is_error,
                content,
                ..
            } => Some((tool.clone(), *is_error, plain_text(content))),
            _ => None,
        })
        .collect()
}
