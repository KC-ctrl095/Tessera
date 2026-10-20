//! The context window of models on the user's own server: read from the
//! server (Ollama, LM Studio, llama.cpp) instead of assumed, without ever
//! asking for a model that isn't loaded; a window too small for Tessera is
//! said plainly in the model list and once in the chat, with how to raise it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

type Log = Arc<Mutex<Vec<String>>>;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn reply() -> impl IntoResponse {
    let mut body = String::new();
    for c in [
        json!({"choices": [{"index": 0, "delta": {"content": "Hi."}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 10, "completion_tokens": 2}}),
    ] {
        body.push_str(&format!("data: {c}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], body)
}

async fn engine(dir: &std::path::Path, id: &str, name: &str, url: &str, extra: &str) -> Engine {
    let config = format!(
        "default_model = \"{id}/{name}\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\ncheck = false\n\n[[provider]]\nid = \"{id}\"\nname = \"{id}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = true\n{extra}"
    );
    std::fs::write(dir.join("config.toml"), config).unwrap();
    Engine::open(Paths::at(dir), EngineOptions { scheduler: false })
        .await
        .unwrap()
}

async fn models(e: &Engine, provider: &str) -> HashMap<String, ModelInfo> {
    let v = e
        .handle(Request::ListProviders { refresh: true }, None)
        .await
        .unwrap();
    let providers: Vec<ProviderInfo> = serde_json::from_value(v).unwrap();
    providers
        .into_iter()
        .filter(|p| p.id == provider)
        .flat_map(|p| p.models)
        .map(|m| (m.id.clone(), m))
        .collect()
}

// ---- Ollama -----------------------------------------------------------------------

/// A stock Ollama: `/v1/models` lists models without a window, `/api/ps` the
/// loaded ones, `/api/show` a model's settings. A chat request loads the
/// model (with 4,096 tokens, Ollama's start-up value).
#[derive(Default)]
struct Ollama {
    /// Every request, "METHOD path".
    log: Log,
    /// Models in memory: name → window.
    loaded: Mutex<HashMap<String, u64>>,
    /// Requests that would have loaded a model, by name.
    loads: Mutex<Vec<String>>,
    /// A slow context read after a reply must not delay the turn ending.
    hold_ps: AtomicBool,
    ps_started: tokio::sync::Notify,
    release_ps: tokio::sync::Notify,
}

async fn start_ollama(o: Arc<Ollama>) -> String {
    let app = Router::new()
        .route(
            "/v1/models",
            get(|State(o): State<Arc<Ollama>>| async move {
                o.log.lock().push("GET /v1/models".into());
                axum::Json(json!({"object": "list", "data": [
                    {"id": "llama3.2:latest", "object": "model", "owned_by": "library"},
                    {"id": "tuned:latest", "object": "model", "owned_by": "library"},
                    {"id": "plain:latest", "object": "model", "owned_by": "library"},
                ]}))
            }),
        )
        .route(
            "/api/ps",
            get(|State(o): State<Arc<Ollama>>| async move {
                o.log.lock().push("GET /api/ps".into());
                if o.hold_ps.load(Ordering::SeqCst) {
                    o.ps_started.notify_one();
                    o.release_ps.notified().await;
                }
                let models: Vec<Value> = o
                    .loaded
                    .lock()
                    .iter()
                    .map(|(n, c)| json!({"name": n, "model": n, "context_length": c}))
                    .collect();
                axum::Json(json!({"models": models}))
            }),
        )
        .route(
            "/api/show",
            post(
                |State(o): State<Arc<Ollama>>, axum::Json(b): axum::Json<Value>| async move {
                    let m = b["model"].as_str().unwrap_or("").to_string();
                    o.log.lock().push(format!("POST /api/show {m}"));
                    let parameters = if m == "tuned:latest" {
                        "temperature 0.7\nnum_ctx                        32768"
                    } else {
                        "temperature 0.7"
                    };
                    axum::Json(json!({"parameters": parameters, "model_info": {"llama.context_length": 131072}}))
                },
            ),
        )
        .route(
            "/v1/chat/completions",
            post(
                |State(o): State<Arc<Ollama>>, axum::Json(b): axum::Json<Value>| async move {
                    let m = b["model"].as_str().unwrap_or("").to_string();
                    o.log.lock().push(format!("POST /v1/chat/completions {m}"));
                    if !o.loaded.lock().contains_key(&m) {
                        o.loads.lock().push(m.clone());
                        o.loaded.lock().insert(m, 4096);
                    }
                    reply()
                },
            ),
        )
        .fallback(|State(o): State<Arc<Ollama>>, req: axum::extract::Request| async move {
            o.log
                .lock()
                .push(format!("UNEXPECTED {} {}", req.method(), req.uri().path()));
            axum::http::StatusCode::NOT_FOUND
        })
        .with_state(o);
    serve(app).await
}

async fn chat(e: &Engine, model: &str, text: &str) -> (String, Vec<Entry>) {
    let v = e
        .handle(
            Request::CreateSession {
                cwd: Some(std::env::temp_dir().display().to_string()),
                model: Some(model.parse().unwrap()),
                // Titled up front: no title request.
                title: Some("T".into()),
                bot_id: None,
                private: false,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(v).unwrap();
    send(e, &s.id, text).await;
    let entries = entries(e, &s.id).await;
    (s.id, entries)
}

async fn send(e: &Engine, id: &str, text: &str) {
    e.handle(
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    tokio::time::timeout(std::time::Duration::from_secs(20), e.wait_idle(id))
        .await
        .expect("the turn never ended");
}

async fn entries(e: &Engine, id: &str) -> Vec<Entry> {
    let v = e
        .handle(
            Request::GetSession {
                session_id: id.into(),
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value::<SessionSnapshot>(v)
        .unwrap()
        .entries
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// The window notices of a chat, once there are `n` (they come beside the turn).
async fn wait_for_warnings(e: &Engine, id: &str, n: usize) -> Vec<String> {
    for _ in 0..100 {
        let w = warnings(&entries(e, id).await);
        if w.len() >= n {
            return w;
        }
        sleep_ms(50).await;
    }
    panic!("no window notice: {:?}", entries(e, id).await);
}

fn warnings(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { level, text, .. }
                if *level == NoticeLevel::Warning && text.contains("window") =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ollama_windows_are_read_and_a_small_one_is_said_in_plain_words() {
    let o = Arc::new(Ollama::default());
    // One model is in memory, started the usual way: 4,096 tokens.
    o.loaded.lock().insert("llama3.2:latest".into(), 4096);
    let url = start_ollama(o.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "ollama", "llama3.2:latest", &url, "").await;

    let m = models(&e, "ollama").await;
    // Loaded: exactly what Ollama runs it with, and the warning says how to raise it.
    assert_eq!(m["llama3.2:latest"].context_window, 4096);
    let w = m["llama3.2:latest"].context_warning.clone().unwrap();
    assert!(
        w.starts_with("Ollama runs this model with a window of 4,096 tokens."),
        "{w}"
    );
    assert!(
        w.contains("too small for Tessera") && w.contains("OLLAMA_CONTEXT_LENGTH"),
        "{w}"
    );
    // Not loaded, with its own `num_ctx`: that, and nothing to warn about.
    assert_eq!(m["tuned:latest"].context_window, 32768);
    assert_eq!(m["tuned:latest"].context_warning, None);
    // Not loaded, no setting: Ollama's start-up value depends on the computer
    // and on `OLLAMA_CONTEXT_LENGTH`, which no API shows. Tessera keeps its
    // usual figure and does not guess a warning; the first answer shows.
    assert_eq!(m["plain:latest"].context_window, 128_000);
    assert_eq!(m["plain:latest"].context_warning, None);

    // Only reads so far: nothing asked Ollama to load a model.
    let asked = o.log.lock().clone();
    for line in &asked {
        assert!(
            matches!(
                line.as_str(),
                "GET /v1/models"
                    | "GET /api/ps"
                    | "POST /api/show tuned:latest"
                    | "POST /api/show plain:latest"
            ),
            "unexpected request: {line}\n{asked:?}"
        );
    }
    assert!(asked.contains(&"GET /api/ps".to_string()));
    assert!(o.loads.lock().is_empty());

    // The user chats with the model that wasn't loaded: Ollama loads it with
    // 4,096; the answer comes first, then (beside the turn) Tessera reads what
    // it runs with, and the chat says so, once.
    let (id, _) = chat(&e, "ollama/plain:latest", "hello").await;
    assert_eq!(*o.loads.lock(), ["plain:latest"]);
    let told = wait_for_warnings(&e, &id, 1).await;
    assert!(
        told[0].starts_with("Ollama runs this model with a window of 4,096 tokens."),
        "{}",
        told[0]
    );
    let m = models(&e, "ollama").await;
    assert_eq!(m["plain:latest"].context_window, 4096);
    assert!(
        m["plain:latest"]
            .context_warning
            .as_deref()
            .unwrap()
            .starts_with("Ollama runs this model")
    );
    // A second message: no second notice.
    send(&e, &id, "again").await;
    sleep_ms(300).await;
    assert_eq!(warnings(&entries(&e, &id).await).len(), 1);

    // Ollama unloads the model (five idle minutes): what was read stays, it
    // isn't replaced by the usual figure.
    o.loaded.lock().clear();
    let m = models(&e, "ollama").await;
    assert_eq!(m["plain:latest"].context_window, 4096);
    assert!(m["plain:latest"].context_warning.is_some());

    // The server restarts; the chat goes on: the notice is in its history, so
    // it isn't said again.
    drop(e);
    let e = engine(dir.path(), "ollama", "llama3.2:latest", &url, "").await;
    // (A first look at the server, as the app's does.)
    models(&e, "ollama").await;
    send(&e, &id, "after the restart").await;
    let m = loop {
        let m = models(&e, "ollama").await;
        if m["plain:latest"].context_window == 4096 {
            break m;
        }
        sleep_ms(50).await;
    };
    assert!(m["plain:latest"].context_warning.is_some());
    sleep_ms(300).await;
    assert_eq!(warnings(&entries(&e, &id).await).len(), 1);
    assert!(
        !o.log.lock().iter().any(|l| l.starts_with("UNEXPECTED")),
        "{:?}",
        o.log.lock()
    );
}

#[tokio::test]
async fn a_slow_window_read_runs_beside_the_turn_not_in_front_of_it() {
    let o = Arc::new(Ollama::default());
    let url = start_ollama(o.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "ollama", "plain:latest", &url, "").await;
    models(&e, "ollama").await;
    o.hold_ps.store(true, Ordering::SeqCst);
    let (id, _) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        chat(&e, "ollama/plain:latest", "hello"),
    )
    .await
    .expect("the turn waited for the slow context read");
    tokio::time::timeout(std::time::Duration::from_secs(2), o.ps_started.notified())
        .await
        .expect("the detached context read never started");
    assert!(warnings(&entries(&e, &id).await).is_empty());
    o.hold_ps.store(false, Ordering::SeqCst);
    o.release_ps.notify_one();
    let told = wait_for_warnings(&e, &id, 1).await;
    assert!(told[0].contains("4,096 tokens"), "{told:?}");
}
#[tokio::test]
async fn a_window_the_user_set_themselves_is_theirs() {
    let o = Arc::new(Ollama::default());
    o.loaded.lock().insert("llama3.2:latest".into(), 4096);
    let url = start_ollama(o.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(
        dir.path(),
        "ollama",
        "llama3.2:latest",
        &url,
        "[[provider.models]]\nid = \"llama3.2:latest\"\ncontext_window = 65536\n",
    )
    .await;
    let m = models(&e, "ollama").await;
    assert_eq!(m["llama3.2:latest"].context_window, 65536);
    assert_eq!(m["llama3.2:latest"].context_warning, None);
}

#[tokio::test]
async fn a_big_enough_window_says_nothing() {
    let o = Arc::new(Ollama::default());
    o.loaded.lock().insert("llama3.2:latest".into(), 32768);
    let url = start_ollama(o.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "ollama", "llama3.2:latest", &url, "").await;
    let m = models(&e, "ollama").await;
    assert_eq!(m["llama3.2:latest"].context_window, 32768);
    assert_eq!(m["llama3.2:latest"].context_warning, None);
}

// ---- LM Studio ---------------------------------------------------------------------

#[tokio::test]
async fn lm_studio_loaded_models_have_the_length_they_were_loaded_with() {
    let log: Log = Default::default();
    let app = Router::new()
        .route(
            "/v1/models",
            get(|State(l): State<Log>| async move {
                l.lock().push("GET /v1/models".into());
                axum::Json(json!({"object": "list", "data": [
                    {"id": "google/gemma-4-26b-a4b", "object": "model", "owned_by": "organization_owner"},
                    {"id": "deepseek-r1", "object": "model", "owned_by": "organization_owner"},
                ]}))
            }),
        )
        .route(
            "/api/v1/models",
            get(|State(l): State<Log>| async move {
                l.lock().push("GET /api/v1/models".into());
                axum::Json(json!({"models": [
                    {"type": "llm", "key": "google/gemma-4-26b-a4b", "max_context_length": 262144,
                     "loaded_instances": [{"id": "google/gemma-4-26b-a4b", "config": {"context_length": 4096}}]},
                    {"type": "llm", "key": "deepseek-r1", "max_context_length": 131072, "loaded_instances": []},
                ]}))
            }),
        )
        .fallback(|State(l): State<Log>, req: axum::extract::Request| async move {
            l.lock()
                .push(format!("UNEXPECTED {} {}", req.method(), req.uri().path()));
            axum::http::StatusCode::NOT_FOUND
        })
        .with_state(log.clone());
    let url = serve(app).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "lms", "deepseek-r1", &url, "").await;
    let m = models(&e, "lms").await;
    assert_eq!(m["google/gemma-4-26b-a4b"].context_window, 4096);
    let w = m["google/gemma-4-26b-a4b"].context_warning.clone().unwrap();
    assert!(
        w.starts_with("LM Studio runs this model with a window of 4,096 tokens."),
        "{w}"
    );
    assert!(w.contains("Context Length"), "{w}");
    // Not loaded: LM Studio chooses when it loads one; nothing claimed.
    assert_eq!(m["deepseek-r1"].context_window, 128_000);
    assert_eq!(m["deepseek-r1"].context_warning, None);
    // Reads of the list and of what is loaded, nothing else (no llama.cpp
    // `/props`, nothing that loads a model).
    let asked = log.lock().clone();
    assert!(
        asked
            .iter()
            .all(|l| l == "GET /v1/models" || l == "GET /api/v1/models"),
        "{asked:?}"
    );
    assert!(asked.contains(&"GET /api/v1/models".to_string()));
}

// ---- llama.cpp ---------------------------------------------------------------------

#[tokio::test]
async fn llama_cpp_reports_the_window_it_runs_not_the_one_the_model_was_trained_for() {
    let props: Log = Default::default();
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                axum::Json(json!({"object": "list", "data": [
                    {"id": "small.gguf", "object": "model", "owned_by": "llamacpp",
                     "meta": {"n_ctx_train": 262144, "n_vocab": 1}},
                ]}))
            }),
        )
        .route(
            "/props",
            get(|State(l): State<Log>, req: axum::extract::Request| async move {
                l.lock().push(format!("GET {}", req.uri()));
                axum::Json(json!({"default_generation_settings": {"n_ctx": 8192}, "chat_template": "{{ messages }}"}))
            }),
        )
        .with_state(props.clone());
    let url = serve(app).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "llama", "small.gguf", &url, "").await;
    let m = models(&e, "llama").await;
    // `llama-server -c 8192` on a model trained for 262,144.
    assert_eq!(m["small.gguf"].context_window, 8192);
    let w = m["small.gguf"].context_warning.clone().unwrap();
    assert!(
        w.starts_with("llama.cpp runs this model with a window of 8,192 tokens."),
        "{w}"
    );
    assert!(w.contains("-c 32768"), "{w}");
    // Asked about the loaded model only, and refused to load another.
    let asked = props.lock().clone();
    assert!(!asked.is_empty());
    assert!(
        asked
            .iter()
            .all(|a| a.contains("model=small.gguf") && a.contains("autoload=false")),
        "{asked:?}"
    );
}

// ---- llama.cpp router: an unloaded second model, and a window that changes ----------

/// A llama.cpp router that holds one model: `/v1/models` with each model's
/// status and `--ctx-size`, `/props` of the loaded one. Like the real one, a
/// request for a model that isn't loaded loads it (unless `autoload=false`).
#[derive(Default)]
struct Router1 {
    status: Mutex<HashMap<String, &'static str>>,
    /// What a loaded model runs with now (the user may restart it smaller).
    running: Mutex<HashMap<String, u64>>,
    /// What each model is launched with (`--ctx-size`).
    launch: Mutex<HashMap<String, u64>>,
    /// `/props` requests: (model, autoload flag, loaded at the time).
    props: Mutex<Vec<(String, Option<String>, bool)>>,
    loads: Mutex<Vec<String>>,
    unload_a_after_reply: AtomicBool,
    a_completed: AtomicBool,
    model_lists: std::sync::atomic::AtomicUsize,
    unloaded_requests: Mutex<Vec<String>>,
}

async fn start_router(r: Arc<Router1>) -> String {
    let app = Router::new()
        .route(
            "/v1/models",
            get(|State(r): State<Arc<Router1>>| async move {
                r.model_lists.fetch_add(1, Ordering::SeqCst);
                // The detached check comes only after A's completion. Change
                // slots here, before its status reply and any targeted read.
                if r.unload_a_after_reply.load(Ordering::SeqCst)
                    && r.a_completed.load(Ordering::SeqCst)
                {
                    r.status.lock().insert("A".into(), "unloaded");
                    r.status.lock().insert("B".into(), "loaded");
                }
                let status = r.status.lock().clone();
                let launch = r.launch.lock().clone();
                let mut ids: Vec<&String> = status.keys().collect();
                ids.sort();
                let data: Vec<Value> = ids
                    .into_iter()
                    .map(|id| {
                        json!({
                            "id": id, "object": "model", "owned_by": "llamacpp",
                            "status": {"value": status[id], "args": ["llama-server", "--jinja", "--ctx-size", launch[id].to_string()]},
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
                 axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>| async move {
                    let model = q.get("model").cloned().unwrap_or_default();
                    let autoload = q.get("autoload").cloned();
                    let loaded = r.status.lock().get(&model).copied() == Some("loaded");
                    r.props
                        .lock()
                        .push((model.clone(), autoload.clone(), loaded));
                    if !loaded {
                        r.unloaded_requests.lock().push(model.clone());
                        if autoload.as_deref() != Some("false") {
                            r.loads.lock().push(model);
                        }
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(json!({"error": {"message": "model is not loaded"}})),
                        )
                            .into_response();
                    }
                    let n_ctx = r.running.lock().get(&model).copied().unwrap_or(0);
                    axum::Json(json!({"default_generation_settings": {"n_ctx": n_ctx}, "chat_template": "{{ messages }}"}))
                        .into_response()
                },
            ),
        )
        .route(
            "/v1/chat/completions",
            post(
                |State(r): State<Arc<Router1>>, axum::Json(b): axum::Json<Value>| async move {
                    let model = b["model"].as_str().unwrap_or("").to_string();
                    if r.status.lock().get(&model).copied() != Some("loaded") {
                        r.loads.lock().push(model.clone());
                        r.unloaded_requests.lock().push(model.clone());
                    }
                    if model == "A" {
                        r.a_completed.store(true, Ordering::SeqCst);
                    }
                    reply()
                },
            ),
        )
        .with_state(r);
    serve(app).await
}

#[tokio::test(start_paused = true)]
async fn a_router_tells_what_its_models_run_with_and_a_restart_with_less_is_noticed() {
    let r = Arc::new(Router1::default());
    for (id, state, ctx) in [("big", "loaded", 65_536u64), ("small", "unloaded", 8_192)] {
        r.status.lock().insert(id.into(), state);
        r.launch.lock().insert(id.into(), ctx);
    }
    r.running.lock().insert("big".into(), 65_536);
    let url = start_router(r.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "router", "big", &url, "").await;

    let m = models(&e, "router").await;
    // The loaded one: what `/props` says. The unloaded one: its `--ctx-size`,
    // too small, said without asking the router about it.
    assert_eq!(m["big"].context_window, 65_536);
    assert_eq!(m["big"].context_warning, None);
    assert_eq!(m["small"].context_window, 8_192);
    let w = m["small"].context_warning.clone().unwrap();
    assert!(
        w.starts_with("llama.cpp runs this model with a window of 8,192 tokens."),
        "{w}"
    );
    {
        let asked = r.props.lock().clone();
        assert!(!asked.is_empty());
        assert!(
            asked.iter().all(|(model, autoload, loaded)| model == "big"
                && autoload.as_deref() == Some("false")
                && *loaded),
            "{asked:?}"
        );
        assert!(r.loads.lock().is_empty());
    }

    // A chat on the loaded model: the window is read again beside the turn
    // (the same, so nothing is said).
    let (id, _) = chat(&e, "router/big", "hello").await;
    sleep_ms(300).await;
    assert!(warnings(&entries(&e, &id).await).is_empty());
    let before = r.props.lock().len();
    assert!(before >= 2, "the window was read again after the reply");

    // The user restarts the server with a smaller window (-c 4096); a minute
    // later the next answer shows it, to the list and to the chat.
    r.running.lock().insert("big".into(), 4_096);
    tokio::time::sleep(std::time::Duration::from_secs(61)).await;
    send(&e, &id, "again").await;
    let told = wait_for_warnings(&e, &id, 1).await;
    assert!(
        told[0].starts_with("llama.cpp runs this model with a window of 4,096 tokens."),
        "{}",
        told[0]
    );
    let m = models(&e, "router").await;
    assert_eq!(m["big"].context_window, 4_096);
    assert!(m["big"].context_warning.is_some());
    // Still only the loaded model was ever asked about, and nothing loaded.
    let asked = r.props.lock().clone();
    assert!(
        asked.iter().all(|(model, autoload, loaded)| model == "big"
            && autoload.as_deref() == Some("false")
            && *loaded),
        "{asked:?}"
    );
    assert!(r.loads.lock().is_empty(), "{:?}", r.loads.lock());
}

#[tokio::test]
async fn router_unload_between_completion_and_detached_read_never_names_unloaded_a() {
    let r = Arc::new(Router1::default());
    for (id, state) in [("A", "loaded"), ("B", "unloaded")] {
        r.status.lock().insert(id.into(), state);
        r.launch.lock().insert(id.into(), 65_536);
        r.running.lock().insert(id.into(), 65_536);
    }
    let url = start_router(r.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path(), "router", "A", &url, "").await;
    models(&e, "router").await;
    r.props.lock().clear();
    r.unload_a_after_reply.store(true, Ordering::SeqCst);
    let lists = r.model_lists.load(Ordering::SeqCst);
    chat(&e, "router/A", "hello").await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while r.model_lists.load(Ordering::SeqCst) == lists {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the detached read never refreshed model status");
    assert_eq!(r.status.lock()["A"], "unloaded");
    assert!(
        r.props.lock().is_empty(),
        "a targeted read survived unloading A: {:?}",
        r.props.lock()
    );
    assert!(
        r.unloaded_requests.lock().is_empty(),
        "named an unloaded model: {:?}",
        r.unloaded_requests.lock()
    );
}
