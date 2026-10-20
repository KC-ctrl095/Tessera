//! Live check of a chat run by the real Claude Code (ignored by default; it
//! uses the signed-in Claude plan, so keep it to the cheapest model):
//!   TESSERA_LIVE_CLAUDE=/usr/local/bin/claude cargo test -p tessera-core \
//!     --test live_claude_code -- --ignored --nocapture
//! Two turns: one that asks to write a file (approved through Tessera), one
//! that checks the conversation continues.

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

#[tokio::test]
#[ignore]
async fn a_real_claude_code_turn_with_an_approval() {
    let Ok(claude) = std::env::var("TESSERA_LIVE_CLAUDE") else {
        return;
    };
    let model = std::env::var("TESSERA_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let home = tempfile::tempdir_in(&tmp).unwrap();
    let work = tempfile::tempdir_in(&tmp).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nreview_interval = 0\n[[provider]]\nid = \"claude-code\"\nname = \"Claude (your subscription)\"\nkind = \"claude_code\"\nbase_url = \"{claude}\"\n[[provider.models]]\nid = \"{model}\"\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let info: ClaudeCodeInfo = serde_json::from_value(
        e.handle(
            Request::ClaudeCodeStatus {
                refresh: true,
                provider: None,
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    println!(
        "status: installed={} version={:?} signed_in={} plan={:?} api_key={} hint={}",
        info.installed, info.version, info.signed_in, info.plan, info.api_key, info.hint
    );
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.path().display().to_string()),
                model: Some(ModelRef {
                    provider: "claude-code".into(),
                    model: model.clone(),
                }),
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
    e.handle(
        Request::UpdateSession {
            session_id: s.id.clone(),
            title: None,
            model: None,
            thinking: None,
            permission: Some(PermissionMode::Ask),
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
    let mut rx = e.subscribe();
    let send = |text: &str| Request::Send {
        session_id: s.id.clone(),
        content: vec![ContentPart::text(text)],
        delivery: Delivery::Auto,
        attachments: Vec::new(),
    };
    e.handle(send("Use the Write tool to create hello.txt in the current folder containing the single word hi. Then reply with just: done"), None)
        .await
        .unwrap();
    let mut approvals = 0;
    let mut deltas = 0;
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            match rx.recv().await {
                Ok(Event::ApprovalRequested { request, .. }) => {
                    approvals += 1;
                    println!(
                        "approval: {} `{}` ({}) call {}",
                        request.tool, request.summary, request.reason, request.call_id
                    );
                    e.handle(
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
                Ok(Event::StreamDelta { .. }) => deltas += 1,
                Ok(Event::SessionUpdated { session })
                    if session.id == s.id
                        && session.status != SessionStatus::Running
                        && session.status != SessionStatus::AwaitingApproval
                        && session.last_seq > 1 =>
                {
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the turn did not finish");
    e.wait_idle(&s.id).await;
    let written = std::fs::read_to_string(work.path().join("hello.txt")).unwrap_or_default();
    println!("approvals: {approvals}, live deltas: {deltas}, hello.txt: {written:?}");
    e.handle(
        send("What single word did you write into that file? Reply with that word only."),
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(180), e.wait_idle(&s.id))
        .await
        .expect("the second turn did not finish");
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    for en in &snap.entries {
        println!(
            "{:>3} {}",
            en.seq,
            serde_json::to_string(&en.body)
                .unwrap()
                .chars()
                .take(300)
                .collect::<String>()
        );
    }
    println!(
        "status {:?}, usage {:?}, context {:?}",
        snap.info.status, snap.info.usage, snap.info.context
    );
    let report: UsageReport = serde_json::from_value(
        e.handle(
            Request::GetUsage {
                from: None,
                to: None,
                utc_offsets: Vec::new(),
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    for m in &report.by_model {
        println!(
            "usage row: {}/{} {:?} {:?}",
            m.provider, m.model, m.billing, m.totals
        );
    }
    assert_eq!(written.trim(), "hi");
    assert!(approvals >= 1);
    assert_eq!(snap.info.status, SessionStatus::Idle);
    let last = snap.entries.iter().rev().find_map(|en| match &en.body {
        EntryBody::Assistant { content, .. } => content.iter().find_map(|p| match p {
            AssistantPart::Text { text } => Some(text.clone()),
            _ => None,
        }),
        _ => None,
    });
    assert!(last.unwrap_or_default().to_lowercase().contains("hi"));
}

/// Tessera's tools in a real Claude Code turn (ignored; uses the plan):
///   TESSERA_LIVE_CLAUDE=/usr/local/bin/claude cargo test -p tessera-core \
///     --test live_claude_code tessera_tools -- --ignored --nocapture
/// One turn in Ask mode: a note and a whiteboard made through the bridge
/// (nothing to approve), and Tessera's command tool with a stored
/// credential (approved once, in Tessera; the value never shows).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn tessera_tools_in_a_real_claude_code_turn() {
    let Ok(claude) = std::env::var("TESSERA_LIVE_CLAUDE") else {
        return;
    };
    let model = std::env::var("TESSERA_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let home = tempfile::tempdir_in(&tmp).unwrap();
    let work = tempfile::tempdir_in(&tmp).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nreview_interval = 0\n[[provider]]\nid = \"claude-code\"\nname = \"Claude (your subscription)\"\nkind = \"claude_code\"\nbase_url = \"{claude}\"\n[[provider.models]]\nid = \"{model}\"\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let call = |r: Request| {
        let e = e.clone();
        async move { e.handle(r, None).await.unwrap() }
    };
    let info: ClaudeCodeInfo = serde_json::from_value(
        call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: None,
        })
        .await,
    )
    .unwrap();
    println!(
        "status: signed_in={} plan={:?} hint={}",
        info.signed_in, info.plan, info.hint
    );
    call(Request::SetSecret {
        name: "LIVE_TOKEN".into(),
        value: "live-check-value-48213".into(),
    })
    .await;
    let s: SessionInfo = serde_json::from_value(
        call(Request::CreateSession {
            cwd: Some(work.path().display().to_string()),
            model: Some(ModelRef {
                provider: "claude-code".into(),
                model: model.clone(),
            }),
            title: None,
            bot_id: None,
            private: false,
            lean: false,
        })
        .await,
    )
    .unwrap();
    call(Request::UpdateSession {
        session_id: s.id.clone(),
        title: None,
        model: None,
        thinking: None,
        permission: Some(PermissionMode::Ask),
        pinned: None,
        archived: None,
        cwd: None,
        members: None,
        fast: None,
        subagents: None,
    })
    .await;
    // Everything Tessera asks is approved (and counted) by a watcher.
    let asked = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let mut rx = e.subscribe();
    let watcher = {
        let (e, asked, sid) = (e.clone(), asked.clone(), s.id.clone());
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let Event::ApprovalRequested { request, .. } = ev {
                    println!(
                        "approval: {} `{}` ({}) call {}",
                        request.tool, request.summary, request.reason, request.call_id
                    );
                    asked.lock().push(request.tool.clone());
                    e.handle(
                        Request::RespondApproval {
                            session_id: sid.clone(),
                            request_id: request.id,
                            decision: ApprovalDecision::Allow,
                        },
                        None,
                    )
                    .await
                    .unwrap();
                }
            }
        })
    };
    call(Request::Send {
        session_id: s.id.clone(),
        content: vec![ContentPart::text(
            "Use your Tessera tools: 1) create a Tessera note titled 'Live bridge check' with the content \
             'made by Claude Code'; 2) show a Tessera whiteboard titled 'Live board' with the markdown '# It works'; \
             3) with Tessera's command tool (mcp__tessera__bash) run exactly: printf %s \"$LIVE_TOKEN\" | wc -c \
             Then reply with just the number that command printed.",
        )],
        delivery: Delivery::Auto,
        attachments: Vec::new(),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::time::timeout(Duration::from_secs(300), e.wait_idle(&s.id))
        .await
        .expect("the turn did not finish");
    watcher.abort();
    let asked = asked.lock().clone();
    let notes: Vec<NoteInfo> = serde_json::from_value(call(Request::ListNotes).await).unwrap();
    println!(
        "notes: {:?}",
        notes.iter().map(|n| &n.title).collect::<Vec<_>>()
    );
    let snap: SessionSnapshot = serde_json::from_value(
        call(Request::GetSession {
            session_id: s.id.clone(),
            since_seq: None,
        })
        .await,
    )
    .unwrap();
    for en in &snap.entries {
        println!(
            "{:>3} {}",
            en.seq,
            serde_json::to_string(&en.body)
                .unwrap()
                .chars()
                .take(400)
                .collect::<String>()
        );
    }
    println!(
        "title: {:?} status {:?} usage {:?}",
        snap.info.title, snap.info.status, snap.info.usage
    );
    let report: UsageReport = serde_json::from_value(
        call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await,
    )
    .unwrap();
    for m in &report.by_model {
        println!(
            "usage row: {}/{} {:?} {:?}",
            m.provider, m.model, m.billing, m.totals
        );
    }
    let all = serde_json::to_string(&snap.entries).unwrap();
    let names: Vec<String> = snap
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            AssistantPart::ToolCall { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    println!("tool calls: {names:?}; asked: {asked:?}");
    assert!(notes.iter().any(|n| n.title == "Live bridge check"));
    for n in ["note", "whiteboard", "bash"] {
        assert!(names.iter().any(|x| x == n), "no {n} call");
    }
    assert!(!names.iter().any(|n| n.starts_with("mcp__tessera__")));
    assert_eq!(asked, vec!["bash".to_string()], "asked once, in Tessera");
    assert!(
        !all.contains("live-check-value-48213"),
        "the credential leaked"
    );
    assert!(all.contains("22"), "the command didn't see the credential");
    assert_eq!(snap.info.status, SessionStatus::Idle);
}

/// Claude Code feeling native in Tessera (ignored; uses the plan, cheapest model):
///   TESSERA_LIVE_CLAUDE=$(which claude) cargo test -p tessera-core \
///     --test live_claude_code native -- --ignored --nocapture
/// Its to-do list becomes the chat's plan, its question comes as Tessera's
/// choice card and the answer reaches it, and its subagent gets a session
/// with its type as the role.
#[tokio::test]
#[ignore]
async fn native_todo_list_question_and_subagent_role() {
    let Ok(claude) = std::env::var("TESSERA_LIVE_CLAUDE") else {
        return;
    };
    let model = std::env::var("TESSERA_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let home = tempfile::tempdir_in(&tmp).unwrap();
    let work = tempfile::tempdir_in(&tmp).unwrap();
    std::fs::write(work.path().join("notes.txt"), "picnic notes\n").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nreview_interval = 0\n[[provider]]\nid = \"claude-code\"\nname = \"Claude (your subscription)\"\nkind = \"claude_code\"\nbase_url = \"{claude}\"\n[[provider.models]]\nid = \"{model}\"\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.path().display().to_string()),
                model: Some(ModelRef {
                    provider: "claude-code".into(),
                    model: model.clone(),
                }),
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
    let mut rx = e.subscribe();
    let send = |text: &str| Request::Send {
        session_id: s.id.clone(),
        content: vec![ContentPart::text(text)],
        delivery: Delivery::Auto,
        attachments: Vec::new(),
    };
    // Approvals are allowed; questions get the second option.
    let run = |rx: &mut tokio::sync::broadcast::Receiver<Event>| {
        let (e, sid) = (e.clone(), s.id.clone());
        let mut rx = rx.resubscribe();
        async move {
            let mut asked = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(240), async {
                loop {
                    match rx.recv().await {
                        Ok(Event::ApprovalRequested { request, .. }) => {
                            println!("approval: {} ({})", request.tool, request.reason);
                            let _ = e
                                .handle(
                                    Request::RespondApproval {
                                        session_id: sid.clone(),
                                        request_id: request.id,
                                        decision: ApprovalDecision::Allow,
                                    },
                                    None,
                                )
                                .await;
                        }
                        Ok(Event::BoardUpdated { board }) => {
                            if let Some(ask) = &board.ask
                                && !asked.contains(&ask.id)
                            {
                                asked.push(ask.id.clone());
                                println!("asked: {} {:?}", ask.prompt, ask.input);
                                let answer = match &ask.input {
                                    AskInput::Choice { options, .. } => AskAnswer::Choice {
                                        index: 1.min(options.len() as u32 - 1),
                                    },
                                    _ => AskAnswer::Text { text: "Tea".into() },
                                };
                                let _ = e
                                    .handle(
                                        Request::AnswerBoard {
                                            id: board.id.clone(),
                                            ask_id: ask.id.clone(),
                                            answer,
                                        },
                                        None,
                                    )
                                    .await;
                            }
                        }
                        Ok(Event::SessionUpdated { session })
                            if session.id == sid
                                && session.status == SessionStatus::Idle
                                && session.last_seq > 1 =>
                        {
                            return;
                        }
                        _ => {}
                    }
                }
            })
            .await;
            asked
        }
    };
    e.handle(send("Do these in order. 1) Use your task tools to create exactly three short tasks for planning a picnic, then mark the first one in_progress. 2) Use the AskUserQuestion tool to ask me which drink to bring, with exactly two options: Lemonade and Tea. 3) Reply with just the drink I picked."), None)
        .await
        .unwrap();
    let asked = run(&mut rx).await;
    e.wait_idle(&s.id).await;
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    let plan = snap
        .info
        .task
        .as_ref()
        .map(|t| t.plan.clone())
        .unwrap_or_default();
    println!("plan: {plan:?}");
    let last = snap.entries.iter().rev().find_map(|en| match &en.body {
        EntryBody::Assistant { content, .. } => content.iter().find_map(|p| match p {
            AssistantPart::Text { text } => Some(text.clone()),
            _ => None,
        }),
        _ => None,
    });
    println!("questions asked: {}, last reply: {last:?}", asked.len());

    // A subagent of a given type: a session of the chat with that role.
    e.handle(send("Use the Agent tool with subagent_type Explore to list the files in the current folder, then reply with just: done"), None)
        .await
        .unwrap();
    run(&mut rx).await;
    e.wait_idle(&s.id).await;
    // Subagents aren't listed on their own: the Agent call's result names its session.
    let snap1: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    let mut kids = Vec::new();
    for en in &snap1.entries {
        if let EntryBody::ToolResult {
            tool,
            details: Some(d),
            ..
        } = &en.body
            && tool == "Agent"
            && let Some(id) = d["subagents"][0]["session_id"].as_str()
        {
            let c: SessionSnapshot = serde_json::from_value(
                e.handle(
                    Request::GetSession {
                        session_id: id.into(),
                        since_seq: None,
                    },
                    None,
                )
                .await
                .unwrap(),
            )
            .unwrap();
            kids.push((
                c.info.title.clone(),
                c.info.role.clone(),
                c.info.parent_id.clone(),
            ));
        }
    }
    println!("subagents: {kids:?}");
    let snap2: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    for en in snap2.entries.iter().rev().take(8).rev() {
        println!(
            "{:>3} {}",
            en.seq,
            serde_json::to_string(&en.body)
                .unwrap()
                .chars()
                .take(400)
                .collect::<String>()
        );
    }

    assert_eq!(plan.len(), 3, "the to-do list is the chat's plan");
    assert_eq!(plan[0].status, PlanStatus::InProgress);
    assert!(!asked.is_empty(), "the question came as a choice card");
    assert!(
        last.unwrap_or_default().to_lowercase().contains("tea"),
        "the answer reached Claude Code"
    );
    assert!(
        kids.iter().any(|k| k.1.as_deref() == Some("Explore")),
        "the subagent's role is its type"
    );
}

/// Batch 14 on the real Claude Code (ignored; uses the plan, keep it to one run):
///   TESSERA_LIVE_CLAUDE=$(which claude) TESSERA_LIVE_CLAUDE_MODEL=sonnet cargo test -p tessera-core \
///     --test live_claude_code batch14 -- --ignored --nocapture --test-threads 1
/// A model newer than Opus 4.7 / Sonnet 4.6 (task tools off by default there): its task
/// list fills the plan; its Write and Edit are copied first (no prompts in Default) so a
/// rewind removes what it made; the verification note before it stops; and the second
/// turn goes to the same process (time to first token through Tessera, both turns).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn batch14_task_list_rewind_check_and_a_warm_turn() {
    let Ok(claude) = std::env::var("TESSERA_LIVE_CLAUDE") else {
        return;
    };
    let model = std::env::var("TESSERA_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "sonnet".into());
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let home = tempfile::tempdir_in(&tmp).unwrap();
    let work = tempfile::tempdir_in(&tmp).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n[[provider]]\nid = \"claude-code\"\nname = \"Claude (your subscription)\"\nkind = \"claude_code\"\nbase_url = \"{claude}\"\n[[provider.models]]\nid = \"{model}\"\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.path().display().to_string()),
                model: Some(ModelRef {
                    provider: "claude-code".into(),
                    model: model.clone(),
                }),
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
    let send = |text: &str| Request::Send {
        session_id: s.id.clone(),
        content: vec![ContentPart::text(text)],
        delivery: Delivery::Auto,
        attachments: Vec::new(),
    };
    // Time from the message to Claude Code's first streamed token, through Tessera.
    let first_token = |text: &'static str| {
        let e = e.clone();
        let sid = s.id.clone();
        let req = send(text);
        async move {
            let mut rx = e.subscribe();
            let t0 = std::time::Instant::now();
            e.handle(req, None).await.unwrap();
            let ttft = tokio::time::timeout(Duration::from_secs(300), async {
                loop {
                    if let Ok(Event::StreamDelta { session_id, .. }) = rx.recv().await
                        && session_id == sid
                    {
                        return t0.elapsed();
                    }
                }
            })
            .await
            .expect("nothing streamed");
            tokio::time::sleep(Duration::from_millis(300)).await;
            tokio::time::timeout(Duration::from_secs(600), e.wait_idle(&sid))
                .await
                .expect("the turn did not finish");
            ttft
        }
    };
    let cold = first_token(
        "Keep a task list as you work on this: create calc.py with a function add(a, b) that returns a + b, then edit calc.py to give add a one-line docstring. Reply in one short sentence when finished.",
    )
    .await;
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    for en in &snap.entries {
        println!(
            "{:>3} {}",
            en.seq,
            serde_json::to_string(&en.body)
                .unwrap()
                .chars()
                .take(260)
                .collect::<String>()
        );
    }
    let plan = snap
        .info
        .task
        .as_ref()
        .map(|t| t.plan.clone())
        .unwrap_or_default();
    println!("plan: {plan:?}");
    let calc = std::fs::read_to_string(work.path().join("calc.py")).unwrap_or_default();
    println!("calc.py: {calc:?}");
    let warm = first_token("Reply with just the word: ok").await;
    println!("time to first token: cold {cold:?}, warm {warm:?}");
    // Rewind the files to before the first message: calc.py was made after it.
    let first = snap
        .entries
        .iter()
        .find(|en| matches!(en.body, EntryBody::User { .. }))
        .unwrap()
        .seq;
    let r: RewindResult = serde_json::from_value(
        e.handle(
            Request::Rewind {
                session_id: s.id.clone(),
                to_seq: first - 1,
                files: true,
                conversation: false,
                dry_run: false,
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    println!(
        "rewind: restored {:?} deleted {:?} skipped {:?} not undone {:?}",
        r.restored, r.deleted, r.skipped, r.not_undone
    );
    assert!(!plan.is_empty(), "no task list");
    assert!(calc.contains("def add"), "{calc}");
    assert_eq!(r.deleted, ["calc.py"]);
    assert!(!work.path().join("calc.py").exists());
    assert!(warm < cold, "warm {warm:?} vs cold {cold:?}");
}

/// Stopping one of Claude Code's subagents alone (ignored; uses the plan):
///   TESSERA_LIVE_CLAUDE=$(which claude) cargo test -p tessera-core \
///     --test live_claude_code batch14_stop -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn batch14_stop_one_subagent() {
    let Ok(claude) = std::env::var("TESSERA_LIVE_CLAUDE") else {
        return;
    };
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let home = tempfile::tempdir_in(&tmp).unwrap();
    let work = tempfile::tempdir_in(&tmp).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 0\n[[provider]]\nid = \"claude-code\"\nname = \"Claude (your subscription)\"\nkind = \"claude_code\"\nbase_url = \"{claude}\"\n[[provider.models]]\nid = \"haiku\"\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.path().display().to_string()),
                model: Some(ModelRef {
                    provider: "claude-code".into(),
                    model: "haiku".into(),
                }),
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
    let mut rx = e.subscribe();
    e.handle(
        Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text(
                "Use the Agent tool once, with subagent_type general-purpose and run_in_background true, and the prompt: Run the Bash command `sleep 90` and then reply done. Then reply with just: started",
            )],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
        None,
    )
    .await
    .unwrap();
    let child = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Ok(Event::SessionUpdated { session }) = rx.recv().await
                && session.parent_call.is_some()
                && session.status == SessionStatus::Running
            {
                return session;
            }
        }
    })
    .await
    .expect("no subagent started");
    println!("subagent {} ({})", child.id, child.title);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let t0 = std::time::Instant::now();
    e.handle(
        Request::Abort {
            session_id: child.id.clone(),
        },
        None,
    )
    .await
    .unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(Event::SessionUpdated { session }) = rx.recv().await
                && session.id == child.id
                && session.status != SessionStatus::Running
            {
                return session.status;
            }
        }
    })
    .await
    .expect("the subagent did not stop");
    println!("subagent {stopped:?} after {:?}", t0.elapsed());
    tokio::time::timeout(Duration::from_secs(120), e.wait_idle(&s.id))
        .await
        .expect("the turn did not end");
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    for en in &snap.entries {
        println!(
            "{:>3} {}",
            en.seq,
            serde_json::to_string(&en.body)
                .unwrap()
                .chars()
                .take(260)
                .collect::<String>()
        );
    }
    assert_eq!(stopped, SessionStatus::Interrupted);
    assert!(
        !snap.entries.iter().any(|en| matches!(&en.body,
            EntryBody::Notice { text, .. } if text == "Stopped.")),
        "the chat's turn itself was not stopped"
    );
}

/// Parent-only B25 acceptance: one tiny Haiku turn; never run in the H5 helper.
/// Run: cargo test -p tessera-core --test live_claude_code b25_shared_command_runner -- --ignored --nocapture
#[tokio::test]
#[ignore = "uses the user's Claude plan; parent acceptance only"]
async fn b25_shared_command_runner() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "default_model = 'claude-code/haiku'\n[security]\nsandbox = 'off'\n[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 0\n[[provider]]\nid = 'claude-code'\nkind = 'claude_code'\n",
    ).unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let chat: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(work.path().display().to_string()),
                    model: Some(ModelRef {
                        provider: "claude-code".into(),
                        model: "haiku".into(),
                    }),
                    title: Some("B25 acceptance".into()),
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
            Request::UpdateSession {
                session_id: chat.id.clone(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(PermissionMode::Auto),
                cwd: None,
                pinned: None,
                archived: None,
                fast: None,
                members: None,
                subagents: None,
            },
            None,
        )
        .await
        .unwrap();
    engine.handle(Request::Send {
        session_id: chat.id.clone(),
        content: vec![ContentPart::text("Use mcp__tessera__bash exactly once to run printf b25-parity-ok in this folder. Do not use Bash or PowerShell, do not save memory or read any account files, and do not run other tools. Reply only done.")],
        delivery: Delivery::Auto, attachments: Vec::new(),
    }, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(120), engine.wait_idle(&chat.id))
        .await
        .expect("Haiku turn timeout");
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
                Request::GetSession {
                    session_id: chat.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let calls: Vec<_> = snap
        .entries
        .iter()
        .flat_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::ToolCall { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(calls, ["bash"], "{calls:?}");
    assert!(snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::ToolResult { tool, content, is_error: false, details: Some(d), .. }
        if tool == "bash" && d.get("exit_code") == Some(&serde_json::json!(0))
            && content.iter().any(|p| matches!(p, ContentPart::Text { text } if text.contains("b25-parity-ok")))
    )), "{:#?}", snap.entries);
    engine.shutdown().await;
}

/// Parent-only B25 acceptance: one tiny Haiku turn and a device-like decline.
/// Run: TESSERA_LIVE_CLAUDE=$(which claude) cargo test -p tessera-core \
///   --test live_claude_code b25_decline_keeps_the_turn_going -- --ignored --exact --nocapture
#[tokio::test]
#[ignore = "uses the user's Claude plan; parent acceptance only"]
async fn b25_decline_keeps_the_turn_going() {
    // Capture just the sanitized approval audit, not other logs or CLI traffic.
    // This current-thread test keeps the scoped dispatcher across the awaits.
    struct AuditTrace(std::sync::Arc<parking_lot::Mutex<Vec<String>>>);
    impl tracing::Subscriber for AuditTrace {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.target() == "tessera_core::approval"
                && *metadata.level() == tracing::Level::INFO
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if !self.enabled(event.metadata()) {
                return;
            }
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0.lock().push(message.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    let audit = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let dispatch = tracing::Dispatch::new(AuditTrace(audit.clone()));
    let _trace = tracing::dispatcher::set_default(&dispatch);
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let home = tempfile::tempdir_in(&tmp).unwrap();
    let work = tempfile::tempdir_in(&tmp).unwrap();
    let claude = std::env::var("TESSERA_LIVE_CLAUDE").unwrap_or_else(|_| "claude".into());
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = 'claude-code/haiku'\n[security]\nsandbox = 'off'\n[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 0\n[[provider]]\nid = 'claude-code'\nkind = 'claude_code'\nbase_url = {}\n", serde_json::to_string(&claude).unwrap()),
    ).unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let chat: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(work.path().display().to_string()),
                    model: Some(ModelRef {
                        provider: "claude-code".into(),
                        model: "haiku".into(),
                    }),
                    title: Some("B25 decline acceptance".into()),
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
            Request::UpdateSession {
                session_id: chat.id.clone(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(PermissionMode::Ask),
                cwd: None,
                pinned: None,
                archived: None,
                fast: None,
                members: None,
                subagents: None,
            },
            None,
        )
        .await
        .unwrap();
    let mut rx = engine.subscribe();
    engine.handle(Request::Send {
        session_id: chat.id.clone(),
        content: vec![ContentPart::text("Use mcp__tessera__bash exactly once to run printf b25-decline-probe in this folder. Do not use Bash or PowerShell, read any account files, save memory, or run other tools. After the tool responds, quote its first sentence and append b25-decline-finished in your final text reply. If it is not approved, do not retry or seek another route; finish your reply anyway.")],
        delivery: Delivery::Auto, attachments: Vec::new(),
    }, None).await.unwrap();
    let mut declined = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match rx.recv().await.expect("live engine events closed") {
                Event::ApprovalRequested {
                    session_id,
                    request,
                } if session_id == chat.id => {
                    assert_eq!(request.tool, "bash", "unexpected approval: {request:?}");
                    assert_eq!(request.arguments["command"], "printf b25-decline-probe");
                    declined.push(request.call_id);
                    engine
                        .handle(
                            Request::RespondApproval {
                                session_id: chat.id.clone(),
                                request_id: request.id,
                                decision: ApprovalDecision::Deny,
                            },
                            Some("b25-acceptance-device"),
                        )
                        .await
                        .unwrap();
                }
                Event::SessionUpdated { session }
                    if session.id == chat.id
                        && session.last_seq > 1
                        && !matches!(
                            session.status,
                            SessionStatus::Running | SessionStatus::AwaitingApproval
                        ) =>
                {
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("Haiku did not finish after the approval response");
    tokio::time::timeout(Duration::from_secs(30), engine.wait_idle(&chat.id))
        .await
        .expect("declined turn did not settle");
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
                Request::GetSession {
                    session_id: chat.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    engine.shutdown().await;
    assert_eq!(declined.len(), 1, "exactly one action was declined");
    assert_eq!(snap.info.permission, PermissionMode::Ask);
    assert_eq!(
        snap.info.status,
        SessionStatus::Idle,
        "the denial must not stop the turn"
    );
    let calls: Vec<_> = snap
        .entries
        .iter()
        .flat_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::ToolCall { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(calls, ["bash"], "no retry or alternative action");
    let denial = snap
        .entries
        .iter()
        .find(|e| {
            matches!(&e.body,
                EntryBody::ToolResult { call_id, tool, is_error: true, content, .. }
                if call_id == &declined[0] && tool == "bash"
                    && plain_text(content).contains("The user declined this bash action.")
            )
        })
        .expect("Claude must receive Tessera's declined-action result");
    let all = serde_json::to_string(&snap.entries).unwrap();
    assert!(
        !all.contains("STOP what you are doing"),
        "not Claude Code's default stop instruction"
    );
    assert!(
        !snap.entries.iter().any(|e| {
            matches!(
                &e.body,
                EntryBody::Assistant {
                    stop: StopReason::Aborted,
                    ..
                }
            ) || matches!(&e.body, EntryBody::Notice { text, .. } if text == "Stopped.")
        }),
        "ordinary Deny is not Stop"
    );
    assert_eq!(
        snap.entries
            .iter()
            .filter(|e| matches!(&e.body, EntryBody::User { author: None, .. }))
            .count(),
        1,
        "the answer completes the same user turn"
    );
    let final_text = snap
        .entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::Assistant {
                content,
                stop: StopReason::Stop,
                error: None,
                ..
            } if e.seq > denial.seq => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .expect("Claude must finish with final text after the denied action");
    assert!(
        final_text.contains("The user declined this bash action.")
            && final_text.contains("b25-decline-finished"),
        "Claude must read the actual refusal and finish: {final_text:?}"
    );
    let audit = audit.lock();
    let matching: Vec<_> = audit
        .iter()
        .filter(|line| {
            line.contains(&format!("session={} ", chat.id))
                && line.contains(" tool=bash outcome=declined answered_by=b25-acceptance-device")
        })
        .collect();
    assert_eq!(matching.len(), 1, "one INFO decline audit: {audit:?}");
    assert!(
        matching[0].contains("approval time=") && !matching[0].contains("b25-decline-probe"),
        "audit has a timestamp, not the command"
    );
    println!(
        "decline audit: {}; final reply: {final_text:?}",
        matching[0]
    );
}

/// What a saved secret changes in a real Claude Code chat (ignored; uses the
/// plan, two short turns): with a secret saved, file reads and commands go
/// through Tessera's redacting tools and the secret never shows up in the
/// chat; without one, Claude's own tools are used. Prints both timings.
///   TESSERA_LIVE_CLAUDE=$(which claude) cargo test -p tessera-core \
///     --test live_claude_code saved_secret -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn a_saved_secret_moves_claude_to_tessera_tools() {
    let Ok(claude) = std::env::var("TESSERA_LIVE_CLAUDE") else {
        return;
    };
    let model = std::env::var("TESSERA_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let tmp = std::env::var("TESSERA_LIVE_TMP")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let canary = format!(
        "lc{}q",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000_000
    );
    for with_secret in [false, true] {
        let home = tempfile::tempdir_in(&tmp).unwrap();
        let work = tempfile::tempdir_in(&tmp).unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            format!("[agent]\nreview_interval = 0\n[security]\nsandbox = \"off\"\n[[provider]]\nid = \"claude-code\"\nname = \"Claude (your subscription)\"\nkind = \"claude_code\"\nbase_url = \"{claude}\"\n[[provider.models]]\nid = \"{model}\"\n"),
        )
        .unwrap();
        std::fs::write(
            work.path().join("notes.txt"),
            format!("Meeting notes\ntoken: {canary}\nnext: buy milk\n"),
        )
        .unwrap();
        let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
            .await
            .unwrap();
        if with_secret {
            e.handle(
                Request::SetSecret {
                    name: "LIVE_TOKEN".into(),
                    value: canary.clone(),
                },
                None,
            )
            .await
            .unwrap();
        }
        let s: SessionInfo = serde_json::from_value(
            e.handle(
                Request::CreateSession {
                    cwd: Some(work.path().display().to_string()),
                    model: Some(ModelRef {
                        provider: "claude-code".into(),
                        model: model.clone(),
                    }),
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
        let mut rx = e.subscribe();
        let started = std::time::Instant::now();
        e.handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text(
                    "Read notes.txt in the current folder, then run the shell command `ls` there. Reply with the line that starts with 'next:' and how many files ls listed.",
                )],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(240), async {
            loop {
                match rx.recv().await {
                    Ok(Event::ApprovalRequested { request, .. }) => {
                        e.handle(
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
                    Ok(Event::SessionUpdated { session })
                        if session.id == s.id
                            && session.status != SessionStatus::Running
                            && session.status != SessionStatus::AwaitingApproval
                            && session.last_seq > 1 =>
                    {
                        return;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("the turn did not finish");
        e.wait_idle(&s.id).await;
        let took = started.elapsed();
        let snap: SessionSnapshot = serde_json::from_value(
            e.handle(
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
        let all = serde_json::to_string(&snap.entries).unwrap();
        let mut tools = Vec::new();
        for en in &snap.entries {
            if let EntryBody::Assistant { content, .. } = &en.body {
                for p in content {
                    if let AssistantPart::ToolCall { name, .. } = p {
                        tools.push(name.clone());
                    }
                }
            }
        }
        let reply = snap
            .entries
            .iter()
            .rev()
            .find_map(|en| match &en.body {
                EntryBody::Assistant { content, .. } => content.iter().find_map(|p| match p {
                    AssistantPart::Text { text } => Some(text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_default();
        println!(
            "with_secret={with_secret}: {:.1} s, tools {tools:?}, status {:?}, reply {:?}, usage {:?}",
            took.as_secs_f64(),
            snap.info.status,
            reply.chars().take(200).collect::<String>(),
            snap.info.usage
        );
        assert_eq!(snap.info.status, SessionStatus::Idle);
        assert!(reply.to_lowercase().contains("milk"), "{reply}");
        if with_secret {
            assert!(!all.contains(&canary), "the saved secret reached the chat");
            assert!(
                !tools.iter().any(|t| t == "Read" || t == "Bash"),
                "native content tools were used: {tools:?}"
            );
        }
    }
}
