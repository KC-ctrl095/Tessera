//! Stored values are removed before result persistence, events and model input.
#![cfg(unix)]

mod common;
use common::*;
use serde_json::json;
use tessera_proto::*;

const VALUE: &str = "fixture-value-123456";
const UTF8: &str = "caf\u{e9}-\u{1f33b}-value";

async fn fixture() -> Harness {
    let h = harness_agent(
        vec![],
        100_000,
        "[security]\nsandbox = 'off'\nredact_patterns = false",
    )
    .await;
    h.engine.auth().set_secret("LAB_VALUE", VALUE).unwrap();
    h.engine.auth().set_secret("LAB_UTF8", UTF8).unwrap();
    h
}

async fn auto(h: &Harness) -> SessionInfo {
    let s = h.session().await;
    let mut request = common_update(&s.id);
    if let Request::UpdateSession { permission, .. } = &mut request {
        *permission = Some(PermissionMode::Auto);
    }
    h.engine.handle(request, None).await.unwrap();
    s
}

fn common_update(id: &str) -> Request {
    serde_json::from_value(json!({"method":"update_session", "params":{"session_id":id}})).unwrap()
}

fn assert_clean(value: &impl serde::Serialize) {
    assert!(!serde_json::to_string(value).unwrap().contains(VALUE));
    assert!(!serde_json::to_string(value).unwrap().contains(UTF8));
}

#[tokio::test]
async fn edit_context_and_echoed_arguments_never_reach_transcripts_or_events() {
    let h = fixture().await;
    let s = auto(&h).await;
    std::fs::write(h.work.join("notes.txt"), format!("{VALUE}\nold\nlast\n")).unwrap();
    h.mock.push(Reply::Tools(vec![(
        "edit".into(),
        json!({"path":"notes.txt", "replace":[{"old":"old", "new":"new"}]}),
    )]));
    // A model's echoed arguments also need scrubbing. This does not change
    // execution's separate arguments or the file the user asked to write.
    h.mock.push(Reply::Tools(vec![(
        "write".into(),
        json!({"path":"copy.txt", "content":VALUE}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "Update the middle line and keep the copy.")
        .await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_clean(&entries);
    let diff = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                details: Some(d),
                ..
            } if tool == "edit" => d["diff"].as_str(),
            _ => None,
        })
        .unwrap();
    assert!(diff.contains("[REDACTED]") && diff.contains("+new"));
    assert_eq!(
        std::fs::read_to_string(h.work.join("copy.txt")).unwrap(),
        VALUE
    );
    while let Ok(event) = events.try_recv() {
        assert_clean(&event);
        if let Event::StreamDelta {
            delta: StreamDelta::ToolCall { arguments, .. },
            ..
        } = event
        {
            assert!(arguments.is_empty());
        }
    }
    for request in h.mock.agent_requests() {
        // The fixture's mock response has the value, not its next request.
        assert_clean(&request);
    }
}

#[tokio::test]
async fn bash_two_writes_at_every_byte_split_are_clean_before_progress() {
    let h = fixture().await;
    let s = auto(&h).await;
    let script = r#"import os, time
for name in ('LAB_VALUE', 'LAB_UTF8'):
    value = os.environ[name].encode()
    for fd in (1, 2):
        for split in range(len(value) + 1):
            os.write(fd, b'before ' + value[:split])
            time.sleep(.02)
            os.write(fd, value[split:] + b' after\n')
os.write(1, 'caf\u00e9 \U0001f33b\n'.encode())
"#;
    std::fs::write(h.work.join("output.py"), script).unwrap();
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command":"LAB_VALUE=$LAB_VALUE LAB_UTF8=$LAB_UTF8 python3 output.py"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "Run the output fixture.").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_clean(&entries);
    let result = tool_results(&entries);
    assert!(!result[0].1);
    assert_eq!(
        result[0].2.matches("[REDACTED]").count(),
        (VALUE.len() + UTF8.len() + 2) * 2
    );
    assert!(result[0].2.contains("caf\u{e9} \u{1f33b}"));
    let mut output = String::new();
    while let Ok(event) = events.try_recv() {
        assert_clean(&event);
        if let Event::ToolOutput { chunk, .. } = event {
            output.push_str(&chunk);
        }
    }
    assert!(!output.contains(VALUE));
    assert_eq!(
        output.matches("[REDACTED]").count(),
        (VALUE.len() + UTF8.len() + 2) * 2
    );
    for request in h.mock.agent_requests() {
        assert_clean(&request);
    }
}

#[tokio::test]
async fn background_job_output_does_not_advance_over_an_unfinished_secret() {
    let h = fixture().await;
    let s = auto(&h).await;
    std::fs::write(
        h.work.join("background.py"),
        r#"import os, time
value = os.environ['LAB_VALUE'].encode()
os.write(1, b'first\n' + value[:9])
time.sleep(3)
os.write(1, value[9:] + b'\nlast\n')
"#,
    )
    .unwrap();
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command":"LAB_VALUE=$LAB_VALUE python3 background.py", "background":true}),
    )]));
    h.mock.push(Reply::Text("started".into()));
    h.send(&s.id, "Start the output fixture in the background.")
        .await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let id = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d), ..
            } => d["job_id"].as_str().map(str::to_owned),
            _ => None,
        })
        .unwrap();
    h.mock.push(Reply::Tools(vec![(
        "job".into(),
        json!({"action":"output", "id":id}),
    )]));
    h.mock.push(Reply::Text("checked".into()));
    h.send(&s.id, "Read the current output.").await;
    h.settle(&s.id).await;
    assert!(
        !tool_results(&h.entries(&s.id).await)
            .last()
            .unwrap()
            .2
            .contains(&VALUE[..9])
    );
    h.mock.push(Reply::Tools(vec![(
        "job".into(),
        json!({"action":"wait", "id":id, "timeout":10}),
    )]));
    h.mock.push(Reply::Text("finished".into()));
    h.send(&s.id, "Wait for the remaining output.").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_clean(&entries);
    assert!(
        tool_results(&entries)
            .last()
            .unwrap()
            .2
            .contains("[REDACTED]")
    );
    for request in h.mock.agent_requests() {
        assert_clean(&request);
    }
}

async fn machine_fixture(streams: bool) {
    let h = fixture().await;
    let s = auto(&h).await;
    let (conn, mut rx) = h.engine.node_conn("fixture-node");
    h.engine
        .node_request(
            Request::NodeHello {
                name: "Fixture".into(),
                os: "linux x86_64".into(),
                version: "0.1.0".into(),
                scope: MachineScope {
                    folders: vec![FolderAccess {
                        path: "/tmp".into(),
                        write: true,
                    }],
                    commands: true,
                    ask: MachineAsk::Chat,
                    ..Default::default()
                },
                generation: 1,
                sandbox: "on".into(),
                sandbox_ok: true,
                home: Some("/tmp".into()),
                computer: None,
                host: None,
                features: None,
                guard_key: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let engine = h.engine.clone();
    let node = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Event::NodeCall { call } = event {
                for split in 0..=VALUE.len() {
                    for (pipe, chunk) in [
                        ("stdout", format!("before {}", &VALUE[..split])),
                        ("stderr", format!("before {}", &VALUE[..split])),
                        ("stdout", format!("{} after\n", &VALUE[split..])),
                        ("stderr", format!("{} after\n", &VALUE[split..])),
                    ] {
                        engine
                            .node_request(
                                Request::NodeProgress {
                                    call_id: call.call_id.clone(),
                                    chunk,
                                    stream: streams.then(|| pipe.into()),
                                },
                                &conn,
                            )
                            .await
                            .unwrap();
                    }
                }
                // A node only knows injected values. Its bounded final buffer
                // could start midway through another stored value. Rebuild it
                // from server-redacted progress, never forward this fragment.
                engine
                    .node_request(
                        Request::NodeReply {
                            call_id: call.call_id,
                            result: NodeResult {
                                output: format!(
                                    "[… earlier output dropped …]{}\n[exit 0]",
                                    &VALUE[9..]
                                ),
                                ok: true,
                                refused: false,
                                exit_code: Some(0),
                                image: None,
                            },
                        },
                        &conn,
                    )
                    .await
                    .unwrap();
                break;
            }
        }
    });
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action":"run", "machine":"Fixture", "command":"output fixture", "cwd":"/tmp"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "Run the machine output fixture.").await;
    h.settle(&s.id).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), node)
        .await
        .unwrap()
        .unwrap();
    let entries = h.entries(&s.id).await;
    assert_clean(&entries);
    let results = tool_results(&entries);
    assert!(!results[0].1, "{:?}", results);
    assert!(!results[0].2.contains(&VALUE[9..]));
    if streams {
        assert_eq!(
            results[0].2.matches("[REDACTED]").count(),
            (VALUE.len() + 1) * 2
        );
    } else {
        assert!(results[0].2.contains("Command output withheld"));
    }
    let mut output = String::new();
    while let Ok(event) = events.try_recv() {
        assert_clean(&event);
        if let Event::ToolOutput { chunk, .. } = event {
            output.push_str(&chunk);
        }
    }
    assert!(!output.contains(VALUE));
    if streams {
        assert_eq!(output.matches("[REDACTED]").count(), (VALUE.len() + 1) * 2);
    } else {
        assert!(output.is_empty());
    }
    for request in h.mock.agent_requests() {
        assert_clean(&request);
    }
}

#[tokio::test]
async fn remote_pipes_are_independent_and_final_output_is_redacted_before_truncation() {
    machine_fixture(true).await;
}

#[tokio::test]
async fn legacy_remote_interleaved_output_is_withheld_when_secrets_exist() {
    machine_fixture(false).await;
}

#[tokio::test]
async fn timeout_holds_output_until_the_background_job_can_finish_redaction() {
    let h = fixture().await;
    let s = auto(&h).await;
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command":"printf '%s' \"$LAB_VALUE\"; sleep 2", "timeout":1}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "Run the timeout fixture.").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_clean(&entries);
    let result = &tool_results(&entries)[0].2;
    assert!(result.contains("continues as background job"), "{result}");
    assert!(
        !result.contains("fixture-value"),
        "held output stays private"
    );
    let id = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d), ..
            } => d["job_id"].as_str(),
            _ => None,
        })
        .expect("background job");
    // A timeout hands the command to job management. Only its terminal
    // boundary can safely flush a suffix that might still be a secret.
    h.mock.push(Reply::Tools(vec![(
        "job".into(),
        json!({"action":"wait", "id":id, "timeout":10}),
    )]));
    h.mock.push(Reply::Text("finished".into()));
    h.send(&s.id, "Wait for the timeout fixture.").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_clean(&entries);
    assert!(tool_results(&entries)[1].2.contains("[REDACTED]"));
    let mut output = String::new();
    while let Ok(event) = events.try_recv() {
        assert_clean(&event);
        if let Event::ToolOutput { chunk, .. } = event {
            output.push_str(&chunk);
        }
    }
    assert!(output.contains("[REDACTED]"));
}
