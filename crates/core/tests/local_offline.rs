//! A model server of the user's own (llama.cpp, Ollama, LM Studio on this
//! computer or the network) that is switched off: its models stay listed,
//! marked offline (also after a restart of Tessera), a turn on one says in
//! plain words that the server isn't answering, and once it answers again
//! the models are normal without anyone refreshing by hand.

use std::sync::Arc;
use std::time::Duration;

use tessera_core::config::{Paths, ProviderConfig};
use tessera_core::llm::LlmRequest;
use tessera_core::providers::Providers;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MODEL: &str = "tiny-model";

/// A tiny OpenAI-compatible server on `addr` that closes every connection
/// after answering (so nothing lingers once it is stopped).
async fn serve(addr: std::net::SocketAddr) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let mut got = Vec::new();
                // Headers, then the body by its length.
                loop {
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    got.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&got).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if got.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let head = String::from_utf8_lossy(&got).to_string();
                let (ctype, body) = if head.starts_with("GET /v1/models") {
                    (
                        "application/json",
                        format!(
                            r#"{{"object":"list","data":[{{"id":"{MODEL}","object":"model"}}]}}"#
                        ),
                    )
                } else {
                    let a = r#"{"choices":[{"index":0,"delta":{"content":"Hi."}}]}"#;
                    let b = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#;
                    (
                        "text/event-stream",
                        format!("data: {a}\n\ndata: {b}\n\ndata: [DONE]\n\n"),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    })
}

/// A free port on this computer.
async fn free_addr() -> std::net::SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap()
}

async fn engine(dir: &std::path::Path, url: &str) -> Engine {
    let config = format!(
        "default_model = \"box/{MODEL}\"\n[agent]\nreview_interval = 0\n\n[[provider]]\nid = \"box\"\nname = \"Desk PC\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = true\n"
    );
    std::fs::write(dir.join("config.toml"), config).unwrap();
    Engine::open(Paths::at(dir), EngineOptions { scheduler: false })
        .await
        .unwrap()
}

async fn the_box(e: &Engine, refresh: bool) -> ProviderInfo {
    let v = e
        .handle(Request::ListProviders { refresh }, None)
        .await
        .unwrap();
    let list: Vec<ProviderInfo> = serde_json::from_value(v).unwrap();
    list.into_iter().find(|p| p.id == "box").unwrap()
}

fn ids(p: &ProviderInfo) -> Vec<&str> {
    p.models.iter().map(|m| m.id.as_str()).collect()
}

#[tokio::test]
async fn a_switched_off_server_keeps_its_models_marked_offline() {
    let dir = tempfile::tempdir().unwrap();
    let addr = free_addr().await;
    let url = format!("http://{addr}/v1");
    let server = serve(addr).await;

    let e = engine(dir.path(), &url).await;
    let p = the_box(&e, true).await;
    assert!(!p.offline, "answering");
    assert_eq!(ids(&p), [MODEL]);
    // Old clients see the same JSON as before while it answers.
    let raw = serde_json::to_value(&p).unwrap();
    assert!(raw.get("offline").is_none(), "{raw}");

    // Switched off: still listed, marked offline.
    server.abort();
    let _ = server.await;
    let p = the_box(&e, true).await;
    assert!(p.offline, "not answering");
    assert_eq!(ids(&p), [MODEL], "its models stay");

    // Tessera restarts while it is off: the same.
    drop(e);
    let e = engine(dir.path(), &url).await;
    e.refresh_models().await;
    let p = the_box(&e, false).await;
    assert!(p.offline, "still off after a restart");
    assert_eq!(ids(&p), [MODEL], "remembered across the restart");

    // It comes back: looking at the models is enough, clients are told.
    let server = serve(addr).await;
    let mut events = e.subscribe();
    let p = the_box(&e, false).await;
    assert!(p.offline, "answered from what is known, at once");
    // (Other changes may be announced first, e.g. the start-up look.)
    let back = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(Event::ProvidersChanged) => {
                    let p = the_box(&e, false).await;
                    if !p.offline {
                        return Some(p);
                    }
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();
    let p = back.expect("clients hear that it is back");
    assert_eq!(ids(&p), [MODEL]);
    server.abort();
}

#[tokio::test]
async fn a_turn_on_a_switched_off_server_says_so_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let auth = Arc::new(tessera_core::auth::AuthStore::load(dir.path().join("auth.json")).unwrap());
    let addr = free_addr().await;
    let cfg = ProviderConfig {
        id: "box".into(),
        name: "Desk PC".into(),
        kind: ProviderKind::OpenaiChat,
        base_url: format!("http://{addr}/v1"),
        discover: true,
        models: vec![],
        headers: Default::default(),
        reasoning_replay: Default::default(),
        parallel: None,
    };
    let (events, mut rx) = tokio::sync::broadcast::channel(16);
    let providers = Providers::new(reqwest::Client::new(), auth, &[cfg])
        .remembering(dir.path().join("model-caps.json"), events);
    let model = ModelRef {
        provider: "box".into(),
        model: MODEL.into(),
    };
    let req = LlmRequest {
        model: MODEL.into(),
        system: String::new(),
        messages: vec![],
        tools: vec![],
        thinking: ThinkingLevel::Auto,
        max_output: None,
        cache_key: "t".into(),
        fast: false,
        daybreak: false,
        no_tool_calls: false,
    };
    let (tx, _keep) = tokio::sync::mpsc::unbounded_channel();
    let cancel = tokio_util::sync::CancellationToken::new();

    // Nothing listens there.
    let err = providers
        .stream(&model, &req, &tx, &cancel)
        .await
        .unwrap_err();
    assert!(err.unreachable);
    assert_eq!(err.message, format!("Desk PC isn't answering at {addr}"));
    assert!(providers.any_offline(), "the picker shows it");
    assert!(matches!(rx.try_recv(), Ok(Event::ProvidersChanged)));

    // Started: the next turn works and the mark goes.
    let server = serve(addr).await;
    let r = providers.stream(&model, &req, &tx, &cancel).await.unwrap();
    assert_eq!(r.stop, StopReason::Stop);
    assert!(!providers.any_offline());
    assert!(matches!(rx.try_recv(), Ok(Event::ProvidersChanged)));
    server.abort();
}
