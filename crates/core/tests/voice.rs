//! Voice through the engine: where speech may go for each choice, OpenAI
//! with the user's own key, the server's own engines (fake ones here, in the
//! places downloads go), the check after a download, and kept recordings.

mod common;

use base64::Engine as _;
use common::*;
use serde_json::json;
use tessera_proto::{
    ErrorCode, Request, ServerInfo, SetupStatus, SpeechAudio, Transcript, VoiceConfig,
};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn get(h: &Harness) -> VoiceConfig {
    serde_json::from_value(h.engine.handle(Request::GetVoice, None).await.unwrap()).unwrap()
}

async fn save(
    h: &Harness,
    f: impl FnOnce(&mut VoiceConfig),
) -> Result<VoiceConfig, tessera_proto::ApiError> {
    let mut c = get(h).await;
    f(&mut c);
    h.engine
        .handle(
            Request::SaveVoice {
                config: c,
                api_key: None,
            },
            None,
        )
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

async fn info(h: &Harness) -> ServerInfo {
    serde_json::from_value(
        h.engine
            .handle(
                Request::Hello {
                    client: "test".into(),
                    version: "0".into(),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn transcribe(
    h: &Harness,
    audio: &[u8],
    engine: Option<&str>,
    partial: bool,
) -> Result<Transcript, tessera_proto::ApiError> {
    h.engine
        .handle(
            Request::Transcribe {
                audio: b64(audio),
                mime: "audio/wav".into(),
                engine: engine.map(str::to_string),
                language: None,
                operation: None,
                partial,
            },
            None,
        )
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

async fn speak(
    h: &Harness,
    text: &str,
    engine: Option<&str>,
) -> Result<SpeechAudio, tessera_proto::ApiError> {
    h.engine
        .handle(
            Request::Speak {
                text: text.into(),
                engine: engine.map(str::to_string),
                operation: None,
            },
            None,
        )
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

fn speech_wav(secs: f64) -> Vec<u8> {
    tessera_core::voice::wav::encode_pcm16(16_000, &vec![300i16; (16_000.0 * secs) as usize])
}

#[tokio::test]
async fn voice_starts_private_and_off_means_off() {
    let h = harness(vec![], 100_000).await;
    let c = get(&h).await;
    assert_eq!(c.engine, "auto");
    assert!(!c.openai_key && !c.keep_audio && c.kept == 0);
    let server = c.server.expect("the server's engines are described");
    assert!(
        matches!(server.state.as_str(), "missing" | "unsupported"),
        "{}",
        server.state
    );
    assert!(!server.listen && !server.speak);
    // Nothing downloaded yet: older apps don't show a microphone.
    let i = info(&h).await;
    assert!(!i.voice);
    assert_eq!(i.voice_engine.as_deref(), Some("auto"));

    let e = transcribe(&h, &speech_wav(1.0), None, false)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{}", e.message);
    // Asking for a cloud service the user didn't choose is refused, unsent.
    let e = transcribe(&h, &speech_wav(1.0), Some("openai"), false)
        .await
        .unwrap_err();
    assert!(e.message.contains("OpenAI"), "{}", e.message);
    let e = transcribe(&h, &speech_wav(1.0), Some("service"), false)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert!(h.mock.state.transcriptions.lock().is_empty());
    assert!(speak(&h, "Hello.", Some("openai")).await.is_err());
    assert!(h.mock.state.speeches.lock().is_empty());

    // OpenAI needs a key first.
    let e = save(&h, |c| c.engine = "openai".into()).await.unwrap_err();
    assert!(e.message.contains("Passwords & keys"), "{}", e.message);
    assert!(save(&h, |c| c.engine = "loud".into()).await.is_err());
    assert!(save(&h, |c| c.server_model = "huge".into()).await.is_err());
    assert!(
        save(&h, |c| c.server_voice = "../voice".into())
            .await
            .is_err()
    );

    let c = save(&h, |c| c.engine = "off".into()).await.unwrap();
    assert_eq!(c.engine, "off");
    let e = transcribe(&h, &speech_wav(1.0), None, false)
        .await
        .unwrap_err();
    assert!(e.message.contains("Voice is off"), "{}", e.message);
    assert!(
        speak(&h, "Hello.", None)
            .await
            .unwrap_err()
            .message
            .contains("Voice is off")
    );
    let i = info(&h).await;
    assert!(!i.voice && i.voice_engine.as_deref() == Some("off"));
    let s: SetupStatus =
        serde_json::from_value(h.engine.handle(Request::SetupStatus, None).await.unwrap()).unwrap();
    assert!(!s.voice);
    assert!(h.engine.handle(Request::InstallVoice, None).await.is_err());

    // Each device on its own: the server handles no speech.
    save(&h, |c| c.engine = "device".into()).await.unwrap();
    let e = transcribe(&h, &speech_wav(1.0), Some("server"), false)
        .await
        .unwrap_err();
    assert!(e.message.contains("each device"), "{}", e.message);
}

#[tokio::test]
async fn an_app_from_before_the_choice_keeps_its_transcription_service() {
    let h = harness(vec![], 100_000).await;
    // What an older app saves: no engine, the service on.
    let old = json!({
        "enabled": true, "provider": "custom", "base_url": h.mock.url, "model": "whisper-1",
        "language": "", "has_key": false
    });
    let saved: VoiceConfig = serde_json::from_value(
        h.engine
            .handle(
                serde_json::from_value(json!({"method": "save_voice", "params": {"config": old, "api_key": "svc-key"}})).unwrap(),
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.engine, "service");
    assert!(info(&h).await.voice);
    // An older app's request (no engine, no partial).
    let req: Request = serde_json::from_value(json!({"method": "transcribe", "params": {"audio": b64(b"RIFFxxxxWAVE"), "mime": "audio/webm"}})).unwrap();
    let t: Transcript = serde_json::from_value(h.engine.handle(req, None).await.unwrap()).unwrap();
    assert_eq!(
        (t.text.as_str(), t.engine.as_str()),
        ("hello from the mock", "service")
    );
    assert_eq!(
        h.mock.state.transcriptions.lock()[0].0.as_deref(),
        Some("Bearer svc-key")
    );
    // Words so far are never asked of a paid service.
    let e = transcribe(&h, &speech_wav(1.0), None, true)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    assert_eq!(h.mock.state.transcriptions.lock().len(), 1);
    // Turning it off in that app goes back to the private choice.
    let off = json!({"enabled": false, "provider": "custom", "base_url": h.mock.url, "model": "whisper-1", "language": "", "has_key": true});
    let saved: VoiceConfig = serde_json::from_value(
        h.engine
            .handle(
                serde_json::from_value(json!({"method": "save_voice", "params": {"config": off}}))
                    .unwrap(),
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.engine, "auto");
    assert!(saved.has_key, "the key stays until replaced");
}

#[tokio::test]
async fn openai_gets_speech_only_when_chosen_and_with_the_users_key() {
    let h = harness(vec![], 100_000).await;
    let spec = json!({"id": "openai", "name": "OpenAI", "kind": "openai_chat", "base_url": h.mock.url,
        "api_key": "sk-voice-test-123", "discover": false, "models": []});
    h.engine
        .handle(
            serde_json::from_value(json!({"method": "save_provider", "params": {"spec": spec}}))
                .unwrap(),
            None,
        )
        .await
        .unwrap();
    assert!(get(&h).await.openai_key);
    // Still the private choice: refused, nothing sent.
    assert!(
        transcribe(&h, &speech_wav(1.0), Some("openai"), false)
            .await
            .is_err()
    );
    assert!(h.mock.state.transcriptions.lock().is_empty());

    let c = save(&h, |c| c.engine = "openai".into()).await.unwrap();
    assert_eq!(c.engine, "openai");
    assert!(info(&h).await.voice);
    let t = transcribe(&h, &speech_wav(1.5), None, false).await.unwrap();
    assert_eq!(
        (t.text.as_str(), t.engine.as_str()),
        ("hello from the mock", "openai")
    );
    {
        let seen = h.mock.state.transcriptions.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.as_deref(), Some("Bearer sk-voice-test-123"));
        assert!(seen[0].1.contains("gpt-4o-mini-transcribe") && seen[0].1.contains("audio.wav"));
    }
    assert_eq!(
        transcribe(&h, &speech_wav(1.0), None, true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        h.mock.state.transcriptions.lock().len(),
        1,
        "no paid partial requests"
    );

    let a = speak(&h, "Hello there. How are you?", None).await.unwrap();
    assert_eq!(a.mime, "audio/mpeg");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&a.audio)
            .unwrap(),
        b"ID3 pretend mp3"
    );
    {
        let seen = h.mock.state.speeches.lock();
        assert_eq!(seen[0].0.as_deref(), Some("Bearer sk-voice-test-123"));
        assert_eq!(seen[0].1["model"], "gpt-4o-mini-tts");
        assert_eq!(seen[0].1["input"], "Hello there. How are you?");
    }
    let too_long = "a".repeat(4_001);
    assert!(speak(&h, &too_long, None).await.is_err());
    let shown = h
        .engine
        .handle(Request::GetVoice, None)
        .await
        .unwrap()
        .to_string();
    assert!(
        !shown.contains("sk-voice-test-123"),
        "the key never reaches a device"
    );
}

#[tokio::test]
async fn a_key_in_passwords_and_keys_counts_as_the_users_openai_key() {
    let h = harness(vec![], 100_000).await;
    let req = json!({"method": "save_secret", "params": {"name": "OPENAI_API_KEY", "fields": {"value": "sk-from-passwords"}}});
    h.engine
        .handle(serde_json::from_value(req).unwrap(), None)
        .await
        .unwrap();
    assert!(get(&h).await.openai_key);
    assert_eq!(
        save(&h, |c| c.engine = "openai".into())
            .await
            .unwrap()
            .engine,
        "openai"
    );
}

/// Fake engines where the downloads would be: whisper prints what it was
/// given, piper copies a second of sound.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn fake_engines(home: &std::path::Path, whisper_body: &str) {
    use std::os::unix::fs::PermissionsExt;
    use tessera_core::voice::catalog;
    let dir = home.join("voice");
    let w = catalog::whisper().unwrap();
    let p = catalog::piper().unwrap();
    for (sub, bin, body) in [
        (w.dir, w.bin, whisper_body.to_string()),
        (p.dir, p.bin, "#!/bin/sh\nread line\nout=''\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output_file ] && out=\"$2\"; shift; done\ncp \"$(dirname \"$0\")/sample.wav\" \"$out\"\n".to_string()),
    ] {
        let d = dir.join(sub);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(bin), body).unwrap();
        std::fs::set_permissions(d.join(bin), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        dir.join(p.dir).join("sample.wav"),
        tessera_core::voice::wav::encode_pcm16(22_050, &[200; 22_050]),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("models")).unwrap();
    for m in ["tiny", "base"] {
        std::fs::write(
            dir.join("models").join(catalog::model(m).unwrap().0),
            b"model",
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("voices")).unwrap();
    std::fs::write(dir.join("voices/en_US-joe-medium.onnx"), b"voice").unwrap();
    std::fs::write(dir.join("voices/en_US-joe-medium.onnx.json"), b"{}").unwrap();
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn the_servers_own_engines_listen_speak_and_keep_nothing_unless_asked() {
    let h = harness(vec![], 100_000).await;
    fake_engines(
        h.dir.path(),
        "#!/bin/sh\necho ' [BLANK_AUDIO]'\necho ' Turn on the lights.'\n",
    );
    let c = get(&h).await;
    let s = c.server.unwrap();
    assert_eq!(
        (s.state.as_str(), s.listen, s.speak, s.download_bytes),
        ("ready", true, true, 0)
    );
    assert!(info(&h).await.voice, "older apps show their microphone now");

    let t = transcribe(&h, &speech_wav(2.0), None, false).await.unwrap();
    assert_eq!(
        (t.text.as_str(), t.engine.as_str()),
        ("Turn on the lights.", "server")
    );
    let t = transcribe(&h, &speech_wav(1.0), Some("server"), true)
        .await
        .unwrap();
    assert_eq!(t.text, "Turn on the lights.");
    assert_eq!(get(&h).await.kept, 0, "nothing kept by default");
    assert!(!h.dir.path().join("voice/recordings").exists());

    let a = speak(&h, "The lights are on.", None).await.unwrap();
    assert_eq!(a.mime, "audio/wav");
    assert_eq!(a.ms, 1000);
    assert!(
        tessera_core::voice::wav::info(
            &base64::engine::general_purpose::STANDARD
                .decode(&a.audio)
                .unwrap()
        )
        .is_some()
    );

    // Keeping recordings: final ones only, deleted with one request.
    let c = save(&h, |c| c.keep_audio = true).await.unwrap();
    assert!(c.keep_audio);
    transcribe(&h, &speech_wav(1.0), None, true).await.unwrap();
    assert_eq!(get(&h).await.kept, 0, "words so far aren't kept");
    transcribe(&h, &speech_wav(1.0), None, false).await.unwrap();
    let c = get(&h).await;
    assert_eq!(c.kept, 1);
    assert_eq!(c.kept_bytes, speech_wav(1.0).len() as u64);
    let c: VoiceConfig = serde_json::from_value(
        h.engine
            .handle(Request::DeleteVoiceRecordings, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!((c.kept, c.kept_bytes), (0, 0));

    // Nothing left to download: "install" only checks that both run.
    let c: VoiceConfig =
        serde_json::from_value(h.engine.handle(Request::InstallVoice, None).await.unwrap())
            .unwrap();
    assert!(matches!(
        c.server.unwrap().state.as_str(),
        "installing" | "ready"
    ));
    let mut state = String::new();
    for _ in 0..100 {
        state = get(&h).await.server.unwrap().state;
        if state != "installing" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(state, "ready");
    // Another voice is offered, with its download size.
    let nl = get(&h)
        .await
        .server
        .unwrap()
        .voices
        .into_iter()
        .find(|v| v.language == "nl")
        .unwrap();
    assert!(!nl.installed && nl.size_bytes > 70_000_000);
    let c = save(&h, |c| c.server_voice = nl.id.clone()).await.unwrap();
    let s = c.server.unwrap();
    assert_eq!(
        (s.state.as_str(), s.speak, s.voice.as_str()),
        ("missing", false, nl.id.as_str())
    );
    assert_eq!(s.download_bytes, nl.size_bytes);
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn an_engine_that_cant_run_here_is_reported_after_the_check() {
    let h = harness(vec![], 100_000).await;
    fake_engines(
        h.dir.path(),
        "#!/bin/sh\necho 'Illegal instruction' >&2\nexit 132\n",
    );
    h.engine.handle(Request::InstallVoice, None).await.unwrap();
    let mut s = get(&h).await.server.unwrap();
    for _ in 0..100 {
        if s.state != "installing" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        s = get(&h).await.server.unwrap();
    }
    assert_eq!(s.state, "failed");
    let e = s.error.unwrap();
    assert!(
        e.contains("speech recognition doesn't run on this computer")
            && e.contains("Illegal instruction"),
        "{e}"
    );
    let e = transcribe(&h, &speech_wav(1.0), None, false)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Provider);
}

/// The service deliberately echoes the exact request credential on both routes.
/// Only synthetic keys are used, in throwaway config/vault and a loopback fixture.
#[tokio::test]
async fn speech_errors_scrub_echoed_service_and_openai_keys_and_transport_userinfo() {
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    };
    let status = Arc::new(AtomicU16::new(500));
    async fn echo_key(
        State(status): State<Arc<AtomicU16>>,
        headers: HeaderMap,
    ) -> (StatusCode, String) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let key = auth.strip_prefix("Bearer ").unwrap_or(auth);
        let code = status.load(Ordering::SeqCst);
        let body = if code == 502 {
            format!("{}{key}", "x".repeat(585))
        } else {
            json!({"error": {"message": format!("Invalid credential: {key}; encoded {}; URL http://localhost/v1?api_key=fixture-query-other&token=fixture-another-query", b64(key.as_bytes()))}}).to_string()
        };
        (StatusCode::from_u16(code).unwrap(), body)
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fixture_status = status.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/audio/transcriptions", post(echo_key))
                .route("/v1/audio/speech", post(echo_key))
                .with_state(fixture_status),
        )
        .await
        .unwrap();
    });
    let h = harness(vec![], 100_000).await;
    let service = "fixture-service-credential-echo";
    let mut config = get(&h).await;
    config.engine = "service".into();
    config.enabled = true;
    config.base_url = format!("http://{addr}/v1");
    config.model = "fake".into();
    h.engine
        .handle(
            Request::SaveVoice {
                config,
                api_key: Some(service.into()),
            },
            None,
        )
        .await
        .unwrap();
    let e = transcribe(&h, &speech_wav(1.0), None, false)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Provider);
    for value in [service.to_string(), b64(service.as_bytes())] {
        assert!(!e.message.contains(&value), "{e:?}");
    }
    assert!(
        !e.message.contains("fixture-query-other") && !e.message.contains("fixture-another-query")
    );
    assert!(e.message.contains("[REDACTED]"));
    status.store(502, Ordering::SeqCst);
    let error = transcribe(&h, &speech_wav(1.0), None, false)
        .await
        .unwrap_err();
    assert!(!error.message.contains("fixture-"), "{error:?}");
    assert!(error.message.contains("[REDACTED]"));
    for code in [401, 403] {
        status.store(code, Ordering::SeqCst);
        let error = transcribe(&h, &speech_wav(1.0), None, false)
            .await
            .unwrap_err();
        assert_eq!(
            error.message,
            "Speech authentication failed. Check the saved API key."
        );
    }
    status.store(500, Ordering::SeqCst);
    let key = "fixture-openai-credential-echo";
    let provider = |url: String| {
        serde_json::from_value(json!({"method": "save_provider", "params": {"spec": {"id": "openai", "name": "OpenAI fixture", "kind": "openai_chat", "base_url": url, "api_key": key, "discover": false, "models": []}}})).unwrap()
    };
    h.engine
        .handle(provider(format!("http://{addr}/v1")), None)
        .await
        .unwrap();
    save(&h, |c| c.engine = "openai".into()).await.unwrap();
    for request in [
        Request::Transcribe {
            audio: b64(&speech_wav(1.0)),
            mime: "audio/wav".into(),
            engine: None,
            language: None,
            operation: None,
            partial: false,
        },
        Request::Speak {
            text: "test".into(),
            engine: None,
            operation: None,
        },
    ] {
        let e = h.engine.handle(request, None).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Provider);
        for value in [key.to_string(), b64(key.as_bytes())] {
            assert!(!e.message.contains(&value), "{e:?}");
        }
        assert!(
            !e.message.contains("fixture-query-other")
                && !e.message.contains("fixture-another-query")
        );
        assert!(e.message.contains("[REDACTED]"));
    }
    status.store(502, Ordering::SeqCst);
    for error in [
        transcribe(&h, &speech_wav(1.0), None, false)
            .await
            .unwrap_err(),
        speak(&h, "test", None).await.unwrap_err(),
    ] {
        assert!(!error.message.contains("fixture-"), "{error:?}");
        assert!(error.message.contains("[REDACTED]"));
    }
    for code in [401, 403] {
        status.store(code, Ordering::SeqCst);
        for error in [
            transcribe(&h, &speech_wav(1.0), None, false)
                .await
                .unwrap_err(),
            speak(&h, "test", None).await.unwrap_err(),
        ] {
            assert_eq!(
                error.message,
                "Speech authentication failed. Check the saved API key."
            );
        }
    }
    server.abort();
    // A refused connection includes its URL in reqwest's error string.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    h.engine
        .handle(
            provider(format!(
                "http://user:fixture-password@{dead_addr}/v1?token={key}"
            )),
            None,
        )
        .await
        .unwrap();
    let e = speak(&h, "test", None).await.unwrap_err();
    for value in [key, "fixture-password"] {
        assert!(!e.message.contains(value), "{e:?}");
    }
}

#[tokio::test]
async fn speech_addresses_and_success_bodies_cannot_be_credential_exits() {
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, HeaderValue, header},
        routing::post,
    };
    use std::sync::{Arc, Mutex};
    type Body = Arc<Mutex<(String, String)>>;
    async fn response(State(body): State<Body>) -> (HeaderMap, String) {
        let (kind, text) = body.lock().unwrap().clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&kind).unwrap());
        (headers, text)
    }
    let body: Body = Arc::new(Mutex::new((
        "application/json".into(),
        json!({"error":{"message":"fixture-bearer-secret"}}).to_string(),
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fixture = body.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/audio/transcriptions", post(response))
                .route("/v1/audio/speech", post(response))
                .with_state(fixture),
        )
        .await
        .unwrap();
    });
    let h = harness(vec![], 100_000).await;
    let key = "fixture-bearer-secret";
    for address in [
        format!("http://{addr}/v1?api_key=fixture-other-query-secret"),
        format!("http://user:fixture-password@{addr}/v1"),
        format!("http://{addr}/v1?%74oken=fixture-encoded-query-secret"),
    ] {
        let error = save(&h, |c| {
            c.base_url = address.clone();
            c.enabled = true;
            c.engine = "service".into();
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(!error.message.contains("fixture-"));
        assert!(!get(&h).await.base_url.contains("fixture-"));
    }
    let mut config = get(&h).await;
    config.engine = "service".into();
    config.enabled = true;
    config.base_url = format!("http://{addr}/v1");
    config.model = "fixture".into();
    h.engine
        .handle(
            Request::SaveVoice {
                config,
                api_key: Some(key.into()),
            },
            None,
        )
        .await
        .unwrap();
    // A 200 nested error, plain text, wrong JSON shape or content type must never become a draft.
    for (kind, text) in [
        (
            "application/json",
            json!({"error":{"message":key}}).to_string(),
        ),
        ("text/plain", key.into()),
        ("application/json", json!({"text":12}).to_string()),
        ("text/html", json!({"text":key}).to_string()),
    ] {
        *body.lock().unwrap() = (kind.into(), text);
        let error = transcribe(&h, &speech_wav(1.0), None, false)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Provider);
        assert!(!error.message.contains(key));
    }
    *body.lock().unwrap() = (
        "application/json; charset=utf-8".into(),
        json!({"text":format!("hello {key}")}).to_string(),
    );
    assert_eq!(
        transcribe(&h, &speech_wav(1.0), None, false)
            .await
            .unwrap()
            .text,
        "hello [REDACTED]"
    );
    let request = serde_json::from_value(json!({"method":"save_provider","params":{"spec":{"id":"openai","name":"fixture","kind":"openai_chat","base_url":format!("http://{addr}/v1"),"api_key":key,"discover":false,"models":[]}}})).unwrap();
    h.engine.handle(request, None).await.unwrap();
    save(&h, |c| c.engine = "openai".into()).await.unwrap();
    for kind in ["application/json", "text/plain", "audio/mpeg"] {
        *body.lock().unwrap() = (kind.into(), json!({"error":{"message":key}}).to_string());
        let error = speak(&h, "test", None).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Provider);
        assert!(!error.message.contains(key));
    }
    server.abort();
}

#[tokio::test]
async fn speech_requires_https_at_save_and_again_for_reused_provider_dispatch() {
    let h = harness(vec![], 100_000).await;
    for address in [
        "http://speech.example/v1",
        "http://8.8.8.8/v1",
        "http://[2001:db8::1]/v1",
    ] {
        let error = save(&h, |c| {
            c.engine = "service".into();
            c.enabled = true;
            c.base_url = address.into();
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("HTTPS"));
    }
    // Literal local IPs need no DNS or network at save; no request is sent here.
    for address in [
        "http://127.0.0.2/v1",
        "http://192.168.1.2/v1",
        "http://[::1]/v1",
        "http://[fd00::1]/v1",
        "http://[fe80::1]/v1",
    ] {
        save(&h, |c| {
            c.engine = "service".into();
            c.enabled = true;
            c.base_url = address.into();
        })
        .await
        .unwrap();
    }
    let provider = |base: &str| {
        serde_json::from_value(json!({"method":"save_provider","params":{"spec":{"id":"openai","name":"fixture","kind":"openai_chat","base_url":base,"api_key":"fixture-https-key","discover":false,"models":[]}}})).unwrap()
    };
    h.engine.handle(provider(&h.mock.url), None).await.unwrap();
    save(&h, |c| {
        c.base_url.clear();
        c.enabled = false;
        c.engine = "openai".into();
    })
    .await
    .unwrap();
    // A model-provider address may be changed independently after voice selection.
    // Both STT and TTS must refuse it before any connection or credential dispatch.
    h.engine
        .handle(provider("http://speech.example/v1"), None)
        .await
        .unwrap();
    for error in [
        transcribe(&h, &speech_wav(1.0), None, false)
            .await
            .unwrap_err(),
        speak(&h, "test", None).await.unwrap_err(),
    ] {
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("HTTPS"));
        assert!(!error.message.contains("fixture-https-key"));
    }
    assert!(
        save(&h, |c| c.engine = "openai".into())
            .await
            .unwrap_err()
            .message
            .contains("HTTPS")
    );
    assert!(h.mock.state.transcriptions.lock().is_empty());
    assert!(h.mock.state.speeches.lock().is_empty());
}

#[tokio::test]
async fn speech_redirects_never_forward_audio_text_or_credentials() {
    use axum::{
        Router,
        http::{StatusCode, header},
        routing::post,
    };
    let h = harness(vec![], 100_000).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let target = h.mock.url.clone();
    let app = Router::new()
        .route(
            "/v1/audio/transcriptions",
            post({
                let target = target.clone();
                move || async move {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(header::LOCATION, format!("{target}/audio/transcriptions"))],
                    )
                }
            }),
        )
        .route(
            "/v1/audio/speech",
            post(move || async move {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(header::LOCATION, format!("{target}/audio/speech"))],
                )
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}/v1");
    save(&h, |c| {
        c.engine = "service".into();
        c.enabled = true;
        c.base_url = base.clone();
    })
    .await
    .unwrap();
    assert!(transcribe(&h, &speech_wav(1.0), None, false).await.is_err());
    let provider = serde_json::from_value(json!({"method":"save_provider","params":{"spec":{"id":"openai","name":"fixture","kind":"openai_chat","base_url":base,"api_key":"fixture-redirect-key","discover":false,"models":[]}}})).unwrap();
    h.engine.handle(provider, None).await.unwrap();
    save(&h, |c| c.engine = "openai".into()).await.unwrap();
    assert!(transcribe(&h, &speech_wav(1.0), None, false).await.is_err());
    assert!(speak(&h, "test", None).await.is_err());
    assert!(
        h.mock.state.transcriptions.lock().is_empty(),
        "redirect must not send audio/key"
    );
    assert!(
        h.mock.state.speeches.lock().is_empty(),
        "redirect must not send text/key"
    );
    server.abort();
}

#[tokio::test]
async fn oversized_speech_success_and_errors_are_capped_even_when_chunked() {
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{Response, StatusCode, header},
        routing::post,
    };
    use std::sync::{Arc, Mutex};
    type Fixture = Arc<Mutex<(StatusCode, &'static str, Bytes, bool)>>;
    async fn response(State(fixture): State<Fixture>) -> Response<Body> {
        let (status, kind, bytes, chunked) = fixture.lock().unwrap().clone();
        let body = if chunked {
            let chunks: Vec<_> = (0..bytes.len())
                .step_by(4096)
                .map(|start| {
                    Ok::<_, std::io::Error>(bytes.slice(start..(start + 4096).min(bytes.len())))
                })
                .collect();
            Body::from_stream(futures::stream::iter(chunks))
        } else {
            Body::from(bytes)
        };
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, kind)
            .body(body)
            .unwrap()
    }
    let fixture: Fixture = Arc::new(Mutex::new((
        StatusCode::OK,
        "application/json",
        Bytes::new(),
        false,
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/audio/transcriptions", post(response))
        .route("/v1/audio/speech", post(response))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let h = harness(vec![], 100_000).await;
    let provider = serde_json::from_value(json!({"method":"save_provider","params":{"spec":{"id":"openai","name":"fixture","kind":"openai_chat","base_url":format!("http://{addr}/v1"),"api_key":"fixture-response-key","discover":false,"models":[]}}})).unwrap();
    h.engine.handle(provider, None).await.unwrap();
    save(&h, |c| c.engine = "openai".into()).await.unwrap();
    for chunked in [false, true] {
        for success in [false, true] {
            for tts in [false, true] {
                let (status, kind, body) = if !success {
                    (StatusCode::BAD_GATEWAY, "application/json", json!({"error":{"message":format!("fixture-response-key{}", "x".repeat(16 * 1024))}}).to_string())
                } else if tts {
                    (
                        StatusCode::OK,
                        "audio/mpeg",
                        format!("ID3{}", "x".repeat(16 * 1024 * 1024)),
                    )
                } else {
                    (
                        StatusCode::OK,
                        "application/json",
                        json!({"text":"x".repeat(64 * 1024)}).to_string(),
                    )
                };
                *fixture.lock().unwrap() = (status, kind, Bytes::from(body), chunked);
                let error = if tts {
                    speak(&h, "test", None).await.unwrap_err()
                } else {
                    transcribe(&h, &speech_wav(1.0), None, false)
                        .await
                        .unwrap_err()
                };
                assert_eq!(error.code, ErrorCode::Provider);
                assert_eq!(
                    error.message, "Speech service returned a response larger than allowed.",
                    "chunked={chunked} success={success} tts={tts}"
                );
            }
        }
    }
    server.abort();
}

#[tokio::test]
async fn inactive_unresolvable_saved_service_does_not_block_other_engines() {
    use tessera_core::{Engine, EngineOptions, config::Paths};
    let mut h = harness(vec![], 100_000).await;
    // Simulate a previously saved LAN service that is no longer reachable.
    // Only this throwaway config is changed; no DNS/hosts configuration needed.
    let address = format!(
        "http://tessera-unavailable-{:016x}.local/v1",
        rand::random::<u64>()
    );
    let paths = Paths::at(h.dir.path());
    let mut config = h.engine.config();
    config.voice.engine = "service".into();
    config.voice.enabled = true;
    config.voice.base_url = address.clone();
    config.voice.model = "fixture".into();
    config.save(&paths).unwrap();
    h.engine = Engine::open(paths, EngineOptions { scheduler: false })
        .await
        .unwrap();
    assert!(
        transcribe(&h, &speech_wav(1.0), None, false)
            .await
            .unwrap_err()
            .message
            .contains("resolve")
    );
    let provider = serde_json::from_value(json!({"method":"save_provider","params":{"spec":{"id":"openai","name":"fixture","kind":"openai_chat","base_url":h.mock.url,"api_key":"fixture-inactive-key","discover":false,"models":[]}}})).unwrap();
    h.engine.handle(provider, None).await.unwrap();
    for engine in ["off", "auto", "server", "device", "openai"] {
        let saved = save(&h, |c| c.engine = engine.into()).await.unwrap();
        assert_eq!(saved.engine, engine);
        assert_eq!(saved.base_url, address);
        assert_eq!(h.engine.config().voice.base_url, address);
    }
    assert!(h.mock.state.transcriptions.lock().is_empty());
    assert!(h.mock.state.speeches.lock().is_empty());
    // The inactive service does not prevent the chosen OpenAI loopback route.
    assert!(transcribe(&h, &speech_wav(1.0), None, false).await.is_ok());
    assert!(speak(&h, "fixture", None).await.is_ok());
    // Selecting that unavailable service must still validate it.
    assert!(
        save(&h, |c| c.engine = "service".into())
            .await
            .unwrap_err()
            .message
            .contains("resolve")
    );
    // Changing an address must still enforce the transport rule even when off.
    assert!(
        save(&h, |c| {
            c.engine = "off".into();
            c.base_url = "http://speech.example/v1".into();
        })
        .await
        .unwrap_err()
        .message
        .contains("HTTPS")
    );
    assert_eq!(get(&h).await.engine, "openai");
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn device_language_hint_saves_detection_without_overriding_shared_language() {
    let h = harness(vec![], 100_000).await;
    fake_engines(
        h.dir.path(),
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -l ]; then echo \"language $2\"; exit 0; fi; shift; done\n",
    );
    for (hint, saved, expected) in [
        (Some("en-US"), "", "en"),
        (Some("nl-NL"), "de", "de"),
        (None, "", "auto"),
    ] {
        save(&h, |c| c.language = saved.into()).await.unwrap();
        let result = h
            .engine
            .handle(
                Request::Transcribe {
                    audio: b64(&speech_wav(1.0)),
                    mime: "audio/wav".into(),
                    engine: Some("server".into()),
                    language: hint.map(str::to_string),
                    operation: None,
                    partial: false,
                },
                None,
            )
            .await
            .unwrap();
        let transcript: Transcript = serde_json::from_value(result).unwrap();
        assert_eq!(transcript.text, format!("language {expected}"));
        assert_eq!(get(&h).await.language, saved, "the hint is not persisted");
    }
    for hint in ["en?key=bad", "en\nother", "a-very-long-language-code"] {
        let error = h
            .engine
            .handle(
                Request::Transcribe {
                    audio: b64(&speech_wav(1.0)),
                    mime: "audio/wav".into(),
                    engine: None,
                    language: Some(hint.into()),
                    operation: None,
                    partial: false,
                },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
    }
}
