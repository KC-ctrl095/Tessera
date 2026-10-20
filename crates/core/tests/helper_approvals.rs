//! A helper waiting on an approval is never invisible to the chat that
//! started it: that chat is told at once, sees the wait in `agents status`,
//! may decline it, and may approve it only where it has that authority
//! itself. Against the scripted mock model.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_proto::*;

async fn wait_for(
    h: &Harness,
    id: &str,
    what: &str,
    pred: impl Fn(&[Entry]) -> bool,
) -> Vec<Entry> {
    for _ in 0..400 {
        let entries = h.entries(id).await;
        if pred(&entries) {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}: {:#?}", h.entries(id).await);
}

/// The harness note the parent got about a waiting helper.
fn notes(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a == "harness" => Some(plain_text(content)),
            _ => None,
        })
        .filter(|t| t.starts_with("[Helper \""))
        .collect()
}

fn result_of(entries: &[Entry], tool: &str, needle: &str) -> Option<(bool, String)> {
    tool_results(entries)
        .into_iter()
        .find(|(t, _, text)| t == tool && text.contains(needle))
        .map(|(_, err, text)| (err, text))
}

async fn child(h: &Harness, parent: &str) -> String {
    for _ in 0..200 {
        if let Some(c) = h.engine.store().children_of(parent).unwrap().first() {
            return c.id.clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no helper started");
}

async fn approval(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> (String, ApprovalRequest) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::ApprovalRequested {
                session_id,
                request,
            }) = rx.recv().await
            {
                return (session_id, request);
            }
        }
    })
    .await
    .expect("no approval was requested")
}

/// The helper asks the user about a command its parent couldn't run without
/// asking either: the parent is told at once and sees it in `agents status`;
/// approving is refused (the user's card stays), declining unblocks the
/// helper with a plain message.
#[tokio::test]
async fn a_parent_is_told_of_its_helpers_wait_and_may_decline_it() {
    let h = harness(vec![], 100_000).await;
    // The parent starts the helper, then works a few seconds (the helper
    // asks meanwhile, so the note is its next step's input).
    h.mock.route(
        "<WAIT-PARENT>",
        Reply::Tools(vec![
            (
                "agent".into(),
                json!({"tasks": [{"name": "Cleaner", "prompt": "Tidy the folder <WAIT-HELPER>"}]}),
            ),
            ("bash".into(), json!({"command": "sleep 3"})),
        ]),
    );
    h.mock.route(
        "<WAIT-PARENT>",
        Reply::Tools(vec![("agents".into(), json!({"action": "status"}))]),
    );
    h.mock.route(
        "<WAIT-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "approve", "name": "Cleaner"}),
        )]),
    );
    h.mock.route(
        "<WAIT-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "decline", "name": "Cleaner"}),
        )]),
    );
    h.mock
        .route("<WAIT-PARENT>", Reply::Text("Declined it.".into()));
    h.mock.route(
        "<WAIT-HELPER>",
        Reply::Tools(vec![("bash".into(), json!({"command": "git clean -nf"}))]),
    );
    h.mock.route(
        "<WAIT-HELPER>",
        Reply::Text("Not tidied: the clean-up was declined.".into()),
    );
    let mut rx = h.engine.subscribe();
    let chat = h.session().await;
    h.send(&chat.id, "Tidy up with a helper <WAIT-PARENT>")
        .await;
    let (shown_on, req) = approval(&mut rx).await;
    assert_eq!(shown_on, chat.id, "the user's card is in the parent chat");
    assert_eq!(req.tool, "bash");
    let kid = child(&h, &chat.id).await;
    let entries = wait_for(&h, &chat.id, "the decline", |e| {
        result_of(e, "agents", "Declined").is_some()
    })
    .await;
    let told = notes(&entries);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("Helper \"Cleaner\" waits for the user's approval ")
            && told[0].contains(": bash git clean -nf"),
        "{}",
        told[0]
    );
    assert!(
        told[0].contains("only the user can approve it"),
        "{}",
        told[0]
    );
    let (err, status) = result_of(&entries, "agents", "subagent(s)").unwrap();
    assert!(!err);
    assert!(
        status.contains("waits for approval ")
            && status.contains(": bash git clean -nf")
            && status.contains("only the user can approve it"),
        "{status}"
    );
    let (err, refused) = result_of(&entries, "agents", "can't approve").unwrap();
    assert!(err, "{refused}");
    assert!(refused.contains("The user's card stays open"), "{refused}");
    // The helper is told who declined, and carries on to its report.
    let kid_entries = wait_for(&h, &kid, "the helper's answer", |e| {
        result_of(e, "bash", "declined").is_some()
    })
    .await;
    let (err, text) = result_of(&kid_entries, "bash", "declined").unwrap();
    assert!(err);
    assert!(
        text.contains("The chat that started you declined this bash action"),
        "{text}"
    );
    assert!(
        !text.contains("STOP") && !text.contains("ask in the chat"),
        "{text}"
    );
    // The user's card is gone.
    let resolved = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::ApprovalResolved {
                request_id,
                decision,
                ..
            }) = rx.recv().await
                && request_id == req.id
            {
                return decision;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(resolved, ApprovalDecision::Deny);
    h.settle(&kid).await;
    h.settle(&chat.id).await;
}

/// Where the parent has the authority itself (the user allowed this command
/// there for the session), it may approve its helper's request.
#[tokio::test]
async fn a_parent_with_the_authority_may_approve_its_helpers_wait() {
    let h = harness(vec![], 100_000).await;
    h.mock.route(
        "<GRANT-PARENT>",
        Reply::Tools(vec![("bash".into(), json!({"command": "git clean -nf"}))]),
    );
    h.mock.route(
        "<GRANT-PARENT>",
        Reply::Tools(vec![
            (
                "agent".into(),
                json!({"tasks": [{"name": "Cleaner", "prompt": "Tidy the folder <GRANT-HELPER>"}]}),
            ),
            ("bash".into(), json!({"command": "sleep 3"})),
        ]),
    );
    h.mock.route(
        "<GRANT-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "approve", "name": "Cleaner"}),
        )]),
    );
    h.mock
        .route("<GRANT-PARENT>", Reply::Text("Approved it.".into()));
    h.mock.route(
        "<GRANT-HELPER>",
        Reply::Tools(vec![("bash".into(), json!({"command": "git clean -nf"}))]),
    );
    h.mock
        .route("<GRANT-HELPER>", Reply::Text("Tidied.".into()));
    let mut rx = h.engine.subscribe();
    let chat = h.session().await;
    h.send(&chat.id, "Tidy up, then with a helper <GRANT-PARENT>")
        .await;
    // The user allows the command in the parent chat for the session.
    let (_, req) = approval(&mut rx).await;
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::AllowSession,
            },
            None,
        )
        .await
        .unwrap();
    // The helper's own request: the parent answers it.
    let (_, helper_req) = approval(&mut rx).await;
    assert_eq!(helper_req.tool, "bash");
    let kid = child(&h, &chat.id).await;
    let entries = wait_for(&h, &chat.id, "the approval", |e| {
        result_of(e, "agents", "Approved").is_some()
    })
    .await;
    let told = notes(&entries);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("you may answer it"), "{}", told[0]);
    let (err, text) = result_of(&entries, "agents", "Approved").unwrap();
    assert!(!err, "{text}");
    let kid_entries = wait_for(&h, &kid, "the helper's command", |e| {
        tool_results(e).iter().any(|(t, _, _)| t == "bash")
    })
    .await;
    let (_, _, ran) = tool_results(&kid_entries)
        .into_iter()
        .find(|(t, _, _)| t == "bash")
        .unwrap();
    assert!(
        !ran.contains("Not approved") && !ran.contains("declined"),
        "{ran}"
    );
    h.settle(&kid).await;
    h.settle(&chat.id).await;
}

/// A parent in YOLO may approve its helper's request: its authority counts
/// as it is when it answers (here raised to YOLO after the helper asked).
#[tokio::test]
async fn a_yolo_parent_may_approve_its_helpers_wait() {
    let h = harness(vec![], 100_000).await;
    h.mock.route(
        "<YOLO-PARENT>",
        Reply::Tools(vec![
            (
                "agent".into(),
                json!({"tasks": [{"name": "Cleaner", "prompt": "Tidy the folder <YOLO-HELPER>"}]}),
            ),
            ("bash".into(), json!({"command": "sleep 3"})),
        ]),
    );
    h.mock.route(
        "<YOLO-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "approve", "name": "Cleaner"}),
        )]),
    );
    h.mock
        .route("<YOLO-PARENT>", Reply::Text("Approved it.".into()));
    h.mock.route(
        "<YOLO-HELPER>",
        Reply::Tools(vec![("bash".into(), json!({"command": "git clean -nf"}))]),
    );
    h.mock.route("<YOLO-HELPER>", Reply::Text("Tidied.".into()));
    let mut rx = h.engine.subscribe();
    let chat = h.session().await;
    h.send(&chat.id, "Tidy up with a helper <YOLO-PARENT>")
        .await;
    let (_, req) = approval(&mut rx).await;
    assert_eq!(req.tool, "bash");
    // The user puts the parent in YOLO while it works.
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: chat.id.clone(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(PermissionMode::Auto),
                pinned: None,
                archived: None,
                cwd: None,
                members: None,
                fast: None,
                subagents: None,
            },
            None,
        )
        .await
        .unwrap();
    let kid = child(&h, &chat.id).await;
    let entries = wait_for(&h, &chat.id, "the approval", |e| {
        result_of(e, "agents", "Approved").is_some() || result_of(e, "agents", "can't").is_some()
    })
    .await;
    let (err, text) = result_of(&entries, "agents", "Approved").expect("approved");
    assert!(!err && text.contains("bash git clean -nf"), "{text}");
    let kid_entries = wait_for(&h, &kid, "the helper's command", |e| {
        tool_results(e).iter().any(|(t, _, _)| t == "bash")
    })
    .await;
    let (_, _, ran) = tool_results(&kid_entries)
        .into_iter()
        .find(|(t, _, _)| t == "bash")
        .unwrap();
    assert!(
        !ran.contains("Not approved") && !ran.contains("declined"),
        "{ran}"
    );
    h.settle(&kid).await;
    h.settle(&chat.id).await;
}

/// The parent was in YOLO when its helper asked ("you may answer it"), then
/// the user lowered it to Default: approving is refused, as in its own chat.
#[tokio::test]
async fn a_parent_lowered_after_its_helper_asked_may_no_longer_approve() {
    let h = harness(vec![], 100_000).await;
    h.mock.route(
        "<LOWER-PARENT>",
        Reply::Tools(vec![
            (
                "agent".into(),
                json!({"tasks": [{"name": "Cleaner", "prompt": "Tidy the folder <LOWER-HELPER>"}]}),
            ),
            ("bash".into(), json!({"command": "sleep 5"})),
        ]),
    );
    h.mock.route(
        "<LOWER-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "approve", "name": "Cleaner"}),
        )]),
    );
    h.mock.route(
        "<LOWER-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "decline", "name": "Cleaner"}),
        )]),
    );
    h.mock
        .route("<LOWER-PARENT>", Reply::Text("Declined it.".into()));
    // The helper works a moment first (unasked), then asks.
    h.mock.route(
        "<LOWER-HELPER>",
        Reply::Tools(vec![("bash".into(), json!({"command": "sleep 2"}))]),
    );
    h.mock.route(
        "<LOWER-HELPER>",
        Reply::Tools(vec![("bash".into(), json!({"command": "git clean -nf"}))]),
    );
    h.mock
        .route("<LOWER-HELPER>", Reply::Text("Not tidied.".into()));
    let mut rx = h.engine.subscribe();
    let chat = h.session().await;
    h.send(&chat.id, "Tidy up with a helper <LOWER-PARENT>")
        .await;
    let kid = child(&h, &chat.id).await;
    let mode = |m: PermissionMode| Request::UpdateSession {
        session_id: chat.id.clone(),
        title: None,
        model: None,
        thinking: None,
        permission: Some(m),
        pinned: None,
        archived: None,
        cwd: None,
        members: None,
        fast: None,
        subagents: None,
    };
    // YOLO before the helper asks; lowered again before the parent answers.
    h.engine
        .handle(mode(PermissionMode::Auto), None)
        .await
        .unwrap();
    let (_, req) = approval(&mut rx).await;
    assert_eq!(req.tool, "bash");
    h.engine
        .handle(mode(PermissionMode::Default), None)
        .await
        .unwrap();
    let entries = wait_for(&h, &chat.id, "the decline", |e| {
        result_of(e, "agents", "Declined").is_some()
    })
    .await;
    let told = notes(&entries);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("You could do this yourself"),
        "{}",
        told[0]
    );
    let (err, refused) = result_of(&entries, "agents", "can't approve").unwrap();
    assert!(err, "{refused}");
    let kid_entries = wait_for(&h, &kid, "the helper's answer", |e| {
        result_of(e, "bash", "declined").is_some()
    })
    .await;
    let (err, text) = result_of(&kid_entries, "bash", "declined").unwrap();
    assert!(
        err && text.contains("The chat that started you declined"),
        "{text}"
    );
    h.settle(&kid).await;
    h.settle(&chat.id).await;
}
