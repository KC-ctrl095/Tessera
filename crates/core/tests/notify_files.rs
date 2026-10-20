//! Notifications for the user's devices (`notify`) and files they can
//! download from a chat (`share_file`, uploads).

mod common;

use std::time::Duration;

use base64::Engine as _;
use common::*;
use serde_json::{Value, json};
use tessera_proto::*;

async fn call<T: serde::de::DeserializeOwned>(
    h: &Harness,
    req: Request,
    device: Option<&str>,
) -> T {
    serde_json::from_value(h.engine.handle(req, device).await.unwrap()).unwrap()
}

/// Notifications in the events seen so far: (title, sent).
fn notifications(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<UserNotification> {
    let mut out = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::Notification { notification } = e {
            out.push(notification);
        }
    }
    out
}

#[tokio::test]
async fn an_agent_notifies_now_and_later_and_each_device_shows_it_once() {
    let h = harness(
        vec![
            Reply::Tools(vec![
                (
                    "notify".into(),
                    json!({"title": "Build finished", "body": "All 42 tests pass.\n\nThe release is in out/."}),
                ),
                (
                    "notify".into(),
                    json!({"title": "Stretch", "body": "Time to stand up", "at": "in 1s"}),
                ),
            ]),
            // The same one again: refused, not sent twice.
            Reply::Tools(vec![(
                "notify".into(),
                json!({"title": "Build finished", "body": "All 42 tests pass.\n\nThe release is in out/."}),
            )]),
            Reply::Text("Done.".into()),
        ],
        100_000,
    )
    .await;
    let (phone, _) = h.engine.store().add_device("Pixel").unwrap();
    let (desk, _) = h.engine.store().add_device("Desktop app").unwrap();
    let devices: Vec<DeviceInfo> = call(
        &h,
        Request::SetDeviceNotify {
            id: desk.clone(),
            notify: false,
        },
        Some(&phone),
    )
    .await;
    assert!(devices.iter().any(|d| d.id == desk && !d.notify));
    assert!(!h.engine.takes_notifications(&desk));
    assert!(h.engine.takes_notifications(&phone));

    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(
        &s.id,
        "Tell me when the build is done, and remind me to stretch",
    )
    .await;
    h.settle(&s.id).await;

    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(
        !results[0].1
            && results[0]
                .2
                .contains("Notification saved for receiving devices: Pixel."),
        "{results:?}"
    );
    assert!(
        !results[1].1 && results[1].2.contains("Scheduled for"),
        "{results:?}"
    );
    assert!(
        results[2].1 && results[2].2.contains("already sent"),
        "{results:?}"
    );

    let first = notifications(&mut events);
    assert_eq!(first.len(), 2, "{first:?}");
    let now = first.iter().find(|n| n.sent).unwrap();
    assert_eq!(now.title, "Build finished");
    assert_eq!(now.body, "All 42 tests pass.\n\nThe release is in out/.");
    assert_eq!(now.session_id.as_deref(), Some(s.id.as_str()));
    let later = first.iter().find(|n| !n.sent).unwrap();
    assert_eq!(later.title, "Stretch");

    // Due a second later: sent then (no scheduler running here: on time by
    // its own timer).
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let due = notifications(&mut events);
    assert_eq!(due.len(), 1, "{due:?}");
    assert!(due[0].sent && due[0].id == later.id);

    // A waking phone gets the latest chat alert, not two popups. The earlier
    // request remains in the inbox as seen, rather than disappearing.
    let p: PendingNotifications = call(&h, Request::PendingNotifications, Some(&phone)).await;
    let ids: Vec<u64> = p.new.iter().map(|n| n.id).collect();
    assert_eq!(ids, [later.id]);
    let inbox: Vec<UserNotification> = call(&h, Request::ListNotificationInbox, Some(&phone)).await;
    assert!(inbox.iter().any(|n| n.id == now.id && n.seen));
    assert!(inbox.iter().any(|n| n.id == later.id && !n.seen));
    assert_eq!(p.next_due, None);
    let _: Value = call(&h, Request::AckNotifications { ids }, Some(&phone)).await;
    let p: PendingNotifications = call(&h, Request::PendingNotifications, Some(&phone)).await;
    assert!(p.new.is_empty());
    // The desk doesn't take them.
    let p: PendingNotifications = call(&h, Request::PendingNotifications, Some(&desk)).await;
    assert!(p.new.is_empty());

    // Opening the chat anywhere marks them seen; the phone removes them.
    let _: Value = call(
        &h,
        Request::SeenNotifications {
            ids: vec![],
            session_id: Some(s.id.clone()),
        },
        Some(&desk),
    )
    .await;
    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::NotificationsSeen { ids } = e {
            seen.extend(ids);
        }
    }
    assert_eq!(seen, [later.id]);
    let p: PendingNotifications = call(&h, Request::PendingNotifications, Some(&phone)).await;
    assert_eq!(p.seen, [later.id]);
    let inbox: Vec<UserNotification> = call(&h, Request::ListNotificationInbox, Some(&phone)).await;
    assert!(inbox.iter().any(|n| n.id == now.id && n.seen));
    assert!(inbox.iter().any(|n| n.id == later.id && n.seen));
}

#[tokio::test]
async fn scheduled_notifications_can_be_listed_cancelled_and_tested_from_settings() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "notify".into(),
                json!({"title": "Call the dentist", "body": "", "at": "tomorrow 9:00"}),
            )]),
            Reply::Tools(vec![("notify".into(), json!({"cancel": 999}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let (phone, _) = h.engine.store().add_device("Pixel").unwrap();
    let s = h.session().await;
    h.send(&s.id, "remind me tomorrow").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].2.contains("Scheduled for"), "{results:?}");
    assert!(
        results[1].1 && results[1].2.contains("no scheduled"),
        "{results:?}"
    );

    let list: Vec<UserNotification> = call(&h, Request::ListNotifications, None).await;
    let n = list.iter().find(|n| !n.sent).unwrap().clone();
    let p: PendingNotifications = call(&h, Request::PendingNotifications, Some(&phone)).await;
    assert_eq!(p.next_due, Some(n.due_at), "a phone wakes up for it");
    let _: Value = call(&h, Request::CancelNotification { id: n.id }, None).await;
    let list: Vec<UserNotification> = call(&h, Request::ListNotifications, None).await;
    assert!(list.iter().all(|x| x.id != n.id));

    // "Send a test" from Settings, now: reaches the phone.
    let t: UserNotification = call(
        &h,
        Request::SendNotification {
            title: "Test".into(),
            body: "It works".into(),
            session_id: None,
            delay_secs: 0,
        },
        Some(&phone),
    )
    .await;
    assert!(t.sent && t.from == "Tessera");
    let p: PendingNotifications = call(&h, Request::PendingNotifications, Some(&phone)).await;
    assert_eq!(p.new.iter().map(|n| n.id).collect::<Vec<_>>(), [t.id]);
    // Nothing to send is refused.
    assert!(
        h.engine
            .handle(
                Request::SendNotification {
                    title: " ".into(),
                    body: "\u{202e}".into(),
                    session_id: None,
                    delay_secs: 0,
                },
                None,
            )
            .await
            .is_err()
    );
}

async fn download(h: &Harness, id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let c: FileChunk = call(
            h,
            Request::ReadSharedFile {
                id: id.into(),
                offset: out.len() as u64,
                len: 1 << 20,
            },
            None,
        )
        .await;
        assert_eq!(c.offset, out.len() as u64);
        out.extend(
            base64::engine::general_purpose::STANDARD
                .decode(c.data)
                .unwrap(),
        );
        if c.eof {
            assert_eq!(c.size, out.len() as u64);
            return out;
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn agents_share_files_as_downloads_but_never_secrets() {
    let h = harness(vec![], 100_000).await;
    let report: Vec<u8> = (0..(2 << 20) + 1234).map(|i| (i % 253) as u8).collect();
    std::fs::write(h.work.join("report.bin"), &report).unwrap();
    std::fs::write(h.work.join(".env"), "TOKEN=abc\n").unwrap();
    std::fs::write(
        h.work.join("notes.txt"),
        "my key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        h.dir.path().join("config.toml"),
        h.work.join("innocent.txt"),
    )
    .unwrap();
    let config = h.dir.path().join("config.toml").display().to_string();
    h.mock.push(Reply::Tools(vec![
        (
            "share_file".into(),
            json!({"path": "report.bin", "name": "Q3 report.bin"}),
        ),
        ("share_file".into(), json!({"path": ".env"})),
        ("share_file".into(), json!({"path": "notes.txt"})),
        ("share_file".into(), json!({"path": config})),
        ("share_file".into(), json!({"path": "innocent.txt"})),
        ("share_file".into(), json!({"path": "missing.txt"})),
    ]));
    h.mock.push(Reply::Text("Here you go.".into()));
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "send me the report").await;
    h.settle(&s.id).await;

    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    assert_eq!(results.len(), 6, "{results:?}");
    assert!(
        !results[0].1 && results[0].2.contains("Shared Q3 report.bin (2.0 MB)"),
        "{results:?}"
    );
    for (i, why) in [
        (1, "credentials"),
        (2, "looks like"),
        (3, "Tessera's own"),
        (4, "Tessera's own"),
    ] {
        assert!(
            results[i].1 && results[i].2.contains(why),
            "{i}: {results:?}"
        );
    }
    assert!(results[5].1, "{results:?}");
    // The tool result carries the file for the chat's download chip.
    let details = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                details: Some(d),
                ..
            } if tool == "share_file" => Some(d.clone()),
            _ => None,
        })
        .unwrap();
    let file: SharedFile = serde_json::from_value(details["file"].clone()).unwrap();
    assert_eq!(
        (file.name.as_str(), file.size, file.source.as_str()),
        ("Q3 report.bin", report.len() as u64, "agent")
    );
    let mut shared = false;
    while let Ok(e) = events.try_recv() {
        shared |= matches!(&e, Event::FileShared { file: f } if f.id == file.id);
    }
    assert!(shared, "clients hear about it");

    // Listed with the chat; downloaded in pieces, as it was when shared.
    let list: Vec<SharedFile> = call(
        &h,
        Request::ListSharedFiles {
            session_id: Some(s.id.clone()),
            board_id: None,
        },
        None,
    )
    .await;
    assert_eq!(
        list.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        [file.id.as_str()]
    );
    std::fs::write(h.work.join("report.bin"), b"changed").unwrap();
    assert_eq!(download(&h, &file.id).await, report);
    // Tessera's copy sits in its own (protected) store, owner-only.
    assert!(file.path.starts_with(&h.dir.path().display().to_string()));
    // A copy swapped for a link (to a secret) is never served.
    let copy = std::path::PathBuf::from(&file.path);
    std::fs::rename(&copy, copy.with_extension("moved")).unwrap();
    std::os::unix::fs::symlink(h.dir.path().join("config.toml"), &copy).unwrap();
    let refused = h
        .engine
        .handle(
            Request::ReadSharedFile {
                id: file.id.clone(),
                offset: 0,
                len: 10,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::NotFound);
    std::fs::remove_file(&copy).unwrap();
    std::fs::rename(copy.with_extension("moved"), &copy).unwrap();

    // Deleting the chat deletes its files.
    let _: Value = call(
        &h,
        Request::DeleteSession {
            session_id: s.id.clone(),
        },
        None,
    )
    .await;
    assert!(!std::path::Path::new(&file.path).exists());
    let gone = h
        .engine
        .handle(
            Request::ReadSharedFile {
                id: file.id.clone(),
                offset: 0,
                len: 10,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn uploads_can_be_downloaded_again_from_any_device() {
    let h = harness(vec![Reply::Text("Got it.".into())], 100_000).await;
    let s = h.session().await;
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("here")],
                delivery: Delivery::Auto,
                attachments: vec![Attachment {
                    name: "plan.pdf".into(),
                    mime: "application/pdf".into(),
                    data: base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 fake"),
                }],
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let list: Vec<SharedFile> = call(
        &h,
        Request::ListSharedFiles {
            session_id: Some(s.id.clone()),
            board_id: None,
        },
        None,
    )
    .await;
    assert_eq!(list.len(), 1);
    assert_eq!(
        (
            list[0].name.as_str(),
            list[0].source.as_str(),
            list[0].mime.as_str()
        ),
        ("plan.pdf", "upload", "application/pdf")
    );
    // The chat's attachment line names the same path.
    let text = h
        .entries(&s.id)
        .await
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::User { content, .. } => Some(plain_text(content)),
            _ => None,
        })
        .unwrap();
    assert!(
        text.contains(&format!("saved at {}", list[0].path)),
        "{text}"
    );
    assert_eq!(download(&h, &list[0].id).await, b"%PDF-1.4 fake");
}

#[tokio::test]
async fn private_chats_offer_neither_tool() {
    let h = harness(vec![Reply::Text("hi".into())], 100_000).await;
    let v = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: None,
                model: None,
                title: None,
                bot_id: None,
                private: true,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(v).unwrap();
    h.send(&s.id, "hello").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let names: Vec<&str> = req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"notify") && !names.contains(&"share_file"),
        "{names:?}"
    );
}

#[tokio::test]
async fn long_work_ends_with_a_notification_unless_the_agent_sent_one() {
    // Every turn counts as long work here.
    let h = harness_agent(
        vec![
            Reply::Text("All 12 pages are translated.\n\nThe glossary is in docs/.".into()),
            Reply::Tools(vec![(
                "notify".into(),
                json!({"title": "Translated", "body": "All 12 pages."}),
            )]),
            Reply::Text("Done.".into()),
            Reply::Text("Private words.".into()),
        ],
        100_000,
        "notify_after_minutes = 0.0000001",
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "Translate the site").await;
    h.settle(&s.id).await;
    let n = notifications(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].title, "Finished");
    assert_eq!(n[0].body, "Worked 1 min. All 12 pages are translated.");
    assert_eq!(n[0].session_id.as_deref(), Some(s.id.as_str()));

    // The agent told the user itself: nothing more.
    h.send(&s.id, "Do it again and tell me").await;
    h.settle(&s.id).await;
    let n = notifications(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].title, "Translated");

    // A private chat's says nothing of what was done.
    let v = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: None,
                model: None,
                title: None,
                bot_id: None,
                private: true,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    let p: SessionInfo = serde_json::from_value(v).unwrap();
    h.send(&p.id, "hello").await;
    h.settle(&p.id).await;
    let n = notifications(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!((n[0].title.as_str(), n[0].body.as_str()), ("Tessera", ""));
}
