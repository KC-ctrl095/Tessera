//! The production OpenRouter route has no endpoint override. Fixtures exercise
//! its shared HTTP implementation on loopback, with synthetic keys only.
use super::*;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Response, StatusCode, header},
    routing::post,
};
use std::sync::{Arc, Mutex};

type Fixture = Arc<Mutex<(StatusCode, &'static str, Vec<u8>, bool)>>;

async fn engine() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "provider = []\n").unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    (dir, engine)
}

#[test]
fn openrouter_admission_is_exact_and_audio_is_opt_in() {
    for base in [OPENROUTER_BASE, "https://openrouter.ai:443/api/v1/"] {
        assert!(is_openrouter(base));
    }
    for base in [
        "http://openrouter.ai/api/v1",
        "https://openrouter.ai:444/api/v1",
        "https://openrouter.ai.other/api/v1",
        "https://other/api/v1",
        "https://openrouter.ai/api/v1/other",
        "https://openrouter.ai/api/v1?key=test",
        "https://name@openrouter.ai/api/v1",
        "https://openrouter.ai/api/v1#fragment",
    ] {
        assert!(!is_openrouter(base), "{base}");
    }
    for engine in ["auto", "device", "server", "openai", "service", "off"] {
        assert!(listen_route(engine, Some("openrouter")).is_err());
        assert!(speak_route(engine, Some("openrouter")).is_err());
    }
    assert_eq!(listen_route("openrouter", None), Ok(Route::OpenRouter));
    assert_eq!(speak_route("openrouter", None), Ok(Route::OpenRouter));
    assert_eq!(
        listen_route("openrouter", Some("server")),
        Ok(Route::Server)
    );
    assert!(listen_route("openrouter", Some("openai")).is_err());
    assert_eq!(openrouter_voice("").unwrap(), "sarah");
    assert_eq!(openrouter_voice("george").unwrap(), "george");
    assert!(openrouter_voice("unknown").is_err());
}

#[tokio::test]
async fn openrouter_keys_reuse_only_canonical_providers_or_eligible_saved_keys() {
    let (_dir, engine) = engine().await;
    let inner = &engine.0;
    assert!(!inner.voice_config().openrouter_key);
    let mut config = inner.voice_config();
    config.engine = "openrouter".into();
    assert!(inner.save_voice(config.clone(), None).await.is_err());
    // A provider's name/id cannot make an unrelated address OpenRouter.
    for base in [
        "http://127.0.0.1/api/v1",
        "https://openrouter.ai.other/api/v1",
        OPENROUTER_BASE,
    ] {
        engine.handle(serde_json::from_value(serde_json::json!({
            "method":"save_provider", "params":{"spec":{
                "id":"openrouter", "name":"OpenRouter", "kind":"openai_chat",
                "base_url":base, "api_key":"fixture-router-key", "discover":false,"models":[]
            }}
        })).unwrap(), None).await.unwrap();
        assert_eq!(inner.voice_config().openrouter_key, base == OPENROUTER_BASE);
    }
    assert_eq!(inner.openrouter().as_deref(), Some("fixture-router-key"));
    inner.save_voice(config, None).await.unwrap();
    // Saved output contains capability, never the credential.
    assert!(
        !serde_json::to_string(&inner.voice_config())
            .unwrap()
            .contains("fixture-router-key")
    );
    assert!(
        inner
            .transcribe("AAAA", "audio/wav", None, true, None)
            .await
            .is_err()
    );
    inner.auth.remove("openrouter").unwrap();
    engine.handle(serde_json::from_value(serde_json::json!({
        "method":"save_secret", "params":{"name":"OPENROUTER_API_KEY", "fields":{"value":"fixture-saved-key"}}
    })).unwrap(), None).await.unwrap();
    assert_eq!(inner.openrouter().as_deref(), Some("fixture-saved-key"));
    assert!(inner.voice_ready());
}

async fn response(
    State(fixture): State<Fixture>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    assert_eq!(headers[header::AUTHORIZATION], "Bearer fixture-router-key");
    let kind = headers[header::CONTENT_TYPE].to_str().unwrap();
    if kind.starts_with("multipart/form-data") {
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains(OPENROUTER_TRANSCRIBE));
        assert!(body.contains("audio.wav"));
        assert!(body.contains("RIFF"));
    } else {
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["model"], OPENROUTER_SPEAK);
        assert_eq!(body["voice"], "sarah");
        assert_eq!(body["response_format"], "mp3");
        assert_eq!(body["input"], "Hello there.");
    }
    let (status, kind, bytes, chunked) = fixture.lock().unwrap().clone();
    let body = if chunked {
        Body::from_stream(futures::stream::iter(
            bytes
                .chunks(4096)
                .map(|b| Ok::<_, std::io::Error>(Bytes::copy_from_slice(b)))
                .collect::<Vec<_>>(),
        ))
    } else {
        Body::from(bytes)
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, kind)
        .header(header::LOCATION, "http://127.0.0.1:1/never-follow")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn fake_openrouter_handles_both_directions_and_bounded_failures() {
    let (_dir, engine) = engine().await;
    let inner = &engine.0;
    let fixture: Fixture = Arc::new(Mutex::new((
        StatusCode::OK,
        "application/json",
        vec![],
        false,
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/api/v1/audio/transcriptions", post(response))
        .route("/api/v1/audio/speech", post(response))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}/api/v1");
    let audio = crate::voice::wav::encode_pcm16(16000, &[100; 16000]);
    let stt = || {
        inner.transcribe_http(
            &base,
            Some("fixture-router-key"),
            OPENROUTER_TRANSCRIBE,
            "en",
            audio.clone(),
            "audio/wav",
        )
    };
    let tts = || {
        inner.speak_http(
            &base,
            "fixture-router-key",
            OPENROUTER_SPEAK,
            "sarah",
            "Hello there.",
        )
    };
    *fixture.lock().unwrap() = (
        StatusCode::OK,
        "application/json",
        br#"{"text":"Hello there."}"#.to_vec(),
        false,
    );
    assert_eq!(stt().await.unwrap(), "Hello there.");
    *fixture.lock().unwrap() = (
        StatusCode::OK,
        "audio/mpeg",
        b"ID3 fixture audio".to_vec(),
        true,
    );
    assert_eq!(tts().await.unwrap().mime, "audio/mpeg");
    for chunked in [false, true] {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::TEMPORARY_REDIRECT,
        ] {
            *fixture.lock().unwrap() = (
                status,
                "application/json",
                br#"{"error":{"message":"fixture-router-key"}}"#.to_vec(),
                chunked,
            );
            for error in [stt().await.unwrap_err(), tts().await.unwrap_err()] {
                assert_eq!(error.code, ErrorCode::Provider);
                assert!(!error.message.contains("fixture-router-key"));
            }
        }
        for (kind, bytes) in [
            ("application/json", br#"{"text":12}"#.to_vec()),
            ("text/plain", b"not JSON".to_vec()),
            ("application/json", br#"{"error":"wrong shape"}"#.to_vec()),
            ("application/json", vec![b'a'; TRANSCRIPT_BYTES + 1]),
        ] {
            *fixture.lock().unwrap() = (StatusCode::OK, kind, bytes, chunked);
            assert!(stt().await.is_err());
        }
        for (kind, bytes) in [
            ("audio/pcm", vec![0; 100]),
            ("audio/mpeg", br#"{"error":"not audio"}"#.to_vec()),
            ("audio/mpeg", vec![]),
            ("audio/mpeg", vec![0; SPEECH_BYTES + 1]),
        ] {
            *fixture.lock().unwrap() = (StatusCode::OK, kind, bytes, chunked);
            assert!(tts().await.is_err());
        }
    }
    server.abort();
}

#[tokio::test]
async fn cancelling_cloud_work_drops_pending_response_and_keeps_engine_usable() {
    let (_dir, engine) = engine().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/api/v1/audio/speech",
        post(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            ([(header::CONTENT_TYPE, "audio/mpeg")], "ID3 delayed")
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}/api/v1");
    let work = engine.0.speak_http(
        &base,
        "fixture-router-key",
        OPENROUTER_SPEAK,
        "sarah",
        "Hello there.",
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(80), work)
            .await
            .is_err()
    );
    assert_eq!(engine.0.voice_config().engine, "auto");
    server.abort();
}
