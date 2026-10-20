//! Private chats stay on the device that made them (batch 17), over real
//! encrypted connections: another device (a phone, a computer, a terminal
//! app, a web browser: each its own login) never lists, opens, changes or
//! hears of one, nor gets its notifications; the same device reconnecting
//! still has it; signing that device out takes its private chats with it.

use std::time::Duration;

use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    // A provider on a closed port: model requests fail fast, which is fine here.
    std::fs::write(
        dir.path().join("config.toml"),
        "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n",
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

/// An encrypted connection with `token`.
async fn connect(engine: &tessera_core::Engine, url: &str, token: &str) -> Client {
    let key = tessera_server::load_identity(engine.paths()).unwrap();
    let client = Client::connect(ServerConfig {
        endpoints: vec![url.to_string()],
        token: token.to_string(),
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

/// A paired device: its id, token and connection.
async fn device(engine: &tessera_core::Engine, url: &str, name: &str) -> (String, String, Client) {
    let (id, token) = engine.store().add_device(name).unwrap();
    let client = connect(engine, url, &token).await;
    (id, token, client)
}

/// The events this client heard within `wait` that name `session`.
async fn heard_of(
    rx: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    session: &str,
    wait: Duration,
) -> Vec<Event> {
    let mut out = Vec::new();
    let end = tokio::time::Instant::now() + wait;
    while let Ok(r) = tokio::time::timeout_at(end, rx.recv()).await {
        let Ok(ClientEvent::Event(e)) = r else {
            continue;
        };
        let names = match &e {
            Event::Notification { notification } => {
                notification.session_id.as_deref() == Some(session)
            }
            e => e.session_id() == Some(session),
        };
        if names {
            out.push(e);
        }
    }
    out
}

fn not_found(r: Result<serde_json::Value, ApiError>) -> bool {
    matches!(r, Err(e) if e.code == ErrorCode::NotFound)
}

#[tokio::test]
async fn a_private_chat_is_only_ever_on_the_device_that_made_it() {
    let (engine, url, _dir) = start().await;
    let (phone_id, phone_token, phone) = device(&engine, &url, "Pixel").await;
    let (_desk_id, _, desk) = device(&engine, &url, "Desktop").await;
    let (_tui_id, _, tui) = device(&engine, &url, "Terminal on my-server").await;
    let (mut phone_rx, mut desk_rx, mut tui_rx) =
        (phone.subscribe(), desk.subscribe(), tui.subscribe());

    // The phone makes a private chat, and a normal one.
    let private: SessionInfo = phone
        .call_as(Request::CreateSession {
            cwd: None,
            model: None,
            title: None,
            bot_id: None,
            private: true,
            lean: false,
        })
        .await
        .unwrap();
    assert!(private.private);
    let normal: SessionInfo = phone
        .call_as(Request::CreateSession {
            cwd: None,
            model: None,
            title: Some("Shared".into()),
            bot_id: None,
            private: false,
            lean: false,
        })
        .await
        .unwrap();

    // Listed on the phone only.
    let ids = |c: &Client| {
        let c = c.clone();
        async move {
            let l: Vec<SessionInfo> = c
                .call_as(Request::ListSessions { archived: false })
                .await
                .unwrap();
            l.into_iter().map(|s| s.id).collect::<Vec<_>>()
        }
    };
    let on_phone = ids(&phone).await;
    assert!(on_phone.contains(&private.id) && on_phone.contains(&normal.id));
    for other in [&desk, &tui] {
        let l = ids(other).await;
        assert!(l.contains(&normal.id), "normal chats are on every device");
        assert!(!l.contains(&private.id), "never listed on another device");
    }

    // Another device can't open, change, write in or delete it: as if it didn't exist.
    for other in [&desk, &tui] {
        assert!(not_found(
            other
                .call(Request::GetSession {
                    session_id: private.id.clone(),
                    since_seq: None,
                })
                .await
        ));
        assert!(not_found(
            other
                .call(Request::Send {
                    session_id: private.id.clone(),
                    content: vec![ContentPart::text("let me in")],
                    delivery: Delivery::Auto,
                    attachments: Vec::new(),
                })
                .await
        ));
        assert!(not_found(
            other
                .call(Request::UpdateSession {
                    session_id: private.id.clone(),
                    title: Some("Renamed".into()),
                    model: None,
                    thinking: None,
                    permission: None,
                    pinned: None,
                    archived: None,
                    cwd: None,
                    members: None,
                    fast: None,
                    subagents: None,
                })
                .await
        ));
        assert!(not_found(
            other
                .call(Request::Abort {
                    session_id: private.id.clone(),
                })
                .await
        ));
        assert!(not_found(
            other
                .call(Request::DeleteSession {
                    session_id: private.id.clone(),
                })
                .await
        ));
        assert!(not_found(
            other
                .call(Request::CreateBoard {
                    session_id: private.id.clone(),
                    title: "Peek".into(),
                    content: BoardContent::Markdown {
                        markdown: "x".into(),
                    },
                    placed: false,
                })
                .await
        ));
    }

    // The phone uses it: it hears of every step, the other devices of none.
    phone
        .call(Request::Send {
            session_id: private.id.clone(),
            content: vec![ContentPart::text("A secret question")],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    // A notification from it (as an agent's or a long turn's would be).
    let _: UserNotification = phone
        .call_as(Request::SendNotification {
            title: "Done".into(),
            body: "Your private answer".into(),
            session_id: Some(private.id.clone()),
            delay_secs: 0,
        })
        .await
        .unwrap();
    let wait = Duration::from_secs(3);
    let (p, d, t) = tokio::join!(
        heard_of(&mut phone_rx, &private.id, wait),
        heard_of(&mut desk_rx, &private.id, wait),
        heard_of(&mut tui_rx, &private.id, wait),
    );
    assert!(
        p.iter().any(|e| matches!(e, Event::SessionUpdated { .. }))
            && p.iter().any(|e| matches!(e, Event::EntryAdded { .. })),
        "the phone hears of its chat: {p:?}"
    );
    assert!(d.is_empty(), "the computer hears nothing of it: {d:?}");
    assert!(t.is_empty(), "the terminal app hears nothing of it: {t:?}");
    // Nor when they ask what they missed.
    for other in [&desk, &tui] {
        let n: Vec<UserNotification> = other.call_as(Request::ListNotifications).await.unwrap();
        assert!(
            n.iter()
                .all(|n| n.session_id.as_deref() != Some(&private.id))
        );
        let pending: PendingNotifications =
            other.call_as(Request::PendingNotifications).await.unwrap();
        assert!(
            pending
                .new
                .iter()
                .all(|n| n.session_id.as_deref() != Some(&private.id))
        );
    }
    let n: Vec<UserNotification> = phone.call_as(Request::ListNotifications).await.unwrap();
    assert!(
        n.iter()
            .any(|n| n.session_id.as_deref() == Some(&private.id))
    );

    // The same device again (a new connection, its own login): still its own.
    phone.close();
    let phone = connect(&engine, &url, &phone_token).await;
    assert!(ids(&phone).await.contains(&private.id));
    let snap: SessionSnapshot = phone
        .call_as(Request::GetSession {
            session_id: private.id.clone(),
            since_seq: None,
        })
        .await
        .unwrap();
    assert!(
        snap.entries
            .iter()
            .any(|e| matches!(&e.body, EntryBody::User { .. }))
    );

    // The phone is signed out (from the computer): its private chat goes at once.
    desk.call(Request::RevokeDevice {
        id: phone_id.clone(),
    })
    .await
    .unwrap();
    let all: Vec<SessionInfo> = serde_json::from_value(
        engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        all.iter().all(|s| s.id != private.id),
        "gone with its device"
    );
    assert!(all.iter().any(|s| s.id == normal.id), "the rest stays");
    desk.close();
    tui.close();
}
