//! Inbox policy over the real encrypted client/server path, in a throwaway home.
use std::time::Duration;
use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::{Engine, EngineOptions, config::Paths};
use tessera_proto::*;

async fn connect(e: &Engine, url: &str, token: &str) -> Client {
    let key = tessera_server::load_identity(e.paths()).unwrap();
    let c = Client::connect(ServerConfig {
        endpoints: vec![url.into()],
        token: token.into(),
        server_key: tessera_secure::b64(key.public()),
        ..Default::default()
    });
    c.wait_connected(Duration::from_secs(5)).await.unwrap();
    assert!(c.status().encrypted);
    c
}

#[tokio::test]
async fn encrypted_inbox_reads_and_id_actions_follow_device_policy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n").unwrap();
    let e = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = tessera_server::router(e.clone(), None).unwrap();
    let server =
        tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    let (owner, token) = e.store().add_device("Phone").unwrap();
    let (_, token2) = e.store().add_device("Desk").unwrap();
    let phone = connect(&e, &url, &token).await;
    let desk = connect(&e, &url, &token2).await;
    let s: SessionInfo = phone
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
    let n: UserNotification = phone
        .call_as(Request::SendNotification {
            title: "Personal reminder".into(),
            body: "A private message".into(),
            session_id: Some(s.id),
            delay_secs: 0,
        })
        .await
        .unwrap();
    let mine: Vec<UserNotification> = phone.call_as(Request::ListNotificationInbox).await.unwrap();
    assert_eq!(mine.len(), 1);
    let other: Vec<UserNotification> = desk.call_as(Request::ListNotificationInbox).await.unwrap();
    assert!(other.is_empty());
    for req in [
        Request::AckNotifications { ids: vec![n.id] },
        Request::SeenNotifications {
            ids: vec![n.id],
            session_id: None,
        },
        Request::CancelNotification { id: n.id },
    ] {
        assert_eq!(desk.call(req).await.unwrap_err().code, ErrorCode::NotFound);
    }
    for _ in 0..2 {
        phone
            .call(Request::SeenNotifications {
                ids: vec![n.id],
                session_id: None,
            })
            .await
            .unwrap();
    }
    let read: Vec<UserNotification> = phone.call_as(Request::ListNotificationInbox).await.unwrap();
    assert!(read[0].seen);
    phone
        .call(Request::SetDeviceNotify {
            id: owner.clone(),
            notify: false,
        })
        .await
        .unwrap();
    let mut events = phone.subscribe();
    let public: UserNotification = desk
        .call_as(Request::SendNotification {
            title: "Server reminder".into(),
            body: "Review later".into(),
            session_id: None,
            delay_secs: 0,
        })
        .await
        .unwrap();
    let end = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut invalidated = false;
    while let Ok(Ok(ev)) = tokio::time::timeout_at(end, events.recv()).await {
        match ev {
            ClientEvent::Event(Event::NotificationsSeen { ids }) if ids.is_empty() => {
                invalidated = true
            }
            ClientEvent::Event(Event::Notification { notification }) => {
                assert_ne!(notification.id, public.id, "OS preference remains off")
            }
            _ => {}
        }
    }
    assert!(invalidated, "inbox refreshes while OS messages are off");
    let items: Vec<UserNotification> = phone.call_as(Request::ListNotificationInbox).await.unwrap();
    assert!(items.iter().any(|n| n.id == public.id));
    desk.call(Request::RevokeDevice { id: owner })
        .await
        .unwrap();
    assert!(e.store().authenticate(&token).unwrap().is_none());
    let other: Vec<UserNotification> = desk.call_as(Request::ListNotificationInbox).await.unwrap();
    assert!(other.iter().all(|item| item.id != n.id));
    server.abort();
}
