//! Agents' messages and shared files over real (encrypted) connections:
//! notifications reach only the devices that take them, and a file attached
//! on one device downloads on another.

use std::time::Duration;

use base64::Engine as _;
use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    // A permanent fixture rejection ends turns immediately, unlike a closed
    // port which correctly exercises the provider's unattended retry budget.
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model.local_addr().unwrap();
    tokio::spawn(async move {
        let mock = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    "fixture model rejection",
                )
            }),
        );
        axum::serve(model, mock).await.unwrap();
    });
    std::fs::write(
        dir.path().join("config.toml"),
        format!("default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://{model_addr}/v1\"\n[[provider.models]]\nid = \"m\"\n"),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    (engine, format!("http://{addr}"), dir)
}

/// A paired device's encrypted connection: its id and client.
async fn device(engine: &tessera_core::Engine, url: &str, name: &str) -> (String, Client) {
    let (id, token) = engine.store().add_device(name).unwrap();
    (id, connect_with_token(engine, url, token).await)
}

async fn connect_with_token(engine: &tessera_core::Engine, url: &str, token: String) -> Client {
    let key = tessera_server::load_identity(engine.paths()).unwrap();
    let client = Client::connect(ServerConfig {
        endpoints: vec![url.to_string()],
        token,
        server_key: tessera_secure::b64(key.public()),
        ..Default::default()
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    assert!(client.status().encrypted);
    client
}

/// Notifications this client received within `wait`.
async fn notifications(
    rx: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    wait: Duration,
) -> Vec<UserNotification> {
    let mut out = Vec::new();
    let end = tokio::time::Instant::now() + wait;
    while let Ok(Ok(e)) = tokio::time::timeout_at(end, rx.recv()).await {
        if let ClientEvent::Event(Event::Notification { notification }) = e {
            out.push(notification);
        }
    }
    out
}

#[tokio::test]
async fn notifications_reach_only_the_devices_that_take_them() {
    let (engine, url, _dir) = start().await;
    let (_phone_id, phone) = device(&engine, &url, "Pixel").await;
    let (desk_id, desk) = device(&engine, &url, "Desktop").await;
    let (mut phone_rx, mut desk_rx) = (phone.subscribe(), desk.subscribe());

    let devices: Vec<DeviceInfo> = desk
        .call_as(Request::SetDeviceNotify {
            id: desk_id.clone(),
            notify: false,
        })
        .await
        .unwrap();
    assert!(
        devices
            .iter()
            .any(|d| d.id == desk_id && !d.notify && d.current)
    );

    let sent: UserNotification = phone
        .call_as(Request::SendNotification {
            title: "Build finished".into(),
            body: "All tests pass".into(),
            session_id: None,
            delay_secs: 0,
        })
        .await
        .unwrap();
    let got = notifications(&mut phone_rx, Duration::from_secs(2)).await;
    assert_eq!(got.iter().map(|n| n.id).collect::<Vec<_>>(), [sent.id]);
    assert!(
        notifications(&mut desk_rx, Duration::from_millis(500))
            .await
            .is_empty(),
        "a device that doesn't take them never hears of them"
    );
    let p: PendingNotifications = desk.call_as(Request::PendingNotifications).await.unwrap();
    assert!(p.new.is_empty(), "nor when it asks what it missed");

    // On again: what it missed comes when it asks; marked shown, never again.
    let _: Vec<DeviceInfo> = desk
        .call_as(Request::SetDeviceNotify {
            id: desk_id.clone(),
            notify: true,
        })
        .await
        .unwrap();
    let p: PendingNotifications = desk.call_as(Request::PendingNotifications).await.unwrap();
    assert_eq!(p.new.iter().map(|n| n.id).collect::<Vec<_>>(), [sent.id]);
    desk.call(Request::AckNotifications { ids: vec![sent.id] })
        .await
        .unwrap();
    let p: PendingNotifications = desk.call_as(Request::PendingNotifications).await.unwrap();
    assert!(p.new.is_empty());
    phone.close();
    desk.close();
}

#[tokio::test]
async fn a_file_attached_on_one_device_downloads_on_another() {
    let (engine, url, _dir) = start().await;
    let (_, phone) = device(&engine, &url, "Pixel").await;
    let (_, desk) = device(&engine, &url, "Desktop").await;
    let s: SessionInfo = phone
        .call_as(Request::CreateSession {
            cwd: None,
            model: None,
            title: None,
            bot_id: None,
            private: false,
            lean: false,
        })
        .await
        .unwrap();
    let data: Vec<u8> = (0..(1 << 20) + 300).map(|i| (i % 241) as u8).collect();
    phone
        .call(Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text("my scan")],
            delivery: Delivery::Auto,
            attachments: vec![Attachment {
                name: "scan.bin".into(),
                mime: "application/octet-stream".into(),
                data: base64::engine::general_purpose::STANDARD.encode(&data),
            }],
        })
        .await
        .unwrap();
    let files: Vec<SharedFile> = desk
        .call_as(Request::ListSharedFiles {
            session_id: Some(s.id.clone()),
            board_id: None,
        })
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    let mut got = Vec::new();
    loop {
        let c: FileChunk = desk
            .call_as(Request::ReadSharedFile {
                id: files[0].id.clone(),
                offset: got.len() as u64,
                len: 1 << 20,
            })
            .await
            .unwrap();
        got.extend(
            base64::engine::general_purpose::STANDARD
                .decode(c.data)
                .unwrap(),
        );
        if c.eof {
            break;
        }
    }
    assert_eq!(got, data, "two pieces, end to end encrypted, intact");
    phone.close();
    desk.close();
}

// Frozen legacy wire decoder: ignores the new kind/quiet fields, exactly
// the failure mode of pre-center native/Android Note::agent (explicit
// messages show their body regardless of jobs/previews preferences).
#[derive(serde::Deserialize)]
#[allow(dead_code)] // frozen 752f201 wire schema, not today's decoder
struct LegacyNotification {
    id: u64,
    title: String,
    body: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    from: String,
    #[serde(default)]
    urgent: bool,
    created_at: u64,
    due_at: u64,
    sent: bool,
    seen: bool,
}
#[derive(serde::Deserialize)]
struct LegacyPending {
    new: Vec<LegacyNotification>,
}
fn legacy_posts(p: LegacyPending, _previews: bool, _jobs: bool) -> Vec<String> {
    // Frozen old Android decide/Note::agent policy: explicit messages are
    // not controlled by previews or jobs. Short plain fixture bodies are
    // below its clipping limits, so this is exactly what it would display.
    p.new
        .into_iter()
        .filter(|n| n.sent && !n.seen)
        .map(|n| n.body)
        .collect()
}

#[tokio::test]
async fn legacy_decoders_never_receive_automatic_content_on_live_or_android_pull_routes() {
    let (engine, url, _dir) = start().await;
    let (_, legacy) = device(&engine, &url, "Legacy phone").await;
    let (_modern_device, modern_token) = engine.store().add_device("Modern phone").unwrap();
    let modern = connect_with_token(&engine, &url, modern_token.clone()).await;
    assert!(
        modern
            .call_as::<bool>(Request::NegotiateNotifications)
            .await
            .unwrap()
    );
    let (mut old_rx, mut new_rx) = (legacy.subscribe(), modern.subscribe());
    let mut job: Job = legacy
        .call_as(Request::SaveJob {
            job: Job {
                name: "Sensitive duty".into(),
                prompt: "Sensitive instructions".into(),
                cwd: _dir.path().to_string_lossy().into(),
                schedule: "@every 1h".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let s: SessionInfo = legacy
        .call_as(Request::RunJob { id: job.id.clone() })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let jobs: Vec<Job> = legacy.call_as(Request::ListJobs).await.unwrap();
            job = jobs.into_iter().find(|j| j.id == job.id).unwrap();
            if job.last_status.as_deref() == Some("error") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        notifications(&mut old_rx, Duration::from_millis(200))
            .await
            .is_empty()
    );
    assert!(
        notifications(&mut new_rx, Duration::from_millis(200))
            .await
            .iter()
            .any(|n| n.kind == "job" && n.session_id.as_deref() == Some(s.id.as_str()))
    );
    let pending = legacy.call(Request::PendingNotifications).await.unwrap();
    let old: LegacyPending = serde_json::from_value(pending).unwrap();
    assert!(
        legacy_posts(old, false, false).is_empty(),
        "legacy policy cannot bypass disabled previews/jobs if no automatic payload exists"
    );
    let pending: PendingNotifications = modern
        .call_as(Request::PendingNotificationsV2)
        .await
        .unwrap();
    assert!(pending.new.iter().any(|n| n.kind == "job"));
    let automatic = pending.new.iter().find(|n| n.kind == "job").unwrap();
    let old_decode: LegacyNotification =
        serde_json::from_value(serde_json::to_value(automatic).unwrap()).unwrap();
    assert!(
        legacy_posts(
            LegacyPending {
                new: vec![old_decode]
            },
            false,
            false
        )[0]
        .contains("Sensitive duty"),
        "the frozen legacy policy reproduces the leak without negotiation, so suppression is essential"
    );
    modern.close();
    let downgraded = connect_with_token(&engine, &url, modern_token).await;
    let pending: LegacyPending = serde_json::from_value(
        downgraded
            .call(Request::PendingNotifications)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        legacy_posts(pending, false, false).is_empty(),
        "a downgraded decoder is safe on the same previously negotiated device"
    );
    let mut downgraded_rx = downgraded.subscribe();
    let second: SessionInfo = downgraded
        .call_as(Request::RunJob { id: job.id.clone() })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let jobs: Vec<Job> = downgraded.call_as(Request::ListJobs).await.unwrap();
            if jobs.iter().any(|j| {
                j.last_session.as_deref() == Some(second.id.as_str())
                    && j.last_status.as_deref() == Some("error")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        notifications(&mut downgraded_rx, Duration::from_millis(200))
            .await
            .is_empty(),
        "negotiation resets on reconnect; same device downgraded to an old app gets no automatic content"
    );
    downgraded.close();
    // Explicit messages retain old behavior, proving this isn't blanket suppression.
    legacy
        .call(Request::SendNotification {
            title: "Requested".into(),
            body: "Explicit message".into(),
            session_id: None,
            delay_secs: 0,
        })
        .await
        .unwrap();
    let old: LegacyPending =
        serde_json::from_value(legacy.call(Request::PendingNotifications).await.unwrap()).unwrap();
    assert_eq!(legacy_posts(old, false, false), ["Explicit message"]);
    legacy.close();
    modern.close();
}
