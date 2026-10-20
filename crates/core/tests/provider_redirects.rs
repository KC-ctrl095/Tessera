//! Provider redirects never forward credentials, prompts or window reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use parking_lot::Mutex;
use serde_json::json;
use tessera_core::auth::{AuthStore, Credential};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

async fn serve(app: Router) -> String {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    format!("http://{addr}")
}

struct Rig {
    engine: Engine,
    destination: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    _home: tempfile::TempDir,
    work: tempfile::TempDir,
}

async fn rig(path: &str, status: u16, owner: &str, kind: &str) -> Rig {
    let destination = Arc::new(AtomicUsize::new(0));
    let hits = destination.clone();
    let to = serve(Router::new().fallback(move || {
        hits.fetch_add(1, Ordering::SeqCst);
        async { axum::Json(json!({"data": []})) }
    }))
    .await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let path = path.to_string();
    let owner = owner.to_string();
    let from = serve(Router::new().fallback(move |req: axum::extract::Request| {
        let (path, to, owner, log) = (path.clone(), to.clone(), owner.clone(), log.clone());
        async move {
            log.lock().push(req.uri().path().to_string());
            if req.uri().path() == path {
                return (StatusCode::from_u16(status).unwrap(), [("location", to)]).into_response();
            }
            match req.uri().path() {
                "/v1/models" => {
                    axum::Json(json!({"data": [{"id": "m", "owned_by": owner}]})).into_response()
                }
                "/api/ps" => axum::Json(json!({"models": []})).into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }))
    .await;
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::at(home.path());
    paths.ensure().unwrap();
    AuthStore::load(paths.auth_file())
        .unwrap()
        .set(
            "p",
            Credential::ApiKey {
                key: "fixture-key".into(),
            },
        )
        .unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = 'p/m'\n[agent]\nreview_interval = 0\n[tuning.'*']\ncheck = false\n[[provider]]\nid = 'p'\nkind = '{kind}'\nbase_url = '{from}/v1'\ndiscover = true\n[[provider.models]]\nid = 'm'\ncontext_window = 100000\n"),
    )
    .unwrap();
    Rig {
        engine: Engine::open(paths, EngineOptions { scheduler: false })
            .await
            .unwrap(),
        destination,
        requests,
        _home: home,
        work: tempfile::tempdir().unwrap(),
    }
}

#[tokio::test]
async fn redirected_completions_fail_plainly_without_contacting_the_destination() {
    for (kind, path) in [
        ("openai_chat", "/v1/chat/completions"),
        ("openai_responses", "/v1/responses"),
        ("anthropic", "/v1/messages"),
    ] {
        for status in [301, 302, 303, 307, 308] {
            let r = rig(path, status, "llamacpp", kind).await;
            let session: SessionInfo = serde_json::from_value(
                r.engine
                    .handle(
                        Request::CreateSession {
                            cwd: Some(r.work.path().display().to_string()),
                            model: None,
                            title: Some("Redirect fixture".into()),
                            bot_id: None,
                            private: false,
                            lean: false,
                        },
                        None,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            r.engine
                .handle(
                    Request::Send {
                        session_id: session.id.clone(),
                        content: vec![ContentPart::text("hello")],
                        delivery: Delivery::Auto,
                        attachments: Vec::new(),
                    },
                    None,
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::time::timeout(Duration::from_secs(5), r.engine.wait_idle(&session.id))
                .await
                .unwrap();
            let snapshot: SessionSnapshot = serde_json::from_value(
                r.engine
                    .handle(
                        Request::GetSession {
                            session_id: session.id,
                            since_seq: None,
                        },
                        None,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(snapshot.entries.iter().any(|e| matches!(&e.body,
                EntryBody::Notice { level: NoticeLevel::Error, text, .. }
                if text.contains(&format!("HTTP {status} redirect")) && text.contains("final endpoint"))), "{kind} {status}: {:?}", snapshot.entries);
            assert!(
                r.requests.lock().iter().any(|p| p == path),
                "{kind} {status}"
            );
            assert_eq!(r.destination.load(Ordering::SeqCst), 0, "{kind} {status}");
        }
    }
}

#[tokio::test]
async fn discovery_props_and_window_reads_do_not_follow_redirects() {
    for (kind, path, owner) in [
        ("openai_chat", "/v1/models", "llamacpp"),
        ("openai_responses", "/v1/models", "llamacpp"),
        ("anthropic", "/v1/models", "llamacpp"),
        ("openai_chat", "/props", "llamacpp"),
        ("openai_chat", "/api/ps", "library"),
        ("openai_chat", "/api/show", "library"),
        ("openai_chat", "/api/v1/models", "organization_owner"),
        ("openai_chat", "/api/v0/models", "organization_owner"),
    ] {
        let r = rig(path, 307, owner, kind).await;
        r.engine
            .handle(Request::ListProviders { refresh: true }, None)
            .await
            .unwrap();
        let asked = r.requests.lock().clone();
        assert!(asked.iter().any(|p| p == path), "{kind} {path}: {asked:?}");
        assert_eq!(r.destination.load(Ordering::SeqCst), 0, "{kind} {path}");
    }
}
