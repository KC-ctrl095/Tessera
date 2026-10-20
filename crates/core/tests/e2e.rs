//! End-to-end engine tests against a scripted mock model.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_proto::*;

#[tokio::test]
async fn tool_loop_runs_and_persists_everything() {
    let h = harness(
        vec![
            Reply::Tools(vec![
                ("bash".into(), json!({"command": "echo hello-from-bash"})),
                ("glob".into(), json!({"pattern": "*"})),
            ]),
            Reply::Text("All done.".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("a.txt"), "x").unwrap();
    let s = h.session().await;
    h.send(&s.id, "run it").await;
    h.settle(&s.id).await;

    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    assert_eq!(results.len(), 2);
    assert!(results[0].2.contains("hello-from-bash"), "{results:?}");
    assert!(results[1].2.contains("a.txt"));
    let last = entries
        .iter()
        .rev()
        .find(|e| matches!(e.body, EntryBody::Assistant { .. }))
        .unwrap();
    match &last.body {
        EntryBody::Assistant { content, .. } => {
            assert!(
                content
                    .iter()
                    .any(|p| matches!(p, AssistantPart::Text { text } if text == "All done."))
            );
            assert!(
                content
                    .iter()
                    .any(|p| matches!(p, AssistantPart::Thinking { .. })),
                "reasoning is kept"
            );
        }
        _ => unreachable!(),
    }
    let info = h.info(&s.id).await;
    assert_eq!(info.status, SessionStatus::Idle);
    assert!(info.usage.input > 0);

    // The second model call saw both tool results, in call order.
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 2);
    let msgs = reqs[1]["messages"].as_array().unwrap();
    let tools: Vec<&serde_json::Value> = msgs.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(tools.len(), 2);
    assert!(
        tools[0]["content"]
            .as_str()
            .unwrap()
            .contains("hello-from-bash")
    );
    // Interleaved reasoning is replayed within the current turn.
    assert!(
        msgs.iter()
            .any(|m| m["role"] == "assistant" && m.get("reasoning_content").is_some())
    );
}

#[tokio::test]
async fn system_prompt_is_stable_between_steps() {
    let h = harness(
        vec![
            Reply::Tools(vec![("bash".into(), json!({"command": "true"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    h.mock.push(Reply::Text("second".into()));
    h.send(&s.id, "again").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 3);
    let sys: Vec<&serde_json::Value> = reqs.iter().map(|r| &r["messages"][0]).collect();
    assert!(
        sys.windows(2).all(|w| w[0] == w[1]),
        "system prompt must be byte-stable for prompt caching"
    );
    // Earlier messages are an exact prefix of later requests (append-only).
    let a = reqs[1]["messages"].as_array().unwrap();
    let b = reqs[2]["messages"].as_array().unwrap();
    assert_eq!(
        &b[..a.len()],
        &a[..],
        "history must be append-only between compactions"
    );
}

#[tokio::test]
async fn dangerous_command_needs_approval_and_denial_is_reported() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "sudo rm -rf /tmp/whatever"}),
            )]),
            Reply::Text("understood".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "clean up").await;
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::ApprovalRequested {
                request,
                session_id,
            }) = events.recv().await
            {
                assert_eq!(session_id, s.id);
                return request;
            }
        }
    })
    .await
    .expect("approval requested");
    assert_eq!(request.tool, "bash");
    assert!(request.reason.contains("elevated"));
    // Snapshot exposes the pending approval for reconnecting clients.
    let snap: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snap.approvals.len(), 1);
    assert_eq!(snap.info.status, SessionStatus::AwaitingApproval);
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: s.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Deny,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(results.len(), 1);
    assert!(results[0].1 && results[0].2.contains("declined"));
}

#[tokio::test]
async fn catastrophic_commands_are_blocked_even_in_auto_mode() {
    let h = harness(
        vec![
            Reply::Tools(vec![("bash".into(), json!({"command": "rm -rf ~"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
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
    h.send(&s.id, "x").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1 && results[0].2.contains("Refused"));
}

#[tokio::test]
async fn abort_stops_a_running_command() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "sleep 30", "timeout": 60}),
            )]),
            Reply::Text("after".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "wait").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let started = std::time::Instant::now();
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
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "abort must be prompt"
    );
    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    assert!(
        results[0].1 && results[0].2.contains("Interrupted"),
        "{results:?}"
    );
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
    assert!(
        entries
            .iter()
            .any(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text == "Stopped."))
    );
}

#[tokio::test]
async fn steering_message_is_injected_after_the_tool_batch() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "sleep 1; echo slow"}),
            )]),
            Reply::Text("noted".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "start").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("also do Y")],
                delivery: Delivery::Steer,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let kinds: Vec<&str> = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User { .. } => Some("user"),
            EntryBody::Assistant { .. } => Some("assistant"),
            EntryBody::ToolResult { .. } => Some("tool"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "tool", "user", "assistant"]);
    let last_req = h.mock.agent_requests().pop().unwrap();
    let msgs = last_req["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["content"], "also do Y");
}

#[tokio::test]
async fn follow_up_runs_after_the_turn() {
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
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("two")],
                delivery: Delivery::FollowUp,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let users: Vec<String> = h
        .entries(&s.id)
        .await
        .iter()
        .filter_map(|e| {
            if let EntryBody::User { content, .. } = &e.body {
                Some(plain_text(content))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(users, ["one", "two"]);
    assert_eq!(h.mock.agent_requests().len(), 2);
}

#[tokio::test]
async fn edit_tool_line_mode_and_stale_tag_protection() {
    let h = harness(vec![], 100_000).await;
    std::fs::write(h.work.join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let tag = tessera_core::tools::files::file_tag("alpha\nbeta\ngamma\n");
    let edit = |tag: &str, at: &str, text: &str| {
        Reply::Tools(vec![(
            "edit".into(),
            json!({"path": "f.txt", "tag": tag, "lines": [{"at": at, "text": text}]}),
        )])
    };
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"path": "f.txt"}),
    )]));
    h.mock.push(Reply::Tools(vec![("edit".into(), json!({"path": "f.txt", "tag": tag, "lines": [{"at": "2", "text": "BETA"}, {"at": ">3", "text": "delta"}]}))]));
    // The same (now stale) tag: line 1 didn't change, so it is rebased...
    h.mock.push(edit(&tag, "1", "ALPHA"));
    // ...but line 2 did, so this is refused with its current text.
    h.mock.push(edit(&tag, "2", "b2"));
    // A version this session never saw.
    h.mock.push(edit("ABCD", "1", "x"));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "edit").await;
    h.settle(&s.id).await;
    assert_eq!(
        std::fs::read_to_string(h.work.join("f.txt")).unwrap(),
        "ALPHA\nBETA\ngamma\ndelta\n"
    );
    let results = tool_results(&h.entries(&s.id).await);
    assert!(!results[1].1, "{results:?}");
    assert!(
        !results[2].1 && results[2].2.contains(&format!("Rebased from #{tag}")),
        "a stale edit of unchanged lines is rebased: {results:?}"
    );
    assert!(
        results[3].1
            && results[3].2.contains(&format!("changed since #{tag}"))
            && results[3].2.contains("2:BETA"),
        "a stale edit of changed lines is refused: {results:?}"
    );
    assert!(
        results[4].1 && results[4].2.contains("changed since that read"),
        "unknown tags are refused: {results:?}"
    );
    // The edit carries a diff for UIs.
    let entries = h.entries(&s.id).await;
    let has_diff = entries.iter().any(|e| matches!(&e.body, EntryBody::ToolResult { details: Some(d), .. } if d["diff"].as_str().is_some_and(|x| x.contains("+BETA"))));
    assert!(has_diff);
}

#[tokio::test]
async fn ledger_survives_compaction_and_checkpoint_is_injected() {
    // Tiny window forces compaction quickly.
    let big = "x".repeat(6_000);
    let mut script = vec![Reply::Tools(vec![(
        "task".into(),
        json!({"goal": "Build the widget", "constraints": ["never touch prod"], "plan": [{"text": "step one", "status": "in_progress"}, {"text": "step two"}], "done_criteria": ["tests pass"]}),
    )])];
    for i in 0..8 {
        script.push(Reply::Tools(vec![(
            "bash".into(),
            json!({"command": format!("printf '{big}' ; echo {i}")}),
        )]));
    }
    script.push(Reply::Text("finished".into()));
    let h = harness(script, 16_000).await;
    let s = h.session().await;
    h.send(&s.id, "please build the widget; remember marker-ABC")
        .await;
    h.settle(&s.id).await;

    let entries = h.entries(&s.id).await;
    let compactions = entries
        .iter()
        .filter(|e| matches!(e.body, EntryBody::Compaction { .. }))
        .count();
    let notices: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            if let EntryBody::Notice { text, .. } = &e.body {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(
        compactions >= 1 || notices.iter().any(|n| n.contains("trimmed")),
        "expected pruning or compaction; notices: {notices:?}"
    );
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);

    let last = h.mock.agent_requests().pop().unwrap();
    let body = last.to_string();
    if compactions >= 1 {
        assert!(h.mock.summary_requests() >= 1);
        let first_user = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap();
        let text = first_user["content"].as_str().unwrap();
        assert!(text.starts_with("[Context checkpoint]"), "{text}");
        assert!(
            text.contains("Goal: Build the widget"),
            "ledger re-injected verbatim"
        );
        assert!(text.contains("never touch prod"));
        assert!(
            text.contains("marker-ABC"),
            "user's own words preserved verbatim"
        );
    }
    // Estimated context stays under the window.
    let est = (body.len() as f64 / 3.6) as u64;
    assert!(
        est < 16_000,
        "request of ~{est} tokens exceeds the 16k window"
    );
    // The full history is still retrievable.
    assert!(tool_results(&entries).iter().all(|r| !r.2.is_empty()));
}

#[tokio::test]
async fn interrupted_tool_calls_are_reported_after_restart() {
    let dir;
    let session_id;
    {
        let h = harness(
            vec![Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "sleep 20"}),
            )])],
            100_000,
        )
        .await;
        let s = h.session().await;
        h.send(&s.id, "long thing").await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        session_id = s.id.clone();
        // Simulate a crash: drop the engine without letting the turn finish.
        dir = h.dir;
        drop(h.engine);
    }
    // Reopen on the same data directory with a fresh mock.
    let mock = Mock::start(vec![Reply::Text("I'll check the state first.".into())]).await;
    let cfg = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    let old_url = cfg
        .lines()
        .find(|l| l.starts_with("base_url"))
        .unwrap()
        .to_string();
    std::fs::write(
        dir.path().join("config.toml"),
        cfg.replace(&old_url, &format!("base_url = \"{}\"", mock.url)),
    )
    .unwrap();
    // Mark the session as mid-run in the store, as a crash would leave it.
    let store = tessera_core::store::Store::open(&dir.path().join("tessera.db")).unwrap();
    let (mut info, internal) = store.get_session(&session_id).unwrap().unwrap();
    info.status = SessionStatus::Running;
    store.put_session(&info, &internal).unwrap();
    drop(store);

    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
                Request::GetSession {
                    session_id: session_id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snap.info.status, SessionStatus::Interrupted);
    engine
        .handle(
            Request::Send {
                session_id: session_id.clone(),
                content: vec![ContentPart::text("continue")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(10), engine.wait_idle(&session_id))
        .await
        .unwrap();
    let req = mock.agent_requests().pop().unwrap();
    let tool_msg = req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()
        .clone();
    let text = tool_msg["content"].as_str().unwrap();
    assert!(text.contains("may or may not have completed"), "{text}");
}

#[tokio::test]
async fn memory_and_skills_round_trip_into_new_sessions() {
    let h = harness(
        vec![
            Reply::Tools(vec![
                ("memory".into(), json!({"target": "user", "action": "add", "content": "Prefers concise answers"})),
                ("skill_manage".into(), json!({"action": "create", "name": "deploy-homelab", "description": "Deploy a service to the homelab", "content": "1. build the image\n2. ship it to the homelab with rsync\n3. restart the service and check it answers"})),
            ]),
            Reply::Text("saved".into()),
            Reply::Text("hello again".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    // The user asks for the skill (otherwise a chat without real work can't make one).
    h.send(&s.id, "remember that, and save the deploy steps as a skill")
        .await;
    // Saving a skill asks first; the user allows it.
    let request = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("the skill asks");
    assert_eq!(request.tool, "skill_manage");
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: s.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let s2 = h.session().await;
    h.send(&s2.id, "hi").await;
    h.settle(&s2.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    let system = req["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Prefers concise answers"));
    assert!(system.contains("deploy-homelab: Deploy a service to the homelab"));
    let skills: Vec<SkillInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSkills { cwd: None }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(skills.iter().any(|s| s.name == "deploy-homelab"));
}

#[tokio::test]
async fn subagents_run_in_parallel_and_report_back() {
    let h = harness(
        vec![
            Reply::Tools(vec![("agent".into(), json!({"wait": true, "tasks": [{"name": "a", "prompt": "look at A", "readonly": true}, {"name": "b", "prompt": "look at B", "readonly": true}]}))]),
            Reply::Text("report from a subagent".into()),
            Reply::Text("report from a subagent".into()),
            Reply::Text("combined".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "investigate").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(results[0].0, "agent");
    assert!(!results[0].1, "{results:?}");
    assert_eq!(results[0].2.matches("report from a subagent").count(), 2);
    // Subagents are hidden from the main session list.
    let list: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(list.len(), 1);
    // Read-only subagents were not offered mutating tools.
    let reqs = h.mock.agent_requests();
    let sub_req = &reqs[1];
    let names: Vec<&str> = sub_req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"bash") && !names.contains(&"edit") && !names.contains(&"agent"),
        "{names:?}"
    );
}

#[tokio::test]
async fn jobs_validate_schedules_and_run_on_demand() {
    let h = harness(vec![Reply::Text("daily report: all good".into())], 100_000).await;
    let bad = h
        .engine
        .handle(
            Request::SaveJob {
                job: Job {
                    id: String::new(),
                    name: "x".into(),
                    schedule: "every blue moon".into(),
                    prompt: "p".into(),
                    cwd: h.work.display().to_string(),
                    model: None,
                    enabled: true,
                    permission: PermissionMode::Auto,
                    bot_id: None,
                    last_run: None,
                    next_run: None,
                    last_session: None,
                    last_status: None,
                    ..Default::default()
                },
            },
            None,
        )
        .await;
    assert!(bad.is_err());
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Daily report".into(),
                        schedule: "0 9 * * *".into(),
                        prompt: "write the report".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: None,
                        last_run: None,
                        next_run: None,
                        last_session: None,
                        last_status: None,
                        ..Default::default()
                    },
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(job.next_run.is_some());
    let info: SessionInfo = serde_json::from_value(
        h.engine
            .handle(Request::RunJob { id: job.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.kind, SessionKind::Job);
    h.settle(&info.id).await;
    let entries = h.entries(&info.id).await;
    assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p, AssistantPart::Text { text } if text.contains("all good"))))));
    let jobs: Vec<Job> =
        serde_json::from_value(h.engine.handle(Request::ListJobs, None).await.unwrap()).unwrap();
    assert_eq!(jobs[0].last_status.as_deref(), Some("ok"));
    // The job's system prompt says nobody is watching.
    let req = h.mock.agent_requests().pop().unwrap();
    assert!(
        req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Scheduled run")
    );
}

#[tokio::test]
async fn secrets_are_redacted_from_tool_output() {
    let h = harness(
        vec![
            Reply::Tools(vec![("bash".into(), json!({"command": "cat creds.env"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(
        h.work.join("creds.env"),
        "TOKEN=sk-supersecretvalue-1234567\n",
    )
    .unwrap();
    h.engine
        .auth()
        .set(
            "someprovider",
            tessera_core::auth::Credential::ApiKey {
                key: "sk-supersecretvalue-1234567".into(),
            },
        )
        .unwrap();
    let s = h.session().await;
    h.send(&s.id, "x").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].2.contains("[REDACTED]") && !results[0].2.contains("supersecret"),
        "{results:?}"
    );
    assert!(
        !h.mock
            .agent_requests()
            .iter()
            .any(|r| r.to_string().contains("supersecret"))
    );
}

#[tokio::test]
async fn credentials_stay_out_of_the_models_reach() {
    use base64::Engine as _;
    let secret = "zz-9f8e7d6c5b4a3210-vault";
    let provider_key = "sk-or-v1-0123456789abcdef0123456789abcdef";
    let h = harness(vec![], 100_000).await;
    let dir = h.dir.path().canonicalize().unwrap();
    // Obfuscated so the command line doesn't name the protected directory.
    let hidden = base64::engine::general_purpose::STANDARD.encode(dir.display().to_string());
    let me = std::process::id();
    h.mock.push(Reply::Tools(vec![
        ("bash".into(), json!({"command": "echo \"len=${#MY_API_TOKEN}\"; echo \"value=$MY_API_TOKEN\""})),
        ("bash".into(), json!({"command": format!("d=$(echo {hidden} | base64 -d); ls -a \"$d\"; base64 -w0 \"$d/auth.json\"; cat \"$d/auth.json\"")})),
        ("bash".into(), json!({"command": format!("d=$(echo {hidden} | base64 -d); base64 -w0 /proc/{me}/root$d/auth.json; tr '\\0' ' ' < /proc/{me}/environ | head -c 200")})),
        ("read".into(), json!({"path": dir.join("auth.json").display().to_string()})),
        ("grep".into(), json!({"pattern": "sk-or", "path": dir.display().to_string()})),
        ("bash".into(), json!({"command": "tessera pair"})),
    ]));
    h.mock.push(Reply::Text("done".into()));
    h.engine
        .handle(
            Request::SetSecret {
                name: "MY_API_TOKEN".into(),
                value: secret.into(),
            },
            None,
        )
        .await
        .unwrap();
    h.engine
        .auth()
        .set(
            "openrouter",
            tessera_core::auth::Credential::ApiKey {
                key: provider_key.into(),
            },
        )
        .unwrap();
    // Names only, never values.
    let listed = h
        .engine
        .handle(Request::ListSecrets, None)
        .await
        .unwrap()
        .to_string();
    assert!(listed.contains("MY_API_TOKEN") && !listed.contains(secret));

    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "x").await;
    // Using a secret needs the user's OK first (by default), naming it.
    let request = next_approval(&mut events)
        .await
        .expect("approval requested for the secret");
    assert!(request.reason.contains("$MY_API_TOKEN"), "{request:?}");
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: s.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    // Unknown/indirect credential reads now ask separately; approving the
    // secret injection must not silently approve them. Allow this fixture
    // explicitly to retain the independent sandbox/redaction assertions.
    for _ in 0..2 {
        let more = next_approval(&mut events)
            .await
            .expect("indirect read asks");
        assert!(more.reason.contains("credential"), "{more:?}");
        h.engine
            .handle(
                Request::RespondApproval {
                    session_id: s.id.clone(),
                    request_id: more.id,
                    decision: ApprovalDecision::Allow,
                },
                None,
            )
            .await
            .unwrap();
    }
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(results.len(), 6, "{results:?}");

    // The vault value is usable by commands but never shown.
    assert!(
        results[0].2.contains("len=25") && results[0].2.contains("value=[REDACTED]"),
        "{:?}",
        results[0]
    );
    let sandboxed = tessera_core::sandbox::support().available;
    let encoded = |v: &str| base64::engine::general_purpose::STANDARD.encode(v);
    if sandboxed {
        // Even encoded (which pattern redaction can't catch), nothing leaks:
        // the directory is empty inside the sandbox…
        assert!(
            results[1].2.contains("No such file") && !results[1].2.contains("tessera.db"),
            "{:?}",
            results[1]
        );
        // …and other processes' view of the filesystem and environment is off
        // limits (in a process namespace of its own the server isn't even there).
        assert!(
            results[2].2.contains("ermission denied") || results[2].2.contains("No such file"),
            "{:?}",
            results[2]
        );
    }
    // In-process tools refuse outright.
    assert!(
        results[3].1 && results[3].2.contains("Refused"),
        "{:?}",
        results[3]
    );
    assert!(
        results[4].1 && results[4].2.contains("Refused"),
        "{:?}",
        results[4]
    );
    // And the agent can't enrol devices.
    assert!(
        results[5].1 && results[5].2.contains("Refused"),
        "{:?}",
        results[5]
    );

    // Nothing that reached the model contains any form of the secrets.
    let sent = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    for v in [secret, provider_key] {
        assert!(!sent.contains(v), "leaked {v}");
        if sandboxed {
            assert!(!sent.contains(&encoded(v)[..20]), "leaked {v} (base64)");
        }
    }
    // The model is told which secrets exist, by name.
    assert!(sent.contains("$MY_API_TOKEN"));
}

#[tokio::test]
async fn voice_is_transcribed_on_the_server_with_its_own_key() {
    let h = harness(vec![], 100_000).await;
    let off = h
        .engine
        .handle(
            Request::Transcribe {
                audio: "AAAA".into(),
                mime: "audio/webm".into(),
                engine: None,
                operation: None,
                partial: false,
            },
            None,
        )
        .await
        .unwrap_err();
    // Out of the box: the server's own engine, not downloaded yet.
    assert!(off.message.contains("isn't set up"), "{}", off.message);

    let config = VoiceConfig {
        enabled: true,
        provider: "custom".into(),
        base_url: h.mock.url.clone(),
        model: "whisper-large-v3-turbo".into(),
        language: "en".into(),
        has_key: false,
        // What an app from before the engine choice sends.
        engine: String::new(),
        keep_audio: false,
        server_model: String::new(),
        server_voice: String::new(),
        server: None,
        openai_key: false,
        kept: 0,
        kept_bytes: 0,
    };
    let saved = h
        .engine
        .handle(
            Request::SaveVoice {
                config,
                api_key: Some("voice-key-123".into()),
            },
            None,
        )
        .await
        .unwrap();
    let saved: VoiceConfig = serde_json::from_value(saved).unwrap();
    assert!(saved.has_key);
    assert_eq!(saved.engine, "service");
    let shown = h
        .engine
        .handle(Request::GetVoice, None)
        .await
        .unwrap()
        .to_string();
    assert!(
        !shown.contains("voice-key-123"),
        "the key is never sent to clients"
    );

    let audio = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        b"RIFF....WAVEfake audio",
    );
    let t: Transcript = serde_json::from_value(
        h.engine
            .handle(
                Request::Transcribe {
                    audio,
                    mime: "audio/wav".into(),
                    engine: None,
                    operation: None,
                    partial: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(t.text, "hello from the mock");
    let seen = h.mock.state.transcriptions.lock().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.as_deref(), Some("Bearer voice-key-123"));
    assert!(
        seen[0].1.contains("whisper-large-v3-turbo")
            && seen[0].1.contains("audio.wav")
            && seen[0].1.contains("fake audio")
    );

    let bad = h
        .engine
        .handle(
            Request::Transcribe {
                audio: "%%%".into(),
                mime: "audio/wav".into(),
                engine: None,
                operation: None,
                partial: false,
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(bad.message.contains("base64"));
}

#[tokio::test]
async fn long_reasoning_is_dropped_before_anything_is_summarised() {
    // ~40k tokens of reasoning in one step of a tool loop (the mock's fixed
    // usage report calibrates estimates down to 0.4x).
    // (Numbered: the same sentence over and over is a loop, cut while streaming.)
    let reasoning: String = (0..3000)
        .map(|i| format!("Step {i}: think about the layout of this module. "))
        .collect();
    let h = harness(
        vec![
            Reply::ThinkTools(
                reasoning.clone(),
                vec![("bash".into(), json!({"command": "echo step-one"}))],
            ),
            Reply::Tools(vec![("bash".into(), json!({"command": "echo step-two"}))]),
            Reply::Text("all done".into()),
        ],
        24_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "do the two steps").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text.contains("reasoning is no longer replayed"))), "notice missing");
    assert!(
        !entries
            .iter()
            .any(|e| matches!(e.body, EntryBody::Compaction { .. })),
        "no summary was needed"
    );
    assert_eq!(h.mock.summary_requests(), 0);
    // The facts stay: both steps ran and the next request still had the first result.
    let reqs = h.mock.agent_requests();
    let last = reqs.last().unwrap().to_string();
    assert!(last.contains("step-one") && last.contains("step-two"));
    assert!(
        !last.contains("layout of this module"),
        "old reasoning is not replayed"
    );
    // The raw reasoning is kept in the session history.
    assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p, AssistantPart::Thinking { text, .. } if text.len() > 50_000)))));
}

#[tokio::test]
async fn bloated_checkpoints_are_condensed() {
    // This exercises compaction, not the host's optional command sandbox.
    let h = harness_agent(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "python3 -c 'print(\"a\" * 20000)'"}),
            )]),
            Reply::Text("ok".into()),
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "python3 -c 'print(\"b\" * 20000)'"}),
            )]),
            Reply::Text("again".into()),
        ],
        100_000,
        "[security]\nsandbox = 'off'\ndefault_permission = 'auto'\n",
    )
    .await;
    h.mock
        .state
        .big_summaries
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let s = h.session().await;
    h.send(&s.id, "first task").await;
    h.settle(&s.id).await;
    h.send(&s.id, "second task").await;
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
    h.settle(&s.id).await;
    let summary = h
        .entries(&s.id)
        .await
        .into_iter()
        .find_map(|e| match e.body {
            EntryBody::Compaction { summary, .. } => Some(summary),
            _ => None,
        })
        .expect("a checkpoint");
    assert!(
        summary.contains("condensed-marker"),
        "the oversized checkpoint was condensed"
    );
    assert!(summary.contains("marker-XYZ"), "critical details survive");
    assert_eq!(
        h.mock.summary_requests(),
        2,
        "one summary, one condensing pass"
    );
}

#[tokio::test]
async fn unknown_tools_and_bad_json_get_helpful_errors() {
    let h = harness(
        vec![
            Reply::Tools(vec![
                ("frobnicate".into(), json!({})),
                ("read".into(), json!({})),
            ]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "x").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1 && results[0].2.contains("Unknown tool") && results[0].2.contains("read"));
    assert!(results[1].1 && results[1].2.contains("path"));
}

#[tokio::test]
async fn background_review_curates_memory() {
    let h = harness(
        vec![
            Reply::Text("Sure, noted.".into()),
            // The reviewer's turn:
            Reply::Tools(vec![("memory".into(), json!({"target": "user", "action": "add", "content": "Homelab runs Proxmox at 192.168.1.20"}))]),
            Reply::Text("Saved one fact.".into()),
        ],
        100_000,
    )
    .await;
    // Review after every turn for this test.
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml"))
        .unwrap()
        .replace("review_interval = 0", "review_interval = 1");
    std::fs::write(h.dir.path().join("config.toml"), cfg).unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let s: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("my homelab is proxmox on 192.168.1.20")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    // Wait for the review to write memory.
    let mut ok = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let m: MemoryState =
            serde_json::from_value(engine.handle(Request::GetMemory, None).await.unwrap()).unwrap();
        if m.user.contains("Proxmox") {
            ok = true;
            break;
        }
    }
    assert!(ok, "review should have saved the fact");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        snap.entries.iter().any(
            |e| matches!(&e.body, EntryBody::Notice { text, .. } if text.starts_with("Learned"))
        )
    );
    // Clients get what was learned as structured items.
    let learned = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::Notice { learned, .. } if !learned.is_empty() => Some(learned.clone()),
            _ => None,
        })
        .expect("structured learned items");
    assert_eq!(learned.len(), 1);
    assert_eq!(
        (learned[0].kind, learned[0].action),
        (LearnedKind::Profile, LearnedAction::Added)
    );
    assert!(learned[0].text.contains("Proxmox"));
    // The reviewer only had memory/skill tools.
    let review_req = h
        .mock
        .agent_requests()
        .into_iter()
        .find(|r| r.to_string().contains("curate long-term memory"))
        .unwrap();
    let names: Vec<String> = review_req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| ["memory", "skill_manage", "skill"].contains(&n.as_str())),
        "{names:?}"
    );
    // Review sessions stay out of the session list.
    let list: Vec<SessionInfo> = serde_json::from_value(
        engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn mcp_stdio_server_tools_are_callable() {
    let h = harness(vec![], 100_000).await;
    let script = h.dir.path().join("mcp_echo.py");
    std::fs::write(&script, r#"
import sys, json
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m = msg["method"]
    if m == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "echo", "version": "1"}}
    elif m == "tools/list":
        res = {"tools": [{"name": "shout", "description": "Upper-case the text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}, "annotations": {"readOnlyHint": True}}]}
    elif m == "tools/call":
        res = {"content": [{"type": "text", "text": msg["params"]["arguments"]["text"].upper()}]}
    else:
        res = {}
    print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
"#).unwrap();
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    std::fs::write(
        h.dir.path().join("config.toml"),
        format!(
            "{cfg}\n[[mcp]]\nname = \"py\"\ncommand = \"python3\"\nargs = [\"{}\"]\n",
            script.display()
        ),
    )
    .unwrap();
    let mock = Mock::start(vec![
        Reply::Tools(vec![(
            "mcp__py__shout".into(),
            json!({"text": "hello mcp"}),
        )]),
        Reply::Text("ok".into()),
    ])
    .await;
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml"))
        .unwrap()
        .replace(&h.mock.url, &mock.url);
    std::fs::write(h.dir.path().join("config.toml"), cfg).unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await; // let the MCP server connect
    let s: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("shout")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(20), engine.wait_idle(&s.id))
        .await
        .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let results = tool_results(&snap.entries);
    assert_eq!(results[0].2, "HELLO MCP", "{results:?}");
    let first = &mock.agent_requests()[0];
    assert!(
        first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "mcp__py__shout")
    );
}

#[tokio::test]
async fn oversized_parallel_batch_is_shortened_to_fit() {
    // One assistant turn reads several huge files at once: the batch cannot be
    // split, so compaction alone cannot fit it.
    let h = harness(vec![], 16_000).await;
    for i in 0..4 {
        std::fs::write(
            h.work.join(format!("big{i}.txt")),
            format!("{}\n", "line of text in a big file\n".repeat(900)),
        )
        .unwrap();
    }
    h.mock.push(Reply::Tools(
        (0..4)
            .map(|i| ("read".to_string(), json!({"path": format!("big{i}.txt")})))
            .collect(),
    ));
    h.mock.push(Reply::Text("read them".into()));
    let s = h.session().await;
    h.send(&s.id, "read all the big files").await;
    h.settle(&s.id).await;
    let last = h.mock.agent_requests().pop().unwrap();
    let est = (last.to_string().len() as f64 / 3.6) as u64;
    assert!(
        est < 16_000,
        "final request ~{est} tokens must fit the 16k window"
    );
    let entries = h.entries(&s.id).await;
    // On a window this small each read stops at its share of it and says
    // where to go on, instead of losing the middle of the file.
    assert!(
        tool_results(&entries)
            .iter()
            .all(|r| r.2.contains("Continue with offset=")),
        "{:?}",
        tool_results(&entries)
            .iter()
            .map(|r| r.2.len())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn shell_commands_run_detached_from_the_terminal() {
    // Its own session: no controlling terminal, so sudo & co. cannot prompt over the TUI.
    // (In a process namespace of its own, the session's leader is outside it: 0.)
    let h = harness(
        vec![
            Reply::Tools(vec![("bash".into(), json!({"command": "sid=$(ps -o sid= -p $$ | tr -d ' '); { [ \"$sid\" = \"$$\" ] || [ \"$sid\" = 0 ]; } && echo session-leader; cat /dev/tty 2>&1 | head -1"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "check").await;
    let request = next_approval(&mut events)
        .await
        .expect("the composite read asks");
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: s.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let out = &tool_results(&h.entries(&s.id).await)[0].2;
    assert!(out.contains("session-leader"), "{out}");
    assert!(
        out.contains("No such device")
            || out.contains("No such device or address")
            || out.contains("not a tty"),
        "{out}"
    );
}

async fn next_approval(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
) -> Option<ApprovalRequest> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(Event::ApprovalRequested { request, .. }) => return Some(request),
                Ok(Event::SessionUpdated { session })
                    if session.status == SessionStatus::Idle && session.last_seq > 1 =>
                {
                    return None;
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

async fn set_permission(h: &Harness, id: &str, permission: PermissionMode) {
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: id.into(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(permission),
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
}

#[tokio::test]
async fn scheduled_jobs_never_get_more_permission_than_their_creator() {
    let unattended = json!({"action": "create", "name": "x", "schedule": "@every 30m", "prompt": "Check the project status.", "unattended": true});
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::Default,
        PermissionMode::Ask,
    ] {
        let h = harness(
            vec![
                Reply::Tools(vec![("schedule".into(), unattended.clone())]),
                Reply::Text("ok".into()),
            ],
            100_000,
        )
        .await;
        let s = h.session().await;
        set_permission(&h, &s.id, mode).await;
        let mut events = h.engine.subscribe();
        h.send(&s.id, "schedule it").await;
        // Read-only chats cannot create work. In other modes it is the
        // user's decision, without granting more authority to the job.
        if mode != PermissionMode::ReadOnly {
            let request = next_approval(&mut events)
                .await
                .expect("approval requested");
            assert_eq!(request.tool, "schedule");
            h.engine
                .handle(
                    Request::RespondApproval {
                        session_id: s.id.clone(),
                        request_id: request.id,
                        decision: ApprovalDecision::Allow,
                    },
                    None,
                )
                .await
                .unwrap();
        }
        h.settle(&s.id).await;
        // …and even then the job can't run with more than the session had.
        let jobs: Vec<Job> =
            serde_json::from_value(h.engine.handle(Request::ListJobs, None).await.unwrap())
                .unwrap();
        let results = tool_results(&h.entries(&s.id).await);
        if mode == PermissionMode::ReadOnly {
            assert!(jobs.is_empty());
            assert!(
                results[0].1 && results[0].2.contains("read-only"),
                "{results:?}"
            );
            assert!(next_approval(&mut events).await.is_none());
        } else {
            assert_eq!(jobs.len(), 1, "{results:?}");
            assert_eq!(jobs[0].permission, mode);
        }
    }
}

#[tokio::test]
async fn background_jobs_are_private_to_their_session() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "echo started; sleep 3", "background": true}),
            )]),
            Reply::Text("started".into()),
        ],
        100_000,
    )
    .await;
    let a = h.session().await;
    h.send(&a.id, "start it").await;
    h.settle(&a.id).await;
    let started = tool_results(&h.entries(&a.id).await);
    let id = started[0].2.split('`').nth(1).expect("job id").to_string();
    h.mock.push(Reply::Tools(vec![
        ("job".into(), json!({"action": "list"})),
        ("job".into(), json!({"action": "output", "id": id})),
    ]));
    h.mock.push(Reply::Text("done".into()));
    let b = h.session().await;
    h.send(&b.id, "what is running?").await;
    h.settle(&b.id).await;
    let seen = tool_results(&h.entries(&b.id).await);
    assert!(seen[0].2.contains("No background jobs"), "{seen:?}");
    assert!(seen[1].1 && seen[1].2.contains("No job"), "{seen:?}");
}

async fn wait_for(
    h: &Harness,
    id: &str,
    what: &str,
    pred: impl Fn(&[Entry]) -> bool,
) -> Vec<Entry> {
    for _ in 0..200 {
        let entries = h.entries(id).await;
        if pred(&entries) {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}: {:#?}", h.entries(id).await);
}

fn reports(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a.starts_with("subagent:") => Some(plain_text(content)),
            _ => None,
        })
        .collect()
}

fn last_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => content.iter().find_map(|p| match p {
                AssistantPart::Text { text } if !text.is_empty() => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn background_subagents_report_back_and_the_parent_resumes() {
    let h = harness(
        vec![
            Reply::Tools(vec![("agent".into(), json!({"tasks": [{"name": "alpha", "prompt": "research topic ALPHA"}, {"name": "beta", "prompt": "research topic BETA"}]}))]),
            Reply::Text("I started two researchers; I'll report back.".into()),
            Reply::Text("Here is the combined answer.".into()),
        ],
        100_000,
    )
    .await;
    h.mock.route(
        "topic ALPHA",
        Reply::Slow(Duration::from_millis(300), "ALPHA findings".into()),
    );
    h.mock.route(
        "topic BETA",
        Reply::Slow(Duration::from_millis(600), "BETA findings".into()),
    );
    let s = h.session().await;
    h.send(&s.id, "research both").await;
    // The parent answers at once instead of blocking on the subagents.
    let entries = wait_for(&h, &s.id, "the parent's first reply", |e| {
        last_text(e).contains("I started two")
    })
    .await;
    let results = tool_results(&entries);
    assert!(results[0].2.contains("in the background"), "{results:?}");
    assert!(reports(&entries).is_empty(), "no report yet");
    // The user can talk to it meanwhile (not stuck behind a queue).
    let status = h
        .engine
        .handle(
            Request::GetSession {
                session_id: s.id.clone(),
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(status["info"]["status"], "idle");
    // Reports arrive, then the parent picks up the combined answer by itself.
    let entries = wait_for(&h, &s.id, "reports and the combined answer", |e| {
        reports(e).len() == 2 && last_text(e).contains("combined")
    })
    .await;
    let r = reports(&entries);
    assert!(
        r[0].contains("ALPHA findings") && r[0].contains("Report from subagent \"alpha\""),
        "{r:?}"
    );
    assert!(r[1].contains("BETA findings"));
    // Subagent sessions count their tool calls; the parent's did one.
    let info: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.info.steps, 1);
}

#[tokio::test]
async fn subagents_are_asked_to_report_when_their_budget_runs_out() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [{"name": "digger", "prompt": "dig into GAMMA"}]}),
            )]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml"))
        .unwrap()
        .replace(
            "review_interval = 0",
            "review_interval = 0\nsubagent_steps = 2",
        );
    std::fs::write(h.dir.path().join("config.toml"), cfg).unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    for _ in 0..5 {
        h.mock.route(
            "dig into GAMMA",
            Reply::Tools(vec![("bash".into(), json!({"command": "echo digging"}))]),
        );
    }
    let s: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("go")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(20), engine.wait_idle(&s.id))
        .await
        .unwrap();
    // After two tool calls the subagent was told to report and offered no tools.
    let reqs = h.mock.agent_requests();
    let first_user = |r: &serde_json::Value| {
        r["messages"]
            .as_array()
            .and_then(|m| m.iter().find(|m| m["role"] == "user"))
            .map(|m| m["content"].to_string())
            .unwrap_or_default()
    };
    let last_sub = reqs
        .iter()
        .rfind(|r| first_user(r).contains("dig into GAMMA"))
        .unwrap();
    assert!(last_sub.to_string().contains("[Budget reached]"));
    assert!(
        last_sub["tools"].as_array().is_none_or(|t| t.is_empty()),
        "{:?}",
        last_sub["tools"]
    );
}

#[tokio::test]
async fn stopping_the_chat_stops_its_background_subagents() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"tasks": [{"name": "slow", "prompt": "a very slow DELTA job"}]}),
            )]),
            Reply::Text("started".into()),
        ],
        100_000,
    )
    .await;
    h.mock.route(
        "DELTA",
        Reply::Slow(Duration::from_secs(30), "never".into()),
    );
    let s = h.session().await;
    h.send(&s.id, "go").await;
    wait_for(&h, &s.id, "the parent's reply", |e| {
        last_text(e).contains("started")
    })
    .await;
    h.engine
        .handle(
            Request::Abort {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let entries = wait_for(&h, &s.id, "the stopped report", |e| !reports(e).is_empty()).await;
    assert!(
        reports(&entries)[0].contains("did not finish") || reports(&entries)[0].contains("stopped"),
        "{:?}",
        reports(&entries)
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    // No automatic follow-up turn after the user stopped things.
    assert_eq!(last_text(&h.entries(&s.id).await), "started");
}

#[tokio::test]
async fn private_chats_never_touch_disk_and_vanish_when_deleted() {
    let h = harness(vec![Reply::Text("private answer".into())], 100_000).await;
    // Something in memory that must not leak into the private chat.
    h.engine
        .handle(
            Request::SetMemory {
                target: MemoryTarget::User,
                content: "The user's name is SECRETNAME.".into(),
            },
            None,
        )
        .await
        .ok();
    let s: SessionInfo = serde_json::from_value(
        h.engine
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
            .unwrap(),
    )
    .unwrap();
    assert!(s.private);
    h.send(&s.id, "my private question about ZEBRAS").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(last_text(&entries).contains("private answer"));
    // Only the web tools; no memory, profile or project context.
    let req = h.mock.agent_requests().pop().unwrap();
    let tools: Vec<&str> = req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    let mut tools = tools;
    tools.sort();
    assert_eq!(tools, vec!["web_fetch", "web_search"]);
    assert!(!req.to_string().contains("SECRETNAME"));
    // Titled locally, never by asking a model.
    let info: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(info.info.title.contains("ZEBRAS"));
    // Nothing reached the database file, the index or the search.
    let raw = std::fs::read(h.dir.path().join("tessera.db")).unwrap();
    let wal = std::fs::read(h.dir.path().join("tessera.db-wal")).unwrap_or_default();
    for bytes in [&raw, &wal] {
        assert!(
            !bytes.windows(6).any(|w| w == b"ZEBRAS"),
            "private text on disk"
        );
        assert!(
            !bytes.windows(s.id.len()).any(|w| w == s.id.as_bytes()),
            "private session id on disk"
        );
    }
    let hits = h
        .engine
        .handle(
            Request::Search {
                query: "ZEBRAS".into(),
                limit: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(hits, json!([]));
    // Deleting erases it.
    h.engine
        .handle(
            Request::DeleteSession {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    assert!(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None
                },
                None
            )
            .await
            .is_err()
    );
    let list: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(list.iter().all(|i| i.id != s.id));
    // Forking is refused.
    let s2: SessionInfo = serde_json::from_value(
        h.engine
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
            .unwrap(),
    )
    .unwrap();
    assert!(
        h.engine
            .handle(
                Request::ForkSession {
                    session_id: s2.id.clone(),
                    at_seq: None
                },
                None
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn goals_are_told_to_the_agent_and_queued_messages_can_be_withdrawn() {
    let h = harness(
        vec![
            Reply::Slow(Duration::from_millis(800), "working".into()),
            Reply::Text("after".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let info: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::SetGoal {
                    session_id: s.id.clone(),
                    goal: "Ship the EPSILON release".into(),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.task.unwrap().goal, "Ship the EPSILON release");
    h.send(&s.id, "start").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Queue two messages while it works, then withdraw the first.
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("first extra")],
                delivery: Delivery::FollowUp,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("second extra")],
                delivery: Delivery::Steer,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snap.queued.len(), 2);
    assert!(snap.queued[0].steer && snap.queued[0].text == "second extra");
    h.engine
        .handle(
            Request::Dequeue {
                session_id: s.id.clone(),
                index: 1,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let all = h
        .mock
        .agent_requests()
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(
        all.contains("Ship the EPSILON release"),
        "the agent was told the goal"
    );
    assert!(
        !all.contains("first extra"),
        "withdrawn messages never reach the agent"
    );
    assert!(all.contains("second extra"));
}

#[tokio::test]
async fn withdrawing_a_queued_message_skips_hidden_notes() {
    let h = harness(
        vec![
            Reply::Slow(Duration::from_millis(800), "working".into()),
            Reply::Text("after".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "start").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // A goal set mid-turn waits as a hidden note ahead of the user's message.
    h.engine
        .handle(
            Request::SetGoal {
                session_id: s.id.clone(),
                goal: "Ship ZETA".into(),
            },
            None,
        )
        .await
        .unwrap();
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("please withdraw me")],
                delivery: Delivery::Steer,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snap.queued.len(), 1, "only the user's own message shows");
    h.engine
        .handle(
            Request::Dequeue {
                session_id: s.id.clone(),
                index: 0,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let all = h
        .mock
        .agent_requests()
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(
        !all.contains("please withdraw me"),
        "the visible message was withdrawn"
    );
    assert!(
        all.contains("Ship ZETA"),
        "the goal note still reached the agent"
    );
}

#[tokio::test]
async fn a_swarm_of_background_subagents_all_report_without_flooding_the_parent() {
    let n = 24;
    let tasks: Vec<serde_json::Value> = (0..n)
        .map(|i| json!({"name": format!("w{i:02}"), "prompt": format!("swarm job <J{i:02}>")}))
        .collect();
    let h = harness(
        vec![
            Reply::Tools(vec![("agent".into(), json!({ "tasks": tasks }))]),
            Reply::Text("Started the swarm.".into()),
            // Once every report is in: read the one that was shortened.
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "report", "name": "w07"}),
            )]),
            Reply::Text("All combined.".into()),
        ],
        1_000_000,
    )
    .await;
    // (Numbered: the same line over and over is a loop, cut while streaming.)
    let lines: String = (0..3_000).map(|i| format!("detail line {i}\n")).collect();
    let huge = format!("HUGE-START {lines} HUGE-END");
    for i in 0..n {
        let text = if i == 7 {
            huge.clone()
        } else {
            format!("result of J{i:02}")
        };
        h.mock.route(
            &format!("<J{i:02}>"),
            Reply::Slow(Duration::from_millis(120), text),
        );
    }
    let s = h.session().await;
    h.send(&s.id, "run the swarm").await;
    let entries = wait_for(&h, &s.id, "every report and the wrap-up", |e| {
        reports(e).len() == n && last_text(e).contains("All combined")
    })
    .await;
    let r = reports(&entries);
    for i in 0..n {
        let hits = r
            .iter()
            .filter(|t| t.contains(&format!("subagent \"w{i:02}\"")))
            .count();
        assert_eq!(hits, 1, "w{i:02} reported {hits} times");
    }
    // The long report was cut to its share, with a way to read the rest...
    let long = r.iter().find(|t| t.contains("\"w07\"")).unwrap();
    assert!(long.contains("HUGE-START") && !long.contains("HUGE-END"));
    assert!(long.len() < 8_000, "{}", long.len());
    assert!(long.contains("`agents` action `report`"), "{long}");
    // ...which returns the whole thing.
    let results = tool_results(&entries);
    let full = &results.last().unwrap().2;
    assert!(
        full.contains("HUGE-END"),
        "{}",
        &full[..200.min(full.len())]
    );
    // The parent was resumed once for the whole batch, not once per report.
    let parent_requests = h
        .mock
        .agent_requests()
        .iter()
        .filter(|req| req.to_string().contains("run the swarm"))
        .count();
    assert_eq!(parent_requests, 4);
}

#[tokio::test]
async fn subagents_cannot_read_reports_of_other_conversations() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [{"name": "spy", "prompt": "secret job <S1>"}]}),
            )]),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    h.mock.route("<S1>", Reply::Text("CLASSIFIED".into()));
    let a = h.session().await;
    h.send(&a.id, "go").await;
    let entries = wait_for(&h, &a.id, "the first chat's answer", |e| {
        last_text(e) == "done"
    })
    .await;
    let child = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d), ..
            } => d["subagents"][0]["session_id"].as_str().map(str::to_string),
            _ => None,
        })
        .expect("subagent session id");
    // Another conversation asks for that report by its session id.
    h.mock.push(Reply::Tools(vec![(
        "agents".into(),
        json!({"action": "report", "name": child}),
    )]));
    h.mock.push(Reply::Text("tried".into()));
    let b = h.session().await;
    h.send(&b.id, "peek").await;
    let entries = wait_for(&h, &b.id, "the second chat's answer", |e| {
        last_text(e) == "tried"
    })
    .await;
    let results = tool_results(&entries);
    assert!(results[0].1, "must be an error: {results:?}");
    assert!(!results[0].2.contains("CLASSIFIED"));
}

fn attachment(name: &str, mime: &str, data: &[u8]) -> Attachment {
    use base64::Engine as _;
    Attachment {
        name: name.into(),
        mime: mime.into(),
        data: base64::engine::general_purpose::STANDARD.encode(data),
    }
}

async fn send_files(
    h: &Harness,
    id: &str,
    text: &str,
    files: Vec<Attachment>,
) -> Result<serde_json::Value, ApiError> {
    h.engine
        .handle(
            Request::Send {
                session_id: id.into(),
                content: vec![ContentPart::text(text)],
                delivery: Delivery::Auto,
                attachments: files,
            },
            None,
        )
        .await
}

#[tokio::test]
async fn attached_files_are_saved_for_the_agent_and_removed_with_the_chat() {
    let h = harness(vec![Reply::Text("got them".into())], 100_000).await;
    let s = h.session().await;
    send_files(
        &h,
        &s.id,
        "look at these",
        vec![
            attachment("notes.txt", "text/plain", b"line one\nline two"),
            attachment("../../etc/evil.pdf", "application/pdf", b"%PDF-1.7\0binary"),
            attachment("pixel.png", "image/png", b"\x89PNG\r\n\x1a\n"),
        ],
    )
    .await
    .unwrap();
    h.settle(&s.id).await;
    let uploads = h.engine.paths().uploads_dir().join(&s.id);
    // Saved under the chat's own folder; path tricks in names go nowhere.
    assert_eq!(
        std::fs::read(uploads.join("notes.txt")).unwrap(),
        b"line one\nline two"
    );
    assert!(uploads.join("evil.pdf").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&uploads).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    // The model sees the text inline, where the binary lives, and the image.
    let req = h.mock.agent_requests()[0].to_string();
    assert!(
        req.contains("[Attached file: notes.txt") && req.contains("line two"),
        "{req}"
    );
    assert!(req.contains("evil.pdf") && req.contains("Binary file"));
    // Images go to the model when it can see them, and are saved either way.
    assert!(req.contains("[Attached image: pixel.png"), "{req}");
    assert!(uploads.join("pixel.png").is_file());
    // The agent's tools can read them even though they sit in Tessera's home.
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"path": uploads.join("notes.txt").display().to_string()}),
    )]));
    h.mock.push(Reply::Text("read it".into()));
    h.send(&s.id, "read the notes").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("line two"),
        "{results:?}"
    );
    // Deleting the chat deletes its files.
    h.engine
        .handle(
            Request::DeleteSession {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    assert!(!uploads.exists());
}

#[tokio::test]
async fn private_chats_never_write_attachments_to_disk() {
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s: SessionInfo = serde_json::from_value(
        h.engine
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
            .unwrap(),
    )
    .unwrap();
    let err = send_files(
        &h,
        &s.id,
        "x",
        vec![attachment("a.pdf", "application/pdf", b"%PDF\0\x01\x02")],
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("private"), "{err:?}");
    send_files(
        &h,
        &s.id,
        "x",
        vec![attachment("a.txt", "text/plain", b"PRIVATE TEXT")],
    )
    .await
    .unwrap();
    h.settle(&s.id).await;
    assert!(!h.engine.paths().uploads_dir().join(&s.id).exists());
    assert!(
        h.mock.agent_requests()[0]
            .to_string()
            .contains("PRIVATE TEXT")
    );
}

#[tokio::test]
async fn oversized_or_too_many_attachments_are_refused() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let big = vec![b'a'; (25 << 20) + 1];
    assert!(
        send_files(
            &h,
            &s.id,
            "x",
            vec![attachment("big.txt", "text/plain", &big)]
        )
        .await
        .is_err()
    );
    let many = (0..11)
        .map(|i| attachment(&format!("{i}.txt"), "text/plain", b"x"))
        .collect();
    assert!(send_files(&h, &s.id, "x", many).await.is_err());
    let mut bad = attachment("x.txt", "text/plain", b"x");
    bad.data = "***not base64***".into();
    assert!(send_files(&h, &s.id, "x", vec![bad]).await.is_err());
    // Nothing was sent to the model or saved.
    assert!(h.mock.agent_requests().is_empty());
    assert!(!h.engine.paths().uploads_dir().join(&s.id).exists());
}

#[tokio::test]
async fn mosaic_layouts_sync_with_versions_and_never_include_private_chats() {
    let h = harness(vec![], 100_000).await;
    let normal = h.session().await;
    let private: SessionInfo = serde_json::from_value(
        h.engine
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
            .unwrap(),
    )
    .unwrap();
    let empty: MosaicLayout =
        serde_json::from_value(h.engine.handle(Request::GetMosaics, None).await.unwrap()).unwrap();
    assert_eq!(empty.version, 0);
    let mut events = h.engine.subscribe();
    let layout = MosaicLayout {
        version: 0,
        mosaics: vec![Mosaic {
            id: "m1".into(),
            name: "Work".into(),
            layout: "grid".into(),
            tiles: vec![
                MosaicTile {
                    id: "t1".into(),
                    session_id: Some(normal.id.clone()),
                    board_id: None,
                    note_id: None,
                },
                MosaicTile {
                    id: "t2".into(),
                    session_id: Some(private.id.clone()),
                    board_id: None,
                    note_id: None,
                },
                MosaicTile {
                    id: "t3".into(),
                    session_id: None,
                    board_id: None,
                    note_id: None,
                },
            ],
            geometry: None,
        }],
    };
    let saved: MosaicLayout = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveMosaics {
                    layout: layout.clone(),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.version, 1);
    // A private chat's tile is never stored or sent to other devices.
    let tiles = &saved.mosaics[0].tiles;
    assert!(
        !tiles
            .iter()
            .any(|t| t.session_id.as_deref() == Some(private.id.as_str())),
        "{tiles:?}"
    );
    assert!(
        tiles
            .iter()
            .any(|t| t.session_id.as_deref() == Some(normal.id.as_str()))
    );
    // Other devices hear about it.
    let changed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Event::MosaicsChanged { layout }) = events.recv().await {
                break layout;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(changed.version, 1);
    // A device that saves from a stale copy is told to refresh, not overwritten.
    let stale = h
        .engine
        .handle(Request::SaveMosaics { layout }, None)
        .await
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::Conflict, "{stale:?}");
    let mut next = saved.clone();
    next.mosaics.clear();
    let deleted: MosaicLayout = serde_json::from_value(
        h.engine
            .handle(Request::SaveMosaics { layout: next }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(deleted.version, 2);
    assert!(deleted.mosaics.is_empty());
    // It persists across restarts.
    let again = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let v: MosaicLayout =
        serde_json::from_value(again.handle(Request::GetMosaics, None).await.unwrap()).unwrap();
    assert_eq!(v.version, 2);
}

#[tokio::test]
async fn a_fresh_server_has_no_model_until_one_can_answer() {
    // Providers that aren't signed in (one with a key to come, and the
    // built-in ChatGPT one): chats would fail, so setup must not count the AI
    // as done, though the default falls back to one of them. (A configured
    // provider also keeps the first run from looking for local servers.)
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[[providers]]\nid = \"anthropic\"\nkind = \"anthropic\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [{ id = \"claude-x\" }]\n",
    )
    .unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let st: SetupStatus =
        serde_json::from_value(engine.handle(Request::SetupStatus, None).await.unwrap()).unwrap();
    assert!(!st.has_model && st.default_model.is_none(), "{st:?}");
}

#[tokio::test]
async fn setup_status_reflects_the_server_and_completes_once() {
    let h = harness(vec![], 100_000).await;
    let st: SetupStatus =
        serde_json::from_value(h.engine.handle(Request::SetupStatus, None).await.unwrap()).unwrap();
    assert!(!st.completed);
    assert!(st.has_model, "{st:?}");
    assert_eq!(
        st.default_model.as_ref().map(|m| m.model.as_str()),
        Some("m")
    );
    assert!(!st.remote && st.devices == 0);
    // Nothing secret is part of the status.
    let raw = h
        .engine
        .handle(Request::SetupStatus, None)
        .await
        .unwrap()
        .to_string();
    assert!(
        !raw.to_lowercase().contains("token") && !raw.to_lowercase().contains("secret"),
        "{raw}"
    );
    h.engine.handle(Request::CompleteSetup, None).await.unwrap();
    let again = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let st: SetupStatus =
        serde_json::from_value(again.handle(Request::SetupStatus, None).await.unwrap()).unwrap();
    assert!(st.completed);
}

#[tokio::test]
async fn remote_headers_merge_and_the_bind_survives_the_running_server() {
    let h = harness(vec![], 100_000).await;
    let set = |headers: Option<std::collections::BTreeMap<String, String>>,
               add: Option<std::collections::BTreeMap<String, String>>,
               remove: Vec<String>| Request::SetRemoteAccess {
        urls: vec!["https://t.example.com".into()],
        headers,
        add_headers: add,
        remove_headers: remove,
    };
    let map = |k: &str, v: &str| std::collections::BTreeMap::from([(k.to_string(), v.to_string())]);
    h.engine
        .handle(
            set(Some(map("CF-Access-Client-Id", "id-1")), None, vec![]),
            None,
        )
        .await
        .unwrap();
    // Adding one header keeps the others.
    let info: RemoteAccessInfo = serde_json::from_value(
        h.engine
            .handle(set(None, Some(map("X-Extra", "v")), vec![]), None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        info.header_names,
        vec!["CF-Access-Client-Id".to_string(), "X-Extra".to_string()]
    );
    let info: RemoteAccessInfo = serde_json::from_value(
        h.engine
            .handle(set(None, None, vec!["X-Extra".into()]), None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.header_names, vec!["CF-Access-Client-Id".to_string()]);
    // The bind is saved through the engine, so its own later saves keep it.
    let st: SetupStatus = serde_json::from_value(
        h.engine
            .handle(
                Request::SetServerBind {
                    bind: "0.0.0.0:7979".into(),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(st.bind, "0.0.0.0:7979");
    assert!(st.e2ee_required, "encryption is required by default");
    h.engine.handle(Request::CompleteSetup, None).await.unwrap();
    h.engine
        .handle(
            Request::SaveWebSearch {
                search: "duckduckgo".into(),
                searxng_url: None,
                headers: None,
            },
            None,
        )
        .await
        .ok();
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(cfg.contains("0.0.0.0:7979"), "{cfg}");
    assert!(
        h.engine
            .handle(
                Request::SetServerBind {
                    bind: "nonsense".into()
                },
                None
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn messages_sent_while_working_are_queued_and_can_be_edited_or_turned_into_steering() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "sleep 1; echo slow"}),
            )]),
            Reply::Text("noted".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "start").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A normal send while it works waits in the queue (not steering).
    h.send(&s.id, "also do Y").await;
    let queued = |events: &mut tokio::sync::broadcast::Receiver<Event>| {
        let mut last = None;
        while let Ok(e) = events.try_recv() {
            if let Event::QueueChanged { queued, .. } = e {
                last = Some(queued);
            }
        }
        last
    };
    let q = queued(&mut events).expect("queue changed");
    assert_eq!(q.len(), 1);
    assert!(!q[0].steer, "{q:?}");
    // Edit it, then deliver it now as steering.
    h.engine
        .handle(
            Request::EditQueued {
                session_id: s.id.clone(),
                index: 0,
                text: "also do Z instead".into(),
            },
            None,
        )
        .await
        .unwrap();
    let q = queued(&mut events).unwrap();
    assert_eq!(q[0].text, "also do Z instead");
    h.engine
        .handle(
            Request::SteerQueued {
                session_id: s.id.clone(),
                index: 0,
            },
            None,
        )
        .await
        .unwrap();
    let q = queued(&mut events).unwrap();
    assert!(q[0].steer);
    assert!(
        h.engine
            .handle(
                Request::EditQueued {
                    session_id: s.id.clone(),
                    index: 5,
                    text: "x".into()
                },
                None
            )
            .await
            .is_err()
    );
    h.settle(&s.id).await;
    // It reached the agent at its next step, edited.
    let last_req = h.mock.agent_requests().pop().unwrap();
    let msgs = last_req["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["content"], "also do Z instead");
}

async fn layout(h: &Harness) -> MosaicLayout {
    serde_json::from_value(h.engine.handle(Request::GetMosaics, None).await.unwrap()).unwrap()
}

#[tokio::test]
async fn an_agent_shows_a_whiteboard_beside_its_chat_and_updates_it() {
    let page = "<!doctype html><html><head><title>t</title></head><body><script type=module>import * as THREE from 'three';</script></body></html>";
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "whiteboard".into(),
                // The user asked for it: on screen (`open`), not only a card.
                json!({"action": "show", "title": "Network", "html": page, "open": true}),
            )]),
            Reply::Text("Shown.".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    // The user's layout has this chat in a mosaic with another tile.
    h.engine
        .handle(
            Request::SaveMosaics {
                layout: MosaicLayout {
                    version: 0,
                    mosaics: vec![Mosaic {
                        id: "m1".into(),
                        name: "Work".into(),
                        layout: "dwindle".into(),
                        tiles: vec![
                            MosaicTile {
                                id: "t1".into(),
                                session_id: Some(s.id.clone()),
                                board_id: None,
                                note_id: None,
                            },
                            MosaicTile {
                                id: "t2".into(),
                                session_id: None,
                                board_id: None,
                                note_id: None,
                            },
                        ],
                        geometry: None,
                    }],
                },
            },
            None,
        )
        .await
        .unwrap();
    let mut events = h.engine.subscribe();
    h.send(&s.id, "draw my network").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(!results[0].1, "{results:?}");
    let boards: Vec<Whiteboard> = serde_json::from_value(
        h.engine
            .handle(
                Request::ListBoards {
                    session_id: Some(s.id.clone()),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(boards.len(), 1);
    let b = &boards[0];
    assert_eq!((b.title.as_str(), b.version), ("Network", 1));
    // Its tile went right after the chat's, on every device.
    let l = layout(&h).await;
    let tiles = &l.mosaics[0].tiles;
    assert_eq!(tiles.len(), 3);
    assert_eq!(tiles[1].board_id.as_deref(), Some(b.id.as_str()));
    let mut saw = (false, false);
    while let Ok(e) = events.try_recv() {
        match e {
            Event::BoardUpdated { board } if board.id == b.id => saw.0 = true,
            Event::MosaicsChanged { .. } => saw.1 = true,
            _ => {}
        }
    }
    assert!(saw.0 && saw.1, "{saw:?}");
    // Updating it by id bumps the version and doesn't add another tile.
    h.mock.push(Reply::Tools(vec![(
        "whiteboard".into(),
        json!({"action": "show", "id": b.id, "markdown": "# Updated"}),
    )]));
    h.mock.push(Reply::Text("Updated.".into()));
    h.send(&s.id, "update it").await;
    h.settle(&s.id).await;
    let b2: Whiteboard = serde_json::from_value(
        h.engine
            .handle(Request::GetBoard { id: b.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(b2.version, 2);
    assert!(matches!(b2.content, BoardContent::Markdown { .. }));
    assert_eq!(layout(&h).await.mosaics[0].tiles.len(), 3);
    // The browser page on this machine needs the board's token.
    let link = h
        .engine
        .handle(Request::BoardLink { id: b.id.clone() }, None)
        .await;
    assert!(link.is_err(), "markdown boards aren't pages");
    // A client can't keep a tile of a board that doesn't exist.
    let mut l = layout(&h).await;
    l.mosaics[0].tiles.push(MosaicTile {
        id: "t9".into(),
        session_id: None,
        board_id: Some("wb_missing".into()),
        note_id: None,
    });
    let saved: MosaicLayout = serde_json::from_value(
        h.engine
            .handle(Request::SaveMosaics { layout: l }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        !saved.mosaics[0]
            .tiles
            .iter()
            .any(|t| t.board_id.as_deref() == Some("wb_missing"))
    );
    // Deleting the chat removes its boards and their tiles.
    h.engine
        .handle(
            Request::DeleteSession {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    assert!(
        h.engine
            .handle(Request::GetBoard { id: b.id.clone() }, None)
            .await
            .is_err()
    );
    assert!(
        !layout(&h).await.mosaics[0]
            .tiles
            .iter()
            .any(|t| t.board_id.is_some())
    );
}

#[tokio::test]
async fn html_boards_are_served_only_with_their_token() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "whiteboard".into(),
                json!({"action": "show", "html": "<p id=x>hello</p>"}),
            )]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "show").await;
    h.settle(&s.id).await;
    let boards: Vec<Whiteboard> = serde_json::from_value(
        h.engine
            .handle(Request::ListBoards { session_id: None }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let id = boards[0].id.clone();
    let link = h
        .engine
        .handle(Request::BoardLink { id: id.clone() }, None)
        .await
        .unwrap();
    let path = link["path"].as_str().unwrap().to_string();
    let token = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    // The versioned, expiring grant is opaque to clients, not a raw key.
    assert!(path.starts_with(&format!("/board/{id}/")), "{path}");
    assert_eq!(
        h.engine.board_page(&id, &token).as_deref(),
        Some("<p id=x>hello</p>")
    );
    assert!(h.engine.board_page(&id, &"0".repeat(32)).is_none());
    assert!(h.engine.board_page("wb_other", &token).is_none());
    let mut tampered = token.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    assert!(h.engine.board_page(&id, &tampered).is_none());
    h.mock.push(Reply::Tools(vec![(
        "whiteboard".into(),
        json!({"action":"show", "id":id, "html":"<p>Updated</p>"}),
    )]));
    h.mock.push(Reply::Text("updated".into()));
    h.send(&s.id, "Update the board.").await;
    h.settle(&s.id).await;
    assert!(
        h.engine.board_page(&id, &token).is_none(),
        "old grant cannot read a new revision"
    );
    let fresh = h
        .engine
        .handle(Request::BoardLink { id: id.clone() }, None)
        .await
        .unwrap();
    let fresh_token = fresh["path"]
        .as_str()
        .unwrap()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap();
    assert_eq!(
        h.engine.board_page(&id, fresh_token).as_deref(),
        Some("<p>Updated</p>")
    );
}

#[tokio::test]
async fn an_agent_waits_for_a_signature_on_a_whiteboard() {
    let h = harness(
        vec![
            Reply::Tools(vec![("whiteboard".into(), json!({"action": "ask", "title": "Application", "prompt": "Sign the application for ACME", "input": "signature"}))]),
            Reply::Text("Thanks, submitted.".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "apply for me").await;
    // The board shows the question.
    let board = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::BoardUpdated { board }) = events.recv().await
                && board.ask.is_some()
            {
                break board;
            }
        }
    })
    .await
    .expect("ask shown");
    let ask = board.ask.clone().unwrap();
    assert!(matches!(ask.input, AskInput::Signature));
    // The chat says it needs the user.
    let info: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.info.status, SessionStatus::AwaitingApproval);
    // Answers must fit the question.
    let answer = |a: AskAnswer| Request::AnswerBoard {
        id: board.id.clone(),
        ask_id: ask.id.clone(),
        answer: a,
    };
    assert!(
        h.engine
            .handle(answer(AskAnswer::Text { text: "ok".into() }), None)
            .await
            .is_err()
    );
    use base64::Engine as _;
    let not_png = base64::engine::general_purpose::STANDARD.encode(b"GIF89a....");
    assert!(
        h.engine
            .handle(answer(AskAnswer::Signature { png: not_png }), None)
            .await
            .is_err()
    );
    let png =
        base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfake-signature-bytes");
    h.engine
        .handle(answer(AskAnswer::Signature { png }), None)
        .await
        .unwrap();
    // Answering twice doesn't work.
    assert!(
        h.engine
            .handle(answer(AskAnswer::Cancel), None)
            .await
            .is_err()
    );
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let r = &results[0].2;
    assert!(
        r.contains("The user signed") && r.contains("signature-"),
        "{r}"
    );
    let path = r
        .split("saved as ")
        .nth(1)
        .unwrap()
        .split(" (PNG")
        .next()
        .unwrap();
    assert!(std::fs::read(path).unwrap().starts_with(b"\x89PNG"));
    // The question is cleared from the board.
    let b: Whiteboard = serde_json::from_value(
        h.engine
            .handle(
                Request::GetBoard {
                    id: board.id.clone(),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(b.ask.is_none());
}

#[tokio::test]
async fn board_content_is_checked_and_local_apps_are_reached_only_through_their_board() {
    use axum::routing::get;
    let app = axum::Router::new().route("/hello", get(|| async { "hi from the dev server" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let h = harness(
        vec![
            Reply::Tools(vec![
                ("whiteboard".into(), json!({"action": "show", "url": "javascript:alert(1)"})),
                ("whiteboard".into(), json!({"action": "show", "html": "<p>x</p>", "markdown": "y"})),
                ("whiteboard".into(), json!({"action": "show", "image_path": "/etc/hostname"})),
                ("whiteboard".into(), json!({"action": "show", "title": "Dev site", "local_port": port, "local_path": "/hello"})),
            ]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "show things").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1 && results[1].1 && results[2].1, "{results:?}");
    assert!(!results[3].1, "{results:?}");
    let boards: Vec<Whiteboard> = serde_json::from_value(
        h.engine
            .handle(Request::ListBoards { session_id: None }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let local = boards.iter().find(|b| b.title == "Dev site").unwrap();
    let fetch = |path: &str| Request::BoardFetch {
        id: local.id.clone(),
        method: "GET".into(),
        path: path.into(),
        headers: vec![(
            "referer".into(),
            "http://127.0.0.1:1/?__tessera=secret".into(),
        )],
        body: None,
    };
    let r: BoardResponse =
        serde_json::from_value(h.engine.handle(fetch("/hello"), None).await.unwrap()).unwrap();
    use base64::Engine as _;
    assert_eq!(r.status, 200);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(r.body)
            .unwrap(),
        b"hi from the dev server"
    );
    assert!(
        h.engine
            .handle(fetch("http://evil.example/"), None)
            .await
            .is_err()
    );
    // Private chats have no whiteboards.
    let p: SessionInfo = serde_json::from_value(
        h.engine
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
            .unwrap(),
    )
    .unwrap();
    h.mock.push(Reply::Text("ok".into()));
    h.send(&p.id, "hi").await;
    h.settle(&p.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    assert!(!req.to_string().contains("\"whiteboard\""));
}

#[tokio::test]
async fn agents_can_read_tesseras_docs_on_demand() {
    let h = harness(
        vec![
            Reply::Tools(vec![("docs".into(), json!({"topic": "whiteboards"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "how do boards work").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].2.contains("three/addons"));
    // The docs themselves aren't in the prompt; only a pointer is.
    let req = h.mock.agent_requests()[0].to_string();
    assert!(!req.contains("CSS2DRenderer") && req.contains("`docs`"));
}

#[tokio::test]
#[ignore]
async fn print_tool_schema_sizes() {
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let mut total = 0;
    for t in req["tools"].as_array().unwrap() {
        let n = t.to_string().len();
        total += n;
        eprintln!("{:>6} {}", n, t["function"]["name"]);
    }
    eprintln!(
        "{total:>6} tools total; system prompt {} bytes",
        req["messages"][0]["content"].as_str().unwrap().len()
    );
}
