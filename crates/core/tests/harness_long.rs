//! Long-running work: compaction that doesn't stall the agent, a ledger that
//! doesn't defeat prompt caching, replies cut off mid-stream.
//! Regression tests for the "stuck compacting for 30 minutes" chat.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_proto::*;

/// A step that grows what compaction can't trim (stage 1 only stubs tool
/// output, never the agent's own calls), and takes a little while.
fn step(i: usize, pad: usize) -> Reply {
    Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("sleep 0.15; echo step{i} # {}", "x".repeat(pad))}),
    )])
}

fn compactions(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|e| matches!(e.body, EntryBody::Compaction { .. }))
        .count()
}

fn notices(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Every status the session went through, recorded from the event stream.
fn watch_statuses(h: &Harness, id: &str) -> std::sync::Arc<parking_lot::Mutex<Vec<SessionStatus>>> {
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut rx = h.engine.subscribe();
    let (id, out) = (id.to_string(), seen.clone());
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Event::SessionUpdated { session } = ev
                && session.id == id
            {
                let mut v = out.lock();
                if v.last() != Some(&session.status) {
                    v.push(session.status);
                }
            }
        }
    });
    seen
}

async fn abort(h: &Harness, id: &str) {
    h.engine
        .handle(
            Request::Abort {
                session_id: id.into(),
            },
            None,
        )
        .await
        .unwrap();
}

async fn settle_long(h: &Harness, id: &str) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(90), h.engine.wait_idle(id))
        .await
        .expect("session did not become idle");
}

#[tokio::test]
async fn checkpoints_are_written_while_the_agent_keeps_working() {
    let mut script: Vec<Reply> = (0..34).map(|i| step(i, 4_000)).collect();
    script.push(Reply::Text("finished".into()));
    let h = harness(script, 40_000).await;
    // A slow summariser (real ones take minutes on long histories).
    h.mock.state.summary_delay_ms.store(700, Ordering::Relaxed);
    let s = h.session().await;
    let statuses = watch_statuses(&h, &s.id);
    h.send(&s.id, "build it; remember marker-QQ").await;
    settle_long(&h, &s.id).await;

    let entries = h.entries(&s.id).await;
    assert!(
        compactions(&entries) >= 1,
        "notices: {:?}",
        notices(&entries)
    );
    assert!(
        !statuses.lock().contains(&SessionStatus::Compacting),
        "the agent never waited for a checkpoint: {:?}",
        statuses.lock()
    );
    // The agent made requests while a checkpoint was being written.
    let timeline = h.mock.state.timeline.lock().clone();
    let start = timeline.iter().position(|t| *t == "summary").unwrap();
    let done = start
        + timeline[start..]
            .iter()
            .position(|t| *t == "summary done")
            .unwrap();
    assert!(
        timeline[start..done].contains(&"agent"),
        "no agent step overlapped the summary: {timeline:?}"
    );
    let last = h.mock.agent_requests().pop().unwrap();
    let text = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap()["content"]
        .to_string();
    assert!(text.contains("[Context checkpoint]"), "{text}");
    assert!(text.contains("marker-QQ"), "the user's words survive");
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
    // No summary was thrown away: each made a checkpoint, except perhaps
    // the next one, already being written.
    assert!(
        h.mock.summary_requests() <= compactions(&entries) + 1,
        "{} summaries for {} checkpoints",
        h.mock.summary_requests(),
        compactions(&entries)
    );
}

#[tokio::test]
async fn when_the_context_fills_first_the_agent_waits_for_the_checkpoint() {
    // Big steps: from under the trigger to past the hard limit in two steps.
    let mut script: Vec<Reply> = (0..6).map(|i| step(i, 18_000)).collect();
    script.push(Reply::Text("finished".into()));
    let h = harness(script, 40_000).await;
    h.mock.state.summary_delay_ms.store(500, Ordering::Relaxed);
    let s = h.session().await;
    let statuses = watch_statuses(&h, &s.id);
    h.send(&s.id, "go").await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(compactions(&entries) >= 1);
    assert!(
        statuses.lock().contains(&SessionStatus::Compacting),
        "{:?}",
        statuses.lock()
    );
    assert!(
        !notices(&entries).iter().any(|n| n.contains("failed")),
        "{:?}",
        notices(&entries)
    );
}

#[tokio::test]
async fn stopping_during_compaction_saves_no_checkpoint() {
    let h = harness(
        vec![step(0, 20_000), step(1, 20_000), Reply::Text("done".into())],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "work").await;
    settle_long(&h, &s.id).await;

    // A manual compaction, stopped while the summariser writes.
    h.mock
        .state
        .summary_delay_ms
        .store(3_000, Ordering::Relaxed);
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Compacting);
    abort(&h, &s.id).await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    assert_eq!(compactions(&entries), 0, "{:?}", notices(&entries));
    let n = notices(&entries);
    assert!(
        !n.iter()
            .any(|t| t.contains("summariser failed") || t.contains("Compaction failed")),
        "{n:?}"
    );
    assert_eq!(n.last().map(String::as_str), Some("Stopped."));
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
}

#[tokio::test]
async fn stopping_a_turn_that_waits_for_its_checkpoint_saves_none() {
    let mut script: Vec<Reply> = (0..6).map(|i| step(i, 18_000)).collect();
    script.push(Reply::Text("finished".into()));
    let h = harness(script, 40_000).await;
    h.mock
        .state
        .summary_delay_ms
        .store(4_000, Ordering::Relaxed);
    let s = h.session().await;
    h.send(&s.id, "go").await;
    // Wait until it waits.
    for _ in 0..100 {
        if h.info(&s.id).await.status == SessionStatus::Compacting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Compacting);
    abort(&h, &s.id).await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    assert_eq!(compactions(&entries), 0);
    let n = notices(&entries);
    assert!(!n.iter().any(|t| t.contains("failed")), "{n:?}");
    assert_eq!(n.last().map(String::as_str), Some("Stopped."));
}

#[tokio::test]
async fn ledger_updates_after_a_checkpoint_keep_the_prompt_prefix_stable() {
    let task = |n: usize| {
        Reply::Tools(vec![(
            "task".into(),
            json!({"add_notes": [format!("note {n}")], "update": [{"index": 1, "status": "in_progress"}]}),
        )])
    };
    let mut script = vec![Reply::Tools(vec![(
        "task".into(),
        json!({"goal": "Ship the widget", "plan": [{"text": "build"}, {"text": "test"}], "constraints": ["never touch prod"]}),
    )])];
    script.extend((0..30).map(|i| step(i, 4_000)));
    // Once compacted: ledger updates, then a plain step.
    for i in 0..3 {
        script.push(task(i));
        script.push(step(100 + i, 10));
    }
    script.push(Reply::Text("finished".into()));
    let h = harness(script, 40_000).await;
    let s = h.session().await;
    h.send(&s.id, "ship it").await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(compactions(&entries) >= 1);

    // The requests after the last checkpoint all start the same way.
    let reqs = h.mock.agent_requests();
    let first = |r: &Value| {
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap()["content"]
            .to_string()
    };
    let tail: Vec<String> = reqs[reqs.len() - 7..].iter().map(first).collect();
    assert!(tail[0].contains("[Context checkpoint]"), "{}", tail[0]);
    assert!(tail[0].contains("never touch prod"), "the ledger is shown");
    assert!(
        tail.windows(2).all(|w| w[0] == w[1]),
        "a ledger update changed the start of the prompt"
    );
    // The agent still saw each update, in its own calls.
    let body = reqs.last().unwrap().to_string();
    assert!(body.contains("note 2"));
}

#[tokio::test]
async fn the_task_tool_confirms_briefly_and_keeps_its_lists_bounded() {
    let notes: Vec<String> = (0..45)
        .map(|i| format!("finding {i}: {}", "normal readable words ".repeat(4)))
        .collect();
    let mut kept = notes.clone();
    while kept.iter().map(|s| s.trim_end().len() + 4).sum::<usize>() > 2_048 {
        kept.remove(0);
    }
    let archived = notes.len() - kept.len();
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "task".into(),
                json!({"goal": "Audit", "plan": [{"text": "scan", "status": "done"}, {"text": "report"}], "done_criteria": ["report written"]}),
            )]),
            Reply::Tools(vec![("task".into(), json!({"add_notes": notes}))]),
            Reply::Tools(vec![("task".into(), json!({}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "audit").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(
        results[0].2,
        "Ledger saved. Plan 1/2 done; next: 2. report. Done when: 0/1 met."
    );
    assert!(results[1].2.len() < 300, "{}", results[1].2);
    assert!(
        results[1]
            .2
            .contains(&format!("{archived} older items archived losslessly")),
        "{}",
        results[1].2
    );
    // No arguments: the whole ledger.
    assert!(results[2].2.contains("Goal: Audit"), "{}", results[2].2);
    assert!(results[2].2.contains("finding 44") && !results[2].2.contains("finding 0:"));
    assert!(results[2].2.contains("use recall artifact="));
    let ledger = h.info(&s.id).await.task.unwrap();
    assert_eq!(
        ledger.notes,
        kept.into_iter()
            .map(|s| s.trim_end().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(ledger.archived["notes"].count, archived as u64);
}

#[tokio::test]
async fn a_reply_cut_off_mid_stream_is_retried_and_stays_visible() {
    let h = harness(
        vec![
            Reply::Cut("Here is what I found so far".into()),
            Reply::Text("The full answer".into()),
            Reply::Text("second turn".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "question").await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let replies: Vec<(String, StopReason, bool)> = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant {
                content,
                stop,
                error,
                ..
            } => Some((
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect(),
                *stop,
                error.is_some(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        vec![
            (
                "Here is what I found so far".into(),
                StopReason::Aborted,
                true
            ),
            ("The full answer".into(), StopReason::Stop, false),
        ]
    );
    assert!(notices(&entries).iter().any(|n| n.contains("retrying")));
    // The model never sees the cut-off text as something it said.
    h.send(&s.id, "next").await;
    settle_long(&h, &s.id).await;
    let last = h.mock.agent_requests().pop().unwrap().to_string();
    assert!(last.contains("The full answer"));
    assert!(!last.contains("found so far"));
}

fn harness_notes(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a == "harness" => Some(plain_text(content)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn long_turns_check_in_at_the_step_limit_instead_of_stopping() {
    let mut script: Vec<Reply> = (0..8)
        .map(|i| {
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": format!("echo part{i}")}),
            )])
        })
        .collect();
    script.push(Reply::Text("all parts built".into()));
    let h = harness_agent(script, 100_000, "max_steps = 3").await;
    let s = h.session().await;
    h.send(&s.id, "build all the parts").await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let notes = harness_notes(&entries);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].starts_with("[Progress check] 3 steps into this turn"));
    assert!(notes[1].starts_with("[Progress check] 6 steps"));
    let last = entries.iter().rev().find_map(|e| match &e.body {
        EntryBody::Assistant { content, .. } => Some(format!("{content:?}")),
        _ => None,
    });
    assert!(
        last.unwrap().contains("all parts built"),
        "the work finished"
    );
    assert!(!notices(&entries).iter().any(|n| n.contains("Paused")));
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
}

#[tokio::test]
async fn a_turn_going_in_circles_pauses_at_the_step_limit() {
    let script: Vec<Reply> = (0..12)
        .map(|_| {
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "printf 'No such directory\\n' >&2; exit 2"}),
            )])
        })
        .collect();
    let h = harness_agent(script, 100_000, "max_steps = 4").await;
    let s = h.session().await;
    h.send(&s.id, "list it").await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let n = notices(&entries);
    assert!(
        n.iter()
            .any(|t| t.starts_with("Paused after 4 steps") && t.contains("going in circles")),
        "{n:?}"
    );
    assert_eq!(h.mock.agent_requests().len(), 4);
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
}

#[tokio::test]
async fn a_message_to_a_running_subagent_reaches_it_at_its_next_step() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"tasks": [{"name": "gamma", "prompt": "probe topic GAMMA"}]}),
            )]),
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "message", "name": "gamma", "text": "the service listens on port 9"}),
            )]),
            Reply::Text("Started gamma.".into()),
            Reply::Text("Combined: gamma used port 9.".into()),
        ],
        100_000,
    )
    .await;
    h.mock.route(
        "topic GAMMA",
        Reply::Tools(vec![("bash".into(), json!({"command": "sleep 1"}))]),
    );
    h.mock
        .route("topic GAMMA", Reply::Text("GAMMA done on port 9".into()));
    let s = h.session().await;
    h.send(&s.id, "probe it").await;
    let mut done = false;
    for _ in 0..200 {
        let entries = h.entries(&s.id).await;
        if entries.iter().any(|e| {
            matches!(&e.body, EntryBody::User { content, author: Some(a) }
                if a.starts_with("subagent:") && plain_text(content).contains("GAMMA done"))
        }) {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(done, "gamma reported");
    let results = tool_results(&h.entries(&s.id).await);
    let sent = results.iter().find(|r| r.0 == "agents").unwrap();
    assert!(
        !sent.1 && sent.2.starts_with("Delivered to \"gamma\" while it works"),
        "{sent:?}"
    );
    // The subagent's next request carried the message.
    let reqs = h.mock.state.requests.lock().clone();
    assert!(
        reqs.iter().any(|r| {
            let b = r.to_string();
            b.contains("topic GAMMA")
                && b.contains(
                    "[Message from the agent that started you] the service listens on port 9",
                )
        }),
        "the message never reached the subagent"
    );
}
