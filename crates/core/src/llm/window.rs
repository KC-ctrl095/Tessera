//! The context window a model server really runs a model with.
//!
//! A server's model list says what a model can do (its trained length), not
//! what the server runs it with: Ollama starts models with 4,096 tokens
//! unless set otherwise, LM Studio loads with the length chosen at load time,
//! llama.cpp with `-c`. A window assumed too large is worse than a small
//! one: the server silently drops the start of the prompt (Tessera's own
//! instructions) and nothing ever compacts. So Tessera asks each kind of
//! server what it runs the model with, in a way that never loads a model:
//!
//! - llama.cpp: `/props` of a model that is already loaded (`openai_chat::llama_props`),
//! - Ollama: `/api/ps` (what is loaded and its `context_length`) and
//!   `/api/show` (a model's own `num_ctx` setting),
//! - LM Studio: `/api/v1/models` (`loaded_instances`) or the older
//!   `/api/v0/models` (`loaded_context_length`).
//!
//! Only reads (and `/api/show`, a read that takes a POST). Hosted APIs are
//! never asked: this is for servers on the user's own computer or network.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};

/// About what Tessera needs: its own instructions and tool list use around
/// 5,000 tokens (2,000 in small model mode), and files, results and answers
/// need room beside them. Below this a model works badly.
pub const ENOUGH: u64 = 16_384;

/// Models asked about with `/api/show` in one look (Ollama lists everything
/// that was ever pulled; a model not loaded matters once it is chosen).
const SHOW_AT_MOST: usize = 24;

/// Each question has this long; a server that doesn't answer is skipped.
const ASK_FOR: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    LlamaCpp,
    Ollama,
    LmStudio,
}

impl Server {
    pub fn name(self) -> &'static str {
        match self {
            Server::LlamaCpp => "llama.cpp",
            Server::Ollama => "Ollama",
            Server::LmStudio => "LM Studio",
        }
    }
}

/// What a server runs one model with, as it said. A model that isn't loaded
/// and has no setting of its own has none: Ollama's start-up value depends on
/// the computer's video memory and on `OLLAMA_CONTEXT_LENGTH`, which no API
/// shows, so Tessera doesn't guess it (it is read after the model's first
/// answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub server: Server,
    pub tokens: u64,
}

/// The server's address without `/v1`: where its own API lives.
pub fn root(base_url: &str) -> &str {
    let b = base_url.trim_end_matches('/');
    b.strip_suffix("/v1").unwrap_or(b)
}

/// Which kind of server a model list came from, by what it says owns the
/// models. `None`: one that needs no asking (vLLM names its window in the
/// list; llama.cpp has its own route) or is not known.
pub fn guess(owner: &str) -> Option<Server> {
    match owner {
        "llamacpp" | "vllm" | "sglang" => None,
        "organization_owner" => Some(Server::LmStudio),
        // Ollama says `library` for its own models and the user's name for
        // others; anything else gets one cheap look at `/api/ps`.
        _ => Some(Server::Ollama),
    }
}

/// What the server runs each of `wanted` with (only those it says). Never
/// loads a model. `None`: it isn't that kind of server (or didn't answer).
pub async fn read(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
    server: Server,
    wanted: &[String],
) -> Option<HashMap<String, Window>> {
    match server {
        Server::Ollama => ollama(http, base_url, headers, wanted).await,
        Server::LmStudio => lm_studio(http, base_url, headers).await,
        Server::LlamaCpp => None,
    }
}

async fn get_json(
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Option<Value> {
    let mut rb = http.get(url).timeout(ASK_FOR);
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.ok()?;
    super::check_redirect(&resp).ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

/// Ollama names models `name:tag`; its list and `ps` agree on that, but a
/// config may leave `:latest` out.
fn same_model(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .strip_suffix(":latest")
            .unwrap_or(s.trim())
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// The `num_ctx` of a Modelfile's parameters (`/api/show` → `parameters`:
/// lines like `num_ctx    8192`).
fn modelfile_num_ctx(parameters: &str) -> Option<u64> {
    parameters.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some("num_ctx"))
            .then(|| w.next()?.parse::<u64>().ok())
            .flatten()
            .filter(|n| *n > 0)
    })
}

/// Ollama: what `/api/ps` says each loaded model runs with, and for the
/// other models of `wanted` their own `num_ctx`, when they have one. `None`:
/// this isn't Ollama.
async fn ollama(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
    wanted: &[String],
) -> Option<HashMap<String, Window>> {
    let root = root(base_url);
    let ps = get_json(http, &format!("{root}/api/ps"), headers).await?;
    let running = ps.get("models")?.as_array()?;
    let loaded = |id: &str| -> Option<u64> {
        running.iter().find_map(|m| {
            let name = m
                .get("model")
                .or_else(|| m.get("name"))
                .and_then(Value::as_str)?;
            same_model(name, id)
                .then(|| m.get("context_length").and_then(Value::as_u64))
                .flatten()
                .filter(|n| *n > 0)
        })
    };
    let mut out = HashMap::new();
    let mut ask: Vec<&str> = Vec::new();
    for id in wanted {
        match loaded(id) {
            Some(tokens) => {
                out.insert(
                    id.clone(),
                    Window {
                        server: Server::Ollama,
                        tokens,
                    },
                );
            }
            None => ask.push(id.as_str()),
        }
    }
    let mut shown = Vec::new();
    for chunk in ask.chunks(4).take(SHOW_AT_MOST / 4) {
        shown.extend(
            futures::future::join_all(chunk.iter().map(|id| show(http, root, headers, id))).await,
        );
    }
    out.extend(shown.into_iter().filter_map(|(id, w)| Some((id, w?))));
    Some(out)
}

/// `/api/show` for one model: its own `num_ctx` setting. `None`: it has none
/// (the server's start-up value applies) or Ollama doesn't know the model.
async fn show(
    http: &reqwest::Client,
    root: &str,
    headers: &[(String, String)],
    id: &str,
) -> (String, Option<Window>) {
    let mut rb = http
        .post(format!("{root}/api/show"))
        .json(&json!({"model": id}))
        .timeout(ASK_FOR);
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    let body: Option<Value> = match rb.send().await {
        Ok(r) if super::check_redirect(&r).is_ok() && r.status().is_success() => {
            r.json().await.ok()
        }
        _ => None,
    };
    let window = body.and_then(|b| {
        let tokens = b
            .get("parameters")
            .and_then(Value::as_str)
            .and_then(modelfile_num_ctx)?;
        Some(Window {
            server: Server::Ollama,
            tokens,
        })
    });
    (id.to_string(), window)
}

/// LM Studio: the loaded models and the length each was loaded with. Models
/// that are not loaded are left out: LM Studio picks the length when it
/// loads one for a request, and Tessera reads it after the first answer.
async fn lm_studio(
    http: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
) -> Option<HashMap<String, Window>> {
    let root = root(base_url);
    let window = |tokens: u64| Window {
        server: Server::LmStudio,
        tokens,
    };
    let mut out = HashMap::new();
    if let Some(v) = get_json(http, &format!("{root}/api/v1/models"), headers).await
        && let Some(models) = v.get("models").and_then(Value::as_array)
    {
        for m in models {
            let Some(instances) = m.get("loaded_instances").and_then(Value::as_array) else {
                continue;
            };
            let Some(tokens) = instances.iter().find_map(|i| {
                i.get("config")?
                    .get("context_length")?
                    .as_u64()
                    .filter(|n| *n > 0)
            }) else {
                continue;
            };
            for id in m
                .get("key")
                .and_then(Value::as_str)
                .into_iter()
                .chain(instances.iter().filter_map(|i| i.get("id")?.as_str()))
            {
                out.insert(id.to_string(), window(tokens));
            }
        }
        return Some(out);
    }
    // LM Studio before 0.4: the beta REST API.
    let v = get_json(http, &format!("{root}/api/v0/models"), headers).await?;
    for m in v.get("data")?.as_array()? {
        if m.get("state").and_then(Value::as_str) != Some("loaded") {
            continue;
        }
        if let (Some(id), Some(tokens)) = (
            m.get("id").and_then(Value::as_str),
            m.get("loaded_context_length")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0),
        ) {
            out.insert(id.to_string(), window(tokens));
        }
    }
    Some(out)
}

/// 4096 → "4,096".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string().into_bytes();
    let groups: Vec<&str> = digits
        .rchunks(3)
        .rev()
        .filter_map(|g| std::str::from_utf8(g).ok())
        .collect();
    groups.join(",")
}

/// How to give the model more room, in the server's own words.
fn how_to_raise(server: Server) -> &'static str {
    match server {
        Server::Ollama => {
            "To raise it: in the Ollama app open Settings and move \"Context length\" up to 32k or more (or start the server with OLLAMA_CONTEXT_LENGTH=32768), then let the model load again."
        }
        Server::LmStudio => {
            "To raise it: in LM Studio load the model again with \"Context Length\" set to 32768 or more (in the model's load settings)."
        }
        Server::LlamaCpp => {
            "To raise it: start llama-server with a larger window, for example -c 32768 (or set ctx-size in the model's preset)."
        }
    }
}

/// A plain-words warning when the window is too small for Tessera, with how
/// to raise it; `None` when it is enough.
pub fn warning(w: &Window) -> Option<String> {
    if w.tokens >= ENOUGH {
        return None;
    }
    let server = w.server.name();
    let n = thousands(w.tokens);
    Some(format!(
        "{server} runs this model with a window of {n} tokens. That is too small for Tessera: its own instructions need about 5,000 and it works best with 16,000 or more. With less, the start of a long conversation is lost or the request fails. {}",
        how_to_raise(w.server)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::State;
    use axum::routing::{get, post};
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    fn wording_and_numbers() {
        assert_eq!(thousands(4096), "4,096");
        assert_eq!(thousands(262144), "262,144");
        assert_eq!(thousands(999), "999");
        assert_eq!(root("http://h:11434/v1"), "http://h:11434");
        assert_eq!(root("http://h:11434/v1/"), "http://h:11434");
        assert_eq!(root("http://h:1234"), "http://h:1234");
        assert_eq!(
            modelfile_num_ctx("temperature 0.7\nnum_ctx                        8192\nstop \"x\""),
            Some(8192)
        );
        assert_eq!(modelfile_num_ctx("stop \"num_ctx 5\""), None);
        assert_eq!(modelfile_num_ctx("num_ctx 0"), None);
        assert!(same_model("llama3.2", "llama3.2:latest"));
        assert!(same_model("Qwen3:8B", "qwen3:8b"));
        assert!(!same_model("qwen3:8b", "qwen3:14b"));
        let w = |server, tokens| Window { server, tokens };
        let ollama = warning(&w(Server::Ollama, 4096)).unwrap();
        assert!(
            ollama.starts_with("Ollama runs this model with a window of 4,096 tokens."),
            "{ollama}"
        );
        assert!(
            ollama.contains("OLLAMA_CONTEXT_LENGTH") && ollama.contains("Settings"),
            "{ollama}"
        );
        assert_eq!(warning(&w(Server::Ollama, 16_384)), None);
        assert_eq!(warning(&w(Server::Ollama, 131_072)), None);
        assert!(
            warning(&w(Server::LmStudio, 4096))
                .unwrap()
                .contains("Context Length")
        );
        assert!(
            warning(&w(Server::LlamaCpp, 8192))
                .unwrap()
                .contains("-c 32768")
        );
        assert_eq!(guess("llamacpp"), None);
        assert_eq!(guess("vllm"), None);
        assert_eq!(guess("organization_owner"), Some(Server::LmStudio));
        assert_eq!(guess("library"), Some(Server::Ollama));
        assert_eq!(guess(""), Some(Server::Ollama));
    }

    /// What a test server was asked: "GET /api/ps", "POST /api/show llama3.2"…
    type Log = Arc<Mutex<Vec<String>>>;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/v1")
    }

    async fn ollama_server(ps: Value, shows: Vec<(&'static str, &'static str)>) -> (String, Log) {
        let log: Log = Default::default();
        let shows: Arc<Vec<(&str, &str)>> = Arc::new(shows);
        let app = Router::new()
            .route(
                "/api/ps",
                get(move |State(l): State<Log>| {
                    let ps = ps.clone();
                    async move {
                        l.lock().push("GET /api/ps".into());
                        axum::Json(ps)
                    }
                }),
            )
            .route(
                "/api/show",
                post(move |State(l): State<Log>, axum::Json(b): axum::Json<Value>| {
                    let shows = shows.clone();
                    async move {
                        let id = b["model"].as_str().unwrap_or("").to_string();
                        l.lock().push(format!("POST /api/show {id}"));
                        match shows.iter().find(|(m, _)| *m == id) {
                            Some((_, params)) => (
                                axum::http::StatusCode::OK,
                                axum::Json(json!({"parameters": params, "model_info": {"llama.context_length": 131072}})),
                            ),
                            None => (
                                axum::http::StatusCode::NOT_FOUND,
                                axum::Json(json!({"error": "model not found"})),
                            ),
                        }
                    }
                }),
            )
            .with_state(log.clone());
        (serve(app).await, log)
    }

    #[tokio::test]
    async fn ollama_loaded_models_say_their_window_and_the_others_their_settings() {
        let (url, log) = ollama_server(
            json!({"models": [
                {"name": "llama3.2:latest", "model": "llama3.2:latest", "context_length": 4096},
                {"name": "qwen3:8b", "model": "qwen3:8b", "size": 1}
            ]}),
            vec![
                ("qwen3:8b", "num_ctx 2048"),
                ("tuned:latest", "temperature 0.7\nnum_ctx   32768"),
                ("plain:latest", "temperature 0.7"),
            ],
        )
        .await;
        let wanted: Vec<String> = [
            "llama3.2",
            "qwen3:8b",
            "tuned:latest",
            "plain:latest",
            "gone:latest",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let http = reqwest::Client::new();
        let w = read(&http, &url, &[], Server::Ollama, &wanted)
            .await
            .unwrap();
        let win = |tokens| Window {
            server: Server::Ollama,
            tokens,
        };
        // Loaded: what it runs with now, even for a name written without the tag.
        assert_eq!(w["llama3.2"], win(4096));
        // Loaded without a `context_length` (older Ollama): its settings.
        assert_eq!(w["qwen3:8b"], win(2048));
        // Not loaded: its own setting. Without one the server's start-up value
        // applies, which no API shows: nothing is guessed.
        assert_eq!(w["tuned:latest"], win(32768));
        assert!(!w.contains_key("plain:latest"));
        assert!(!w.contains_key("gone:latest"));
        // Reads only: one look at what is loaded, and `show` for the rest.
        let mut asked = log.lock().clone();
        asked.sort();
        assert_eq!(
            asked,
            [
                "GET /api/ps",
                "POST /api/show gone:latest",
                "POST /api/show plain:latest",
                "POST /api/show qwen3:8b",
                "POST /api/show tuned:latest",
            ]
        );
    }

    #[tokio::test]
    async fn something_that_is_not_ollama_is_left_alone() {
        let app = Router::new().route(
            "/api/ps",
            get(|| async { axum::http::StatusCode::NOT_FOUND }),
        );
        let url = serve(app).await;
        let http = reqwest::Client::new();
        let wanted = vec!["m".to_string()];
        assert!(
            read(&http, &url, &[], Server::Ollama, &wanted)
                .await
                .is_none()
        );
        // A server that answers with something else entirely.
        let app = Router::new().route("/api/ps", get(|| async { axum::Json(json!({"hello": 1})) }));
        let url = serve(app).await;
        assert!(
            read(&http, &url, &[], Server::Ollama, &wanted)
                .await
                .is_none()
        );
        // Nothing listening.
        assert!(
            read(&http, "http://127.0.0.1:9/v1", &[], Server::Ollama, &wanted)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn lm_studio_loaded_models_say_the_length_they_were_loaded_with() {
        let log: Log = Default::default();
        let app = Router::new()
            .route(
                "/api/v1/models",
                get(|State(l): State<Log>| async move {
                    l.lock().push("GET /api/v1/models".into());
                    axum::Json(json!({"models": [
                        {"type": "llm", "key": "google/gemma-4-26b-a4b", "max_context_length": 262144,
                         "loaded_instances": [{"id": "google/gemma-4-26b-a4b", "config": {"context_length": 4096}}]},
                        {"type": "llm", "key": "deepseek-r1", "max_context_length": 131072, "loaded_instances": []},
                        {"type": "embedding", "key": "nomic", "loaded_instances": []}
                    ]}))
                }),
            )
            .with_state(log.clone());
        let url = serve(app).await;
        let http = reqwest::Client::new();
        let w = read(&http, &url, &[], Server::LmStudio, &[]).await.unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(
            w["google/gemma-4-26b-a4b"],
            Window {
                server: Server::LmStudio,
                tokens: 4096,
            }
        );
        assert_eq!(*log.lock(), ["GET /api/v1/models"]);
    }

    #[tokio::test]
    async fn lm_studio_before_its_new_api_is_read_through_the_beta_one() {
        let app = Router::new().route(
            "/api/v0/models",
            get(|| async {
                axum::Json(json!({"object": "list", "data": [
                    {"id": "qwen2-vl-7b-instruct", "state": "not-loaded", "max_context_length": 32768},
                    {"id": "meta-llama-3.1-8b-instruct", "state": "loaded", "max_context_length": 131072, "loaded_context_length": 8192},
                    {"id": "loaded-without-the-field", "state": "loaded", "max_context_length": 4096}
                ]}))
            }),
        );
        let url = serve(app).await;
        let http = reqwest::Client::new();
        let w = read(&http, &url, &[], Server::LmStudio, &[]).await.unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w["meta-llama-3.1-8b-instruct"].tokens, 8192);
    }
}
