//! Thinking modes of llama.cpp models served by a router that holds one
//! model at a time: the model in memory is read from its chat template, a
//! model that isn't keeps what was read the last time it was loaded (or a
//! marked guess), and nothing Tessera does on its own makes the router load
//! a model. Also: the level the user last picked per model.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

const QWEN: &str = "qwen3.8-27b";
const UNCENSORED: &str = "qwen3.8-27b-uncensored";
const OSS: &str = "gpt-oss-20b";
/// A hybrid-thinking model nothing else is known about.
const HYBRID: &str = "qwen3-30b-a3b";

/// `/props` of Qwen 3.8 27B as llama.cpp serves it.
fn qwen_props() -> Value {
    serde_json::from_str(include_str!("fixtures/llama-props-qwen3.8-27b.json")).unwrap()
}

/// A llama.cpp router (one slot): `/v1/models` with each model's status and
/// launch arguments, `/props?model=` and completions. Like the real one, a
/// request for a model that isn't loaded loads it (unless `autoload=false`),
/// evicting the loaded one.
#[derive(Default)]
struct Router1 {
    status: Mutex<HashMap<String, &'static str>>,
    /// `/props` requests: (model, autoload flag, loaded at the time).
    props: Mutex<Vec<(String, Option<String>, bool)>>,
    /// Models loaded because of a request (completions or `/props`).
    loads: Mutex<Vec<String>>,
    chats: Mutex<Vec<Value>>,
}

impl Router1 {
    fn set(&self, model: &str, status: &'static str) {
        self.status.lock().insert(model.into(), status);
    }

    fn loaded(&self, model: &str) -> bool {
        matches!(
            self.status.lock().get(model).copied(),
            Some("loaded" | "sleeping")
        )
    }

    fn load(&self, model: &str) {
        let mut s = self.status.lock();
        for v in s.values_mut() {
            *v = "unloaded";
        }
        s.insert(model.into(), "loaded");
        self.loads.lock().push(model.into());
    }

    fn props_for(&self, model: &str) -> Vec<(String, Option<String>, bool)> {
        self.props
            .lock()
            .iter()
            .filter(|p| p.0 == model)
            .cloned()
            .collect()
    }
}

async fn start_router(r: Arc<Router1>) -> String {
    let app = Router::new()
        .route(
            "/v1/models",
            get(|State(r): State<Arc<Router1>>| async move {
                let status = r.status.lock().clone();
                let mut ids: Vec<&String> = status.keys().collect();
                ids.sort();
                let data: Vec<Value> = ids
                    .into_iter()
                    .map(|id| {
                        let effort = if id == UNCENSORED { "xhigh" } else { "high" };
                        json!({
                            "id": id, "object": "model", "owned_by": "llamacpp",
                            "status": {
                                "value": status[id],
                                "args": ["llama-server", "--jinja", "--reasoning-effort", effort,
                                         "--reasoning-format", "deepseek", "--ctx-size", "262144",
                                         "--model", format!("/models/{id}.gguf")],
                            },
                        })
                    })
                    .collect();
                axum::Json(json!({"object": "list", "data": data}))
            }),
        )
        .route(
            "/props",
            get(
                |State(r): State<Arc<Router1>>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    let model = q.get("model").cloned().unwrap_or_default();
                    let autoload = q.get("autoload").cloned();
                    let loaded = r.loaded(&model);
                    r.props.lock().push((model.clone(), autoload.clone(), loaded));
                    if !loaded {
                        if autoload.as_deref() == Some("false") {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({"error": {"message": "model is not loaded"}})),
                            )
                                .into_response();
                        }
                        r.load(&model);
                    }
                    if model.starts_with("qwen") {
                        axum::Json(qwen_props()).into_response()
                    } else {
                        // gpt-oss: a template without the controls Tessera reads.
                        axum::Json(json!({"chat_template": "{{ messages }}", "default_generation_settings": {"n_ctx": 131072}}))
                            .into_response()
                    }
                },
            ),
        )
        .route(
            "/v1/chat/completions",
            post(
                |State(r): State<Arc<Router1>>, axum::Json(req): axum::Json<Value>| async move {
                    let model = req["model"].as_str().unwrap_or_default().to_string();
                    if !r.loaded(&model) {
                        r.load(&model);
                    }
                    r.chats.lock().push(req);
                    let mut body = String::new();
                    for c in [
                        json!({"choices": [{"index": 0, "delta": {"content": "Hi."}}]}),
                        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 10, "completion_tokens": 2}}),
                    ] {
                        body.push_str(&format!("data: {c}\n\n"));
                    }
                    body.push_str("data: [DONE]\n\n");
                    ([("content-type", "text/event-stream")], body)
                },
            ),
        )
        .with_state(r);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

async fn engine(dir: &std::path::Path, url: &str) -> Engine {
    let config = format!(
        "default_model = \"llama/{QWEN}\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\ncheck = false\n\n[[provider]]\nid = \"llama\"\nname = \"llama.cpp\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = true\n"
    );
    std::fs::write(dir.join("config.toml"), config).unwrap();
    Engine::open(Paths::at(dir), EngineOptions { scheduler: false })
        .await
        .unwrap()
}

async fn call(e: &Engine, req: Request) -> Value {
    e.handle(req, None).await.unwrap()
}

/// The llama.cpp models' modes, after a fresh look at the router.
async fn modes(e: &Engine) -> HashMap<String, ModelInfo> {
    let v = call(e, Request::ListProviders { refresh: true }).await;
    let providers: Vec<ProviderInfo> = serde_json::from_value(v).unwrap();
    providers
        .into_iter()
        .filter(|p| p.id == "llama")
        .flat_map(|p| p.models)
        .map(|m| (m.id.clone(), m))
        .collect()
}

fn levels(m: &ModelInfo) -> Vec<ThinkingLevel> {
    m.thinking.iter().map(|o| o.level).collect()
}

fn auto_hint(m: &ModelInfo) -> &str {
    &m.thinking[0].hint
}

use ThinkingLevel::*;
const TEMPLATE: [ThinkingLevel; 5] = [Auto, Off, Low, Medium, XHigh];

/// Nothing asked about a model that wasn't in memory, and nothing loaded.
fn nothing_loaded(r: &Router1) {
    let props = r.props.lock().clone();
    assert!(
        props
            .iter()
            .all(|(_, autoload, loaded)| *loaded && autoload.as_deref() == Some("false")),
        "every /props request was for a loaded model, with autoload=false: {props:?}"
    );
    assert!(r.loads.lock().is_empty(), "loaded: {:?}", r.loads.lock());
}

#[tokio::test]
async fn an_unloaded_model_keeps_the_modes_read_while_it_was_loaded() {
    let r = Arc::new(Router1::default());
    r.set(QWEN, "loaded");
    r.set(UNCENSORED, "unloaded");
    let url = start_router(r.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), &url).await;

    let m = modes(&e).await;
    assert_eq!(levels(&m[QWEN]), TEMPLATE);
    let labels: Vec<&str> = m[QWEN].thinking.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Server default", "Off", "Low", "Medium", "Extra high"]
    );
    // `--reasoning-effort high`, which this template reads as xhigh.
    assert!(
        auto_hint(&m[QWEN]).ends_with("(Extra high)"),
        "{}",
        auto_hint(&m[QWEN])
    );
    assert_eq!(m[QWEN].default_thinking, Auto);
    // Never loaded yet: the template of the model it was made from, as a
    // guess that says so (with the server's own `--reasoning-effort xhigh`).
    assert_eq!(levels(&m[UNCENSORED]), TEMPLATE);
    assert!(
        auto_hint(&m[UNCENSORED])
            .contains("(Extra high); the exact modes show once this model has run")
    );
    assert!(r.props_for(UNCENSORED).is_empty());

    // The user switches models elsewhere: the uncensored one is in memory now.
    r.set(QWEN, "unloaded");
    r.set(UNCENSORED, "loaded");
    let before = r.props_for(QWEN).len();
    let m = modes(&e).await;
    assert_eq!(levels(&m[UNCENSORED]), TEMPLATE);
    assert!(
        auto_hint(&m[UNCENSORED]).ends_with("(Extra high)"),
        "its own now"
    );
    assert_eq!(levels(&m[QWEN]), TEMPLATE, "remembered while asleep");
    assert_eq!(r.props_for(QWEN).len(), before, "not asked while unloaded");

    // A restart with both asleep: both still known, from the data dir.
    drop(e);
    r.set(UNCENSORED, "unloaded");
    let e = engine(dir.path(), &url).await;
    let asked = r.props.lock().len();
    let m = modes(&e).await;
    assert_eq!(levels(&m[QWEN]), TEMPLATE);
    assert_eq!(levels(&m[UNCENSORED]), TEMPLATE);
    assert_eq!(
        r.props.lock().len(),
        asked,
        "nothing asked: nothing is loaded"
    );
    nothing_loaded(&r);
    drop(e);
}

#[tokio::test]
async fn first_use_reads_the_template_and_requests_send_what_it_takes() {
    let r = Arc::new(Router1::default());
    r.set(QWEN, "loaded");
    r.set(HYBRID, "unloaded");
    let url = start_router(r.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), &url).await;
    let mut events = e.subscribe();
    // Nothing known: the family's on/off, marked as a guess.
    let m = modes(&e).await;
    assert_eq!(levels(&m[HYBRID]), [Auto, Off, High]);
    assert!(auto_hint(&m[HYBRID]).contains("once this model has run"));
    nothing_loaded(&r);

    // The user picks it and "On": the router loads it for the reply (the
    // user's doing), then Tessera reads its template.
    let chat = |model: &str, thinking: ThinkingLevel, text: &str| {
        let (e, model, text) = (&e, model.to_string(), text.to_string());
        async move {
            let s: SessionInfo = serde_json::from_value(
                call(
                    e,
                    Request::CreateSession {
                        cwd: None,
                        model: Some(ModelRef {
                            provider: "llama".into(),
                            model,
                        }),
                        title: Some("t".into()),
                        bot_id: None,
                        private: false,
                        lean: false,
                    },
                )
                .await,
            )
            .unwrap();
            call(e, update(&s.id, None, Some(thinking))).await;
            call(
                e,
                Request::Send {
                    session_id: s.id.clone(),
                    content: vec![ContentPart::text(text)],
                    delivery: Delivery::Auto,
                    attachments: Vec::new(),
                },
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::time::timeout(Duration::from_secs(20), e.wait_idle(&s.id))
                .await
                .unwrap();
        }
    };
    chat(HYBRID, High, "hello").await;
    let body = r.chats.lock().last().cloned().unwrap();
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    assert!(
        body.get("reasoning_effort").is_none(),
        "unknown efforts aren't guessed"
    );
    let p = r.props_for(HYBRID);
    assert!(
        p.iter()
            .all(|(_, a, loaded)| *loaded && a.as_deref() == Some("false")),
        "{p:?}"
    );
    assert!(!p.is_empty(), "read after the reply");
    let mut changed = false;
    while let Ok(ev) = events.try_recv() {
        changed |= matches!(ev, Event::ProvidersChanged);
    }
    assert!(changed, "clients hear about the new modes");
    let v = call(&e, Request::ListProviders { refresh: false }).await;
    let providers: Vec<ProviderInfo> = serde_json::from_value(v).unwrap();
    let m = providers
        .iter()
        .flat_map(|p| &p.models)
        .find(|m| m.id == HYBRID)
        .unwrap();
    assert_eq!(levels(m), TEMPLATE);
    assert!(!auto_hint(m).contains("once"), "no longer a guess");

    // Each mode as the template wants it (qwen3.8-27b is loaded again by the
    // router for its own turns, as the user asked).
    chat(QWEN, Off, "one").await;
    let b = r.chats.lock().last().cloned().unwrap();
    assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
    assert!(b.get("reasoning_effort").is_none());
    chat(QWEN, Low, "two").await;
    let b = r.chats.lock().last().cloned().unwrap();
    assert_eq!(b["reasoning_effort"], "low");
    assert_eq!(b["chat_template_kwargs"]["enable_thinking"], true);
    chat(QWEN, XHigh, "three").await;
    assert_eq!(r.chats.lock().last().unwrap()["reasoning_effort"], "xhigh");
    chat(QWEN, Auto, "four").await;
    let b = r.chats.lock().last().cloned().unwrap();
    assert!(b.get("reasoning_effort").is_none() && b.get("chat_template_kwargs").is_none());
    // Discovery itself never loaded anything.
    let by_requests: Vec<String> = r.loads.lock().clone();
    assert_eq!(by_requests, [HYBRID, QWEN], "only replies loaded models");
    drop(e);
}

fn update(id: &str, model: Option<&str>, thinking: Option<ThinkingLevel>) -> Request {
    Request::UpdateSession {
        session_id: id.into(),
        title: None,
        model: model.map(|m| ModelRef {
            provider: "llama".into(),
            model: m.into(),
        }),
        thinking,
        permission: None,
        pinned: None,
        archived: None,
        cwd: None,
        members: None,
        fast: None,
        subagents: None,
    }
}

async fn new_chat(e: &Engine, model: &str) -> SessionInfo {
    serde_json::from_value(
        call(
            e,
            Request::CreateSession {
                cwd: None,
                model: Some(ModelRef {
                    provider: "llama".into(),
                    model: model.into(),
                }),
                title: Some("t".into()),
                bot_id: None,
                private: false,
                lean: false,
            },
        )
        .await,
    )
    .unwrap()
}

#[tokio::test]
async fn new_chats_start_at_the_level_last_picked_for_the_model() {
    let r = Arc::new(Router1::default());
    r.set(QWEN, "loaded");
    r.set(OSS, "unloaded");
    let url = start_router(r.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), &url).await;
    let m = modes(&e).await;
    assert_eq!(levels(&m[OSS]), [Auto, Low, Medium, High]);
    assert_eq!(m[QWEN].last_thinking, None);

    // Nothing picked yet: the model's default.
    let a = new_chat(&e, QWEN).await;
    assert_eq!(a.thinking, Auto);
    let a: SessionInfo =
        serde_json::from_value(call(&e, update(&a.id, None, Some(XHigh))).await).unwrap();
    assert_eq!(a.thinking, XHigh);
    assert_eq!(
        modes(&e).await[QWEN].last_thinking,
        Some(XHigh),
        "clients see it"
    );

    // A new chat on the model starts there.
    assert_eq!(new_chat(&e, QWEN).await.thinking, XHigh);
    // So does a chat switched to it from another model (gpt-oss starts at
    // medium, its maker's recommendation: never low for agent work).
    let c = new_chat(&e, OSS).await;
    assert_eq!(c.thinking, Medium);
    let c: SessionInfo =
        serde_json::from_value(call(&e, update(&c.id, Some(QWEN), None)).await).unwrap();
    assert_eq!(c.thinking, XHigh);
    // A level sent with the switch wins (and is remembered).
    let d = new_chat(&e, OSS).await;
    let d: SessionInfo =
        serde_json::from_value(call(&e, update(&d.id, Some(QWEN), Some(Low))).await).unwrap();
    assert_eq!(d.thinking, Low);
    assert_eq!(new_chat(&e, QWEN).await.thinking, Low);
    // Picking the default again is remembered too.
    call(&e, update(&d.id, None, Some(Auto))).await;
    assert_eq!(new_chat(&e, QWEN).await.thinking, Auto);
    // Other models are untouched.
    assert_eq!(new_chat(&e, OSS).await.thinking, Medium);

    // Kept across restarts; a level the model doesn't offer snaps to the nearest.
    e.store()
        .kv_set(
            "model_thinking",
            &json!({ format!("llama/{QWEN}"): "medium", format!("llama/{OSS}"): "xhigh" })
                .to_string(),
        )
        .unwrap();
    drop(e);
    let e = engine(dir.path(), &url).await;
    modes(&e).await;
    assert_eq!(new_chat(&e, QWEN).await.thinking, Medium);
    assert_eq!(new_chat(&e, OSS).await.thinking, High);
    nothing_loaded(&r);
    drop(e);
}

/// The models a local server listed stay listed after a restart while the
/// server is off (a PC asleep, a router restarting): they used to vanish
/// from the model picker until the server answered again.
#[tokio::test]
async fn the_models_stay_listed_while_the_server_is_off() {
    let r = Arc::new(Router1::default());
    r.set(QWEN, "loaded");
    r.set(UNCENSORED, "unloaded");
    let url = start_router(r.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    {
        let e = engine(dir.path(), &url).await;
        assert_eq!(modes(&e).await.len(), 2);
    }
    // Off: it lists nothing now.
    r.status.lock().clear();
    let e = engine(dir.path(), &url).await;
    let m = modes(&e).await;
    assert!(
        m.contains_key(QWEN) && m.contains_key(UNCENSORED),
        "{:?}",
        m.keys()
    );
    // Another server under that name: what the old one listed is moot.
    let other = start_router(Arc::new(Router1::default())).await;
    drop(e);
    let e = engine(dir.path(), &other).await;
    assert!(modes(&e).await.is_empty());
}
