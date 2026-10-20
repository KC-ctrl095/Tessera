//! Session option authorization without any model backend.
use tessera_core::{Engine, EngineOptions, config::Paths};
use tessera_proto::*;

async fn harness() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
default_model = "mock/m"
[agent]
review_interval = 0
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "http://127.0.0.1:18769/v1"
discover = false
[[provider.models]]
id = "m"
context_window = 100000
max_output = 2000
"#,
    )
    .unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    (dir, engine)
}

async fn session(engine: &Engine, private: bool, device: Option<&str>) -> SessionInfo {
    serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: None,
                    model: None,
                    title: None,
                    bot_id: None,
                    private,
                    lean: false,
                },
                device,
            )
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn daybreak_cannot_be_enabled_without_discovered_openai_access() {
    let (_dir, engine) = harness().await;
    let session = session(&engine, false, None).await;
    assert!(!session.daybreak);
    let error = engine
        .handle(
            Request::SetSessionDaybreak {
                session_id: session.id.clone(),
                enabled: true,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("not confirmed Daybreak Blue access"));
    let off = engine
        .handle(
            Request::SetSessionDaybreak {
                session_id: session.id.clone(),
                enabled: false,
            },
            None,
        )
        .await
        .unwrap();
    assert!(!serde_json::from_value::<SessionInfo>(off).unwrap().daybreak);
}

#[tokio::test]
async fn daybreak_request_respects_private_chat_device_ownership() {
    let (_dir, engine) = harness().await;
    let session = session(&engine, true, Some("owner-device")).await;
    let denied = engine
        .handle(
            Request::SetSessionDaybreak {
                session_id: session.id.clone(),
                enabled: false,
            },
            Some("other-device"),
        )
        .await;
    assert!(denied.is_err());
    let accepted = engine
        .handle(
            Request::SetSessionDaybreak {
                session_id: session.id,
                enabled: false,
            },
            Some("owner-device"),
        )
        .await;
    assert!(accepted.is_ok());
}
