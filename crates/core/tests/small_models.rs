//! How the agent loop copes with what small local models (and some servers)
//! send: tool calls written as text, other harnesses' tool names and
//! argument shapes, calls streamed without an `index`, empty replies,
//! replies cut off at the output limit, and repeated failures while fixing.

mod common;

use common::*;
use serde_json::{Value, json};
use tessera_proto::*;

fn text(t: &str, finish: &str) -> Reply {
    Reply::Raw(vec![
        json!({"choices": [{"index": 0, "delta": {"content": t}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}], "usage": {"prompt_tokens": 100, "completion_tokens": 10}}),
    ])
}

fn calls_made(entries: &[Entry]) -> Vec<(String, Value)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            AssistantPart::ToolCall {
                name, arguments, ..
            } => Some((name.clone(), arguments.clone())),
            _ => None,
        })
        .collect()
}

fn results(entries: &[Entry]) -> Vec<(String, String, bool)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                content,
                is_error,
                ..
            } => Some((tool.clone(), plain_text(content), *is_error)),
            _ => None,
        })
        .collect()
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
async fn a_call_written_as_text_runs() {
    let h = harness(
        vec![
            text(
                "Let me check.\n<tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"echo from-text-call\"}}\n</tool_call>",
                "stop",
            ),
            text("It printed from-text-call.", "stop"),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "run echo").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_eq!(
        calls_made(&entries),
        vec![("bash".into(), json!({"command": "echo from-text-call"}))]
    );
    let r = results(&entries);
    assert!(r[0].1.contains("from-text-call") && !r[0].2, "{r:?}");
    // The model's next request shows a real call (and no raw tags).
    let reqs = h.mock.agent_requests();
    let second = reqs[1].to_string();
    assert!(
        second.contains("tool_calls") && !second.contains("<tool_call>"),
        "{second}"
    );
    assert_eq!(h.info(&s.id).await.preview, "It printed from-text-call.");
}

#[tokio::test]
async fn other_harnesses_names_and_shapes_run_as_the_tool_takes_them() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "Write".into(),
                json!({"file_path": "notes.txt", "contents": "alpha\nbeta\n"}),
            )]),
            Reply::Tools(vec![(
                "functions.read".into(),
                json!({"file_path": "notes.txt"}),
            )]),
            Reply::Tools(vec![(
                "Edit".into(),
                json!({"file_path": "notes.txt", "old_string": "beta", "new_string": "gamma"}),
            )]),
            Reply::Text("Done.".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "write, read and edit notes.txt").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let names: Vec<String> = calls_made(&entries).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["write", "read", "edit"]);
    let r = results(&entries);
    assert!(r.iter().all(|(_, _, err)| !err), "{r:?}");
    let cwd = h.info(&s.id).await.cwd;
    let written = std::fs::read_to_string(std::path::Path::new(&cwd).join("notes.txt")).unwrap();
    assert_eq!(written, "alpha\ngamma\n");
}

#[tokio::test]
async fn calls_streamed_without_an_index_stay_apart() {
    let h = harness(
        vec![
            Reply::Raw(vec![
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"id": "a1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\": "}}]}}]}),
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"function": {"arguments": "\"echo one\"}"}}]}}]}),
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"id": "b2", "type": "function", "function": {"name": "bash", "arguments": "{\"command\": \"echo two\"}"}}]}}]}),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
            ]),
            text("Both ran.", "stop"),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "two commands").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_eq!(
        calls_made(&entries),
        vec![
            ("bash".into(), json!({"command": "echo one"})),
            ("bash".into(), json!({"command": "echo two"}))
        ]
    );
}

#[tokio::test]
async fn an_empty_reply_gets_one_nudge() {
    let h = harness(
        vec![
            Reply::Raw(vec![
                json!({"choices": [{"index": 0, "delta": {"reasoning_content": "Hmm, I will answer."}}]}),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
            ]),
            text("Paris.", "stop"),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "capital of France?").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let notes = harness_notes(&entries);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("[Empty reply]"), "{}", notes[0]);
    assert_eq!(h.info(&s.id).await.preview, "Paris.");
    assert_eq!(h.mock.agent_requests().len(), 2);

    // Twice in a row: one nudge, then the turn ends with a notice.
    let empty = || {
        Reply::Raw(vec![
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ])
    };
    h.mock.push(empty());
    h.mock.push(empty());
    h.send(&s.id, "and Spain?").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_eq!(harness_notes(&entries).len(), 2);
    assert_eq!(h.mock.agent_requests().len(), 4);
    assert!(
        entries.iter().any(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text.contains("without saying anything"))),
    );
}

#[tokio::test]
async fn a_reply_cut_at_the_limit_continues_once_and_local_requests_carry_the_limit() {
    let h = harness(
        vec![
            text("Step one is done; step two", "length"),
            text(" is done too.", "stop"),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "explain").await;
    h.settle(&s.id).await;
    let notes = harness_notes(&h.entries(&s.id).await);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("[Continue]"), "{}", notes[0]);
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 2);
    // The mock is on this machine: its model's output limit is sent.
    assert_eq!(reqs[0]["max_tokens"], json!(2000));
}

#[tokio::test]
async fn a_call_cut_at_the_limit_says_so_and_goes_back_as_an_empty_object() {
    let h = harness(
        vec![
            Reply::Raw(vec![
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "w1", "type": "function", "function": {"name": "write", "arguments": "{\"path\": \"big.txt\", \"content\": \"aaaa"}}]}}]}),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "length"}]}),
            ]),
            text("I'll write it in parts.", "stop"),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "write a big file").await;
    h.settle(&s.id).await;
    let r = results(&h.entries(&s.id).await);
    assert!(
        r[0].2 && r[0].1.contains("reached the output limit inside this call"),
        "{r:?}"
    );
    let second = h.mock.agent_requests()[1].clone();
    let call = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|m| m["tool_calls"].as_array().and_then(|c| c.first()).cloned())
        .unwrap();
    assert_eq!(call["function"]["arguments"], "{}");
}

#[tokio::test]
async fn fixing_and_testing_again_is_not_a_loop() {
    // The same failing command six times, with an edit between each try:
    // never blocked, never a loop warning.
    let mut script = vec![Reply::Tools(vec![(
        "write".into(),
        json!({"path": "fix.txt", "content": "0\n"}),
    )])];
    for i in 1..=6 {
        script.push(Reply::Tools(vec![(
            "bash".into(),
            json!({"command": "false"}),
        )]));
        script.push(Reply::Tools(vec![(
            "write".into(),
            json!({"path": "fix.txt", "content": format!("{i}\n")}),
        )]));
    }
    script.push(Reply::Text("Gave up for now.".into()));
    let h = harness(script, 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "fix it").await;
    h.settle(&s.id).await;
    let r = results(&h.entries(&s.id).await);
    let bash: Vec<&(String, String, bool)> = r.iter().filter(|(t, _, _)| t == "bash").collect();
    assert_eq!(bash.len(), 6);
    assert!(
        bash.iter()
            .all(|(_, out, _)| !out.contains("Blocked") && !out.contains("Loop warning")),
        "{bash:?}"
    );

    // Without the edits in between it is blocked as before.
    let mut script = Vec::new();
    for _ in 0..6 {
        script.push(Reply::Tools(vec![(
            "bash".into(),
            json!({"command": "false"}),
        )]));
    }
    script.push(Reply::Text("Stopped.".into()));
    let h = harness(script, 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "try it").await;
    h.settle(&s.id).await;
    let r = results(&h.entries(&s.id).await);
    assert!(r.last().unwrap().1.contains("Blocked"), "{r:?}");
}

/// Trying one thing after another that fails (sites refusing, commands
/// that don't apply) is work, not circling: no pause, one nudge.
#[tokio::test]
async fn different_calls_failing_in_a_row_are_not_a_loop() {
    let mut script = Vec::new();
    for i in 0..10 {
        script.push(Reply::Tools(vec![(
            "bash".into(),
            json!({"command": format!("echo site{i} refused; exit 1")}),
        )]));
    }
    script.push(Reply::Text(
        "None of them worked; here is what I found instead.".into(),
    ));
    let h = harness(script, 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "try the sites").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(
        !entries
            .iter()
            .any(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text.contains("Paused"))),
        "paused"
    );
    assert_eq!(
        h.info(&s.id).await.preview,
        "None of them worked; here is what I found instead."
    );
    let notes = results(&entries)
        .iter()
        .filter(|(_, out, _)| out.contains("in a row"))
        .count();
    assert_eq!(notes, 1, "one nudge");
}
