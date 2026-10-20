//! "Finished" comes once, when long work has really ended: not while the
//! chat's own helpers are still at work (their reports start its next turn),
//! not before a queued message it takes up next. Against the scripted mock.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_proto::*;

fn notifications(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<UserNotification> {
    let mut out = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::Notification { notification } = e {
            out.push(notification);
        }
    }
    out
}

fn said(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect()
}

async fn until_said(h: &Harness, id: &str, text: &str) {
    for _ in 0..400 {
        if said(&h.entries(id).await).iter().any(|t| t == text) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never said {text:?}: {:?}", said(&h.entries(id).await));
}

#[tokio::test]
async fn long_work_with_helpers_is_finished_once_when_all_of_it_ends() {
    // Every turn counts as long work here.
    let h = harness_agent(vec![], 100_000, "notify_after_minutes = 0.0000001").await;
    h.mock.route(
        "<LONG-PARENT>",
        Reply::Tools(vec![(
            "agent".into(),
            json!({"tasks": [{"name": "Slowpoke", "prompt": "Take your time <LONG-HELPER>"}]}),
        )]),
    );
    h.mock
        .route("<LONG-PARENT>", Reply::Text("The helper is on it.".into()));
    h.mock.route(
        "<LONG-PARENT>",
        Reply::Text("All done after the helper.".into()),
    );
    h.mock.route(
        "<LONG-HELPER>",
        Reply::Slow(Duration::from_secs(2), "Helper report.".into()),
    );
    let mut events = h.engine.subscribe();
    let chat = h.session().await;
    h.send(&chat.id, "Work long <LONG-PARENT>").await;
    until_said(&h, &chat.id, "The helper is on it.").await;
    h.settle(&chat.id).await;
    // Its turn ended, but its helper is still at work: not finished.
    assert!(notifications(&mut events).is_empty());
    until_said(&h, &chat.id, "All done after the helper.").await;
    h.settle(&chat.id).await;
    let n = notifications(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].title, "Finished");
    assert!(n[0].body.ends_with("All done after the helper."), "{n:?}");
    // A new message from the user is new work: its own notification.
    h.send(&chat.id, "And one more thing").await;
    h.settle(&chat.id).await;
    let n = notifications(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].title, "Finished");
}

#[tokio::test]
async fn a_queued_message_is_part_of_the_same_work() {
    let h = harness_agent(vec![], 100_000, "notify_after_minutes = 0.0000001").await;
    h.mock.route(
        "<QUEUE-FIRST>",
        Reply::Slow(Duration::from_secs(2), "First part done.".into()),
    );
    h.mock
        .route("<QUEUE-FIRST>", Reply::Text("Second part done.".into()));
    let mut events = h.engine.subscribe();
    let chat = h.session().await;
    h.send(&chat.id, "Start <QUEUE-FIRST>").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Sent while it works: queued, taken up right after.
    h.send(&chat.id, "and then the second part").await;
    until_said(&h, &chat.id, "Second part done.").await;
    h.settle(&chat.id).await;
    let n = notifications(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].body.ends_with("Second part done."), "{n:?}");
}
