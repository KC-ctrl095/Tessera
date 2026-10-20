//! The queue above the composer: messages waiting for a busy agent, and how
//! they reach it (after the turn, at its next step, or sent now).

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use tessera_proto::*;
use tokio::sync::broadcast::Receiver;

fn user_texts(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: None,
            } => Some(plain_text(content)),
            _ => None,
        })
        .collect()
}

/// Every queue the session's clients were told about so far, in order.
fn queues(events: &mut Receiver<Event>, sid: &str) -> Vec<Vec<QueuedMessage>> {
    let mut out = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::QueueChanged { session_id, queued } = e
            && session_id == sid
        {
            out.push(queued);
        }
    }
    out
}

/// Wait until the user's messages in the transcript begin with `want`.
async fn wait_for_users<const N: usize>(h: &Harness, sid: &str, want: [&str; N]) {
    let start = Instant::now();
    loop {
        if user_texts(&h.entries(sid).await).starts_with(&want.map(String::from)) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "never saw {want:?}: {:?}",
            user_texts(&h.entries(sid).await)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn queued_now(h: &Harness, sid: &str) -> Vec<QueuedMessage> {
    let v = h
        .engine
        .handle(
            Request::GetSession {
                session_id: sid.into(),
                since_seq: Some(u64::MAX),
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value::<SessionSnapshot>(v).unwrap().queued
}

async fn send_as(h: &Harness, sid: &str, text: &str, delivery: Delivery) {
    h.engine
        .handle(
            Request::Send {
                session_id: sid.into(),
                content: vec![ContentPart::text(text)],
                delivery,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_follow_up_leaves_the_queue_as_soon_as_it_is_delivered() {
    let h = harness(
        vec![
            Reply::Slow(Duration::from_millis(500), "first".into()),
            Reply::Slow(Duration::from_millis(1500), "second".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "one").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    h.send(&s.id, "maybe something greek").await;
    let told = queues(&mut events, &s.id);
    assert_eq!(told.last().map(Vec::len), Some(1), "{told:?}");

    // The first reply ends and the follow-up starts the next one.
    wait_for_users(&h, &s.id, ["one", "maybe something greek"]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Running);
    // Clients that only listen (no reload) drop its card too.
    let told = queues(&mut events, &s.id);
    assert_eq!(told.last(), Some(&Vec::new()), "{told:?}");
    assert!(queued_now(&h, &s.id).await.is_empty());
    h.settle(&s.id).await;
}

#[tokio::test]
async fn a_queued_message_comes_whole_for_editing_and_keeps_its_attachments() {
    use base64::Engine as _;
    let h = harness(
        vec![
            Reply::Slow(Duration::from_millis(600), "first".into()),
            Reply::Text("second".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "one").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let long = format!("{}\nand a second line", "a long message ".repeat(30));
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text(&long)],
                delivery: Delivery::Auto,
                attachments: vec![Attachment {
                    name: "notes.txt".into(),
                    mime: "text/plain".into(),
                    data: base64::engine::general_purpose::STANDARD.encode(b"the file's text"),
                }],
            },
            None,
        )
        .await
        .unwrap();
    // The whole of what they typed (not a one-line preview), the file counted.
    let q = queued_now(&h, &s.id).await;
    assert_eq!(q.len(), 1, "{q:?}");
    assert_eq!(q[0].text, long.trim());
    assert_eq!(q[0].attachments, 1);

    // Editing replaces what they typed and keeps the file.
    h.engine
        .handle(
            Request::EditQueued {
                session_id: s.id.clone(),
                index: 0,
                text: "shorter now".into(),
            },
            None,
        )
        .await
        .unwrap();
    let q = queued_now(&h, &s.id).await;
    assert_eq!((q[0].text.as_str(), q[0].attachments), ("shorter now", 1));
    // Emptied, it is just the file; a message with nothing left can't be.
    h.engine
        .handle(
            Request::EditQueued {
                session_id: s.id.clone(),
                index: 0,
                text: "  ".into(),
            },
            None,
        )
        .await
        .unwrap();
    let q = queued_now(&h, &s.id).await;
    assert_eq!((q[0].text.as_str(), q[0].attachments), ("", 1));
    h.settle(&s.id).await;
    let users = user_texts(&h.entries(&s.id).await);
    assert!(
        users[1].starts_with("[Attached file: notes.txt"),
        "{users:?}"
    );
}

fn last_reply(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
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
        .unwrap_or_default()
}

async fn interrupt_queued(h: &Harness, sid: &str, index: u32) {
    h.engine
        .handle(
            Request::InterruptQueued {
                session_id: sid.into(),
                index,
            },
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn send_now_stops_the_turn_and_the_next_one_starts_with_that_message() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "sleep 30", "timeout": 60}),
            )]),
            Reply::Text("on it".into()),
            Reply::Text("and the rest".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "start").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    send_as(&h, &s.id, "later", Delivery::FollowUp).await;
    send_as(&h, &s.id, "read this too", Delivery::Steer).await;
    send_as(&h, &s.id, "stop, do X instead", Delivery::FollowUp).await;
    let texts = |q: &[QueuedMessage]| q.iter().map(|m| m.text.clone()).collect::<Vec<_>>();
    assert_eq!(
        texts(&queued_now(&h, &s.id).await),
        ["read this too", "later", "stop, do X instead"]
    );
    let mut events = h.engine.subscribe();
    let mut timeline = h.engine.subscribe();
    let started = Instant::now();
    interrupt_queued(&h, &s.id, 2).await;
    // It goes first, shown as being sent, until the next turn has it.
    let told = queues(&mut events, &s.id);
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        texts(&told[0]),
        ["stop, do X instead", "read this too", "later"]
    );
    assert!(told[0][0].interrupt && !told[0][0].steer);
    assert!(told[0][1].steer && !told[0][1].interrupt);

    wait_for_users(&h, &s.id, ["start", "stop, do X instead", "read this too"]).await;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the running command was stopped at once"
    );
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    // Steering was read at once too; the follow-up waited for its turn.
    assert_eq!(
        user_texts(&entries),
        ["start", "stop, do X instead", "read this too", "later"]
    );
    assert_eq!(last_reply(&entries), "and the rest");
    // What Stop does: the interrupted call is reported, and a notice says so.
    let results = tool_results(&entries);
    assert!(
        results[0].1 && results[0].2.contains("Interrupted"),
        "{results:?}"
    );
    let stopped = entries
        .iter()
        .position(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text == "Stopped."))
        .expect("a Stopped. notice");
    let sent = entries
        .iter()
        .position(|e| matches!(&e.body, EntryBody::User { content, .. } if plain_text(content) == "stop, do X instead"))
        .unwrap();
    assert!(stopped < sent);
    // The model saw the message right away, before the follow-up.
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 3);
    let seen = reqs[1]["messages"].to_string();
    let (x, too) = (
        seen.find("stop, do X instead").unwrap(),
        seen.find("read this too").unwrap(),
    );
    assert!(x < too && !seen.contains("\"later\""), "{seen}");
    // The chat stayed busy between the two turns, and the queue emptied.
    let mut idle_before_sent = false;
    let mut delivered = false;
    while let Ok(e) = timeline.try_recv() {
        match e {
            Event::EntryAdded { entry, .. } => {
                if let EntryBody::User { content, .. } = &entry.body {
                    delivered |= plain_text(content) == "stop, do X instead";
                }
            }
            Event::SessionUpdated { session } if session.id == s.id && !delivered => {
                idle_before_sent |= session.status == SessionStatus::Idle;
            }
            _ => {}
        }
    }
    assert!(delivered && !idle_before_sent);
    assert!(queued_now(&h, &s.id).await.is_empty());
    assert_eq!(queues(&mut events, &s.id).last(), Some(&Vec::new()));
}

#[tokio::test]
async fn sending_with_interrupt_stops_a_long_reply() {
    let h = harness(
        vec![
            Reply::Slow(Duration::from_secs(30), "never".into()),
            Reply::Text("a haiku then".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "write an essay").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    send_as(&h, &s.id, "actually, just a haiku", Delivery::Interrupt).await;
    wait_for_users(&h, &s.id, ["write an essay", "actually, just a haiku"]).await;
    h.settle(&s.id).await;
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(last_reply(&h.entries(&s.id).await), "a haiku then");
    assert!(queued_now(&h, &s.id).await.is_empty());
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
}

#[tokio::test]
async fn send_now_on_an_idle_chat_starts_a_turn_with_that_message() {
    let h = harness(
        vec![
            Reply::Slow(Duration::from_secs(30), "never".into()),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "one").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.send(&s.id, "two").await;
    h.engine
        .handle(
            Request::Abort {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    // Stop keeps the queue.
    assert_eq!(queued_now(&h, &s.id).await.len(), 1);
    interrupt_queued(&h, &s.id, 0).await;
    wait_for_users(&h, &s.id, ["one", "two"]).await;
    h.settle(&s.id).await;
    assert_eq!(last_reply(&h.entries(&s.id).await), "ok");
    assert!(queued_now(&h, &s.id).await.is_empty());
}

#[tokio::test]
async fn stop_right_after_send_now_keeps_the_message_queued() {
    let h = harness(
        vec![Reply::Slow(Duration::from_secs(30), "never".into())],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "one").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.send(&s.id, "two").await;
    send_as(&h, &s.id, "steer it", Delivery::Steer).await;
    let mut events = h.engine.subscribe();
    interrupt_queued(&h, &s.id, 1).await;
    // The admission event is synchronous. Do not await GetSession here: its
    // blocking snapshot yields to the cancelled worker, which may legitimately
    // start the next turn before Stop is sent. This current-thread test stops
    // immediately after admission, not after work has already been consumed.
    let mut accepted = None;
    while let Ok(event) = events.try_recv() {
        if let Event::QueueChanged { session_id, queued } = event
            && session_id == s.id
        {
            accepted = Some(queued);
        }
    }
    assert!(accepted.unwrap()[0].interrupt);
    h.engine
        .handle(
            Request::Abort {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    assert_eq!(user_texts(&h.entries(&s.id).await), ["one"]);
    // First in the queue still, then the steering (as Stop leaves it).
    let q = queued_now(&h, &s.id).await;
    let shown: Vec<_> = q
        .iter()
        .map(|m| (m.text.as_str(), m.steer, m.interrupt))
        .collect();
    assert_eq!(shown, [("two", false, false), ("steer it", false, false)]);
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
}

#[tokio::test]
async fn messages_sent_while_it_compacts_run_once_it_is_done() {
    // Enough history for a checkpoint.
    let big = |c: char| {
        Reply::Tools(vec![(
            "bash".into(),
            json!({"command": format!("python3 -c 'print(\"{c}\" * 20000)'")}),
        )])
    };
    let h = harness_agent(
        vec![
            big('a'),
            Reply::Text("one done".into()),
            big('b'),
            Reply::Text("two done".into()),
            Reply::Text("three done".into()),
        ],
        100_000,
        "[security]\nsandbox = 'off'\ndefault_permission = 'auto'\n",
    )
    .await;
    h.mock
        .state
        .summary_delay_ms
        .store(800, std::sync::atomic::Ordering::Relaxed);
    let s = h.session().await;
    h.send(&s.id, "one").await;
    h.settle(&s.id).await;
    h.send(&s.id, "two").await;
    h.settle(&s.id).await;
    h.engine
        .handle(
            Request::Compact {
                session_id: s.id.clone(),
                instructions: None,
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Compacting);
    h.send(&s.id, "three").await;
    assert_eq!(queued_now(&h, &s.id).await.len(), 1);
    wait_for_users(&h, &s.id, ["one", "two", "three"]).await;
    h.settle(&s.id).await;
    assert_eq!(last_reply(&h.entries(&s.id).await), "three done");
    assert!(queued_now(&h, &s.id).await.is_empty());
}
