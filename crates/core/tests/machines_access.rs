//! Who may use a machine, and when it asks first, decided on the server in
//! the `machines` tool's own dispatch (the same for every engine's turns):
//! a machine only certain bots may use is out of reach of plain chats and
//! other bots, and within reach of that bot's chats, its subagents and its
//! scheduled duties. The machine here is a stand-in on its own connection
//! that records what reaches it.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use parking_lot::Mutex;
use serde_json::json;
use tessera_proto::*;

fn bot(name: &str) -> Bot {
    Bot {
        id: String::new(),
        name: name.into(),
        handle: String::new(),
        avatar: String::new(),
        color: String::new(),
        role: "homelab upkeep".into(),
        personality: format!("{name} is calm and brief."),
        instructions: String::new(),
        model: None,
        thinking: None,
        permission: PermissionMode::Default,
        tools: ToolPreset::Coding,
        allowed_tools: vec![],
        cwd: None,
        own_memory: false,
        share_user_profile: true,
        can_message: vec!["*".into()],
        askable: None,
        knowledge: Vec::new(),
        starters: Vec::new(),
        // These tests are about the machine's own say: the bots may use
        // whatever it allows.
        machines: Some(BotMachines {
            default: MachineAccess::Allowed,
            ..Default::default()
        }),
        secrets: None,
        created_at: 0,
        updated_at: 0,
    }
}

async fn save_bot(h: &Harness, b: Bot) -> Bot {
    let v = h
        .engine
        .handle(Request::SaveBot { bot: b }, None)
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn chat(h: &Harness, bot: Option<&Bot>) -> SessionInfo {
    let v = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: Some(h.work.display().to_string()),
                model: None,
                title: None,
                bot_id: bot.map(|b| b.id.clone()),
                private: false,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

/// A machine on its own connection: says hello with `scope`, answers every
/// action "ran it" and keeps what reached it.
async fn machine(
    h: &Harness,
    device: &str,
    name: &str,
    scope: MachineScope,
) -> Arc<Mutex<Vec<NodeCall>>> {
    let (conn, mut rx) = h.engine.node_conn(device);
    h.engine
        .node_request(
            Request::NodeHello {
                name: name.into(),
                os: "linux x86_64".into(),
                version: "0.1.0".into(),
                scope,
                generation: 1,
                sandbox: "on".into(),
                sandbox_ok: true,
                home: Some("/home/me".into()),
                computer: None,
                host: None,
                features: None,
                guard_key: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, keep) = (h.engine.clone(), seen.clone());
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                keep.lock().push(call.clone());
                let _ = engine
                    .node_request(
                        Request::NodeReply {
                            call_id: call.call_id,
                            result: NodeResult {
                                output: "ran it".into(),
                                ok: true,
                                refused: false,
                                exit_code: Some(0),
                                image: None,
                            },
                        },
                        &conn,
                    )
                    .await;
            }
        }
    });
    seen
}

/// Sends `text`, allows every approval, waits for the run to end; the
/// approvals asked.
async fn run(h: &Harness, id: &str, text: &str) -> Vec<ApprovalRequest> {
    let mut events = h.engine.subscribe();
    h.send(id, text).await;
    let mut asked = Vec::new();
    let mut started = false;
    let done = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Ok(Event::ApprovalRequested {
                    session_id,
                    request,
                }) => {
                    asked.push(request.clone());
                    h.engine
                        .handle(
                            Request::RespondApproval {
                                session_id,
                                request_id: request.id,
                                decision: ApprovalDecision::Allow,
                            },
                            None,
                        )
                        .await
                        .unwrap();
                }
                Ok(Event::SessionUpdated { session }) if session.id == id => {
                    if session.status != SessionStatus::Idle {
                        started = true;
                    } else if started {
                        return;
                    }
                }
                Ok(_) => {}
                Err(e) => panic!("events: {e}"),
            }
        }
    })
    .await;
    assert!(done.is_ok(), "the run did not finish");
    h.settle(id).await;
    asked
}

fn run_on(machine: &str, command: &str) -> serde_json::Value {
    json!({"action": "run", "machine": machine, "command": command, "cwd": "/home/me/code"})
}

async fn machines(h: &Harness) -> Vec<MachineInfo> {
    serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap()).unwrap()
}

#[tokio::test]
async fn only_the_bot_a_machine_allows_reaches_it_with_its_helpers_and_duties() {
    let h = harness(vec![], 100_000).await;
    let sweeper = save_bot(&h, bot("Sweeper")).await;
    let other = save_bot(&h, bot("Other")).await;
    let code = MachineScope {
        folders: vec![FolderAccess {
            path: "/home/me/code".into(),
            write: true,
        }],
        commands: true,
        ask: MachineAsk::Chat,
        ..Default::default()
    };
    let lab = machine(
        &h,
        "dev_lab",
        "Lab",
        MachineScope {
            bots: Some(vec![sweeper.id.clone()]),
            ..code.clone()
        },
    )
    .await;

    // A plain chat: the machine isn't there for it, and nothing reaches it.
    let plain = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Lab", "uptime"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &plain.id, "PLAIN check the lab").await;
    assert!(
        asked.is_empty(),
        "refused before anyone is asked: {asked:?}"
    );
    let results = tool_results(&h.entries(&plain.id).await);
    assert!(
        results
            .iter()
            .all(|r| r.1 && r.2.contains("Unknown tool `machines`")),
        "{results:?}"
    );
    let request = h.mock.agent_requests().last().unwrap().clone();
    assert!(
        !request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "machines")
    );
    // Another bot: the same.
    let o = chat(&h, Some(&other)).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Lab", "uptime"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &o.id, "OTHER check the lab").await;
    let r = tool_results(&h.entries(&o.id).await).remove(0);
    assert!(r.1 && r.2.contains("Unknown tool `machines`"), "{r:?}");
    assert!(lab.lock().is_empty(), "nothing reached the machine");
    let recent = &machines(&h).await[0].recent;
    assert!(
        recent.is_empty(),
        "hidden tools never reach or log activity on a machine: {recent:?}"
    );

    // The sweeper's own chat: it runs, unasked (the machine asks nothing on
    // its own and the command isn't risky), and the machine is told who.
    let s = chat(&h, Some(&sweeper)).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Lab", "uptime"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "SWEEPER check the lab").await;
    assert!(asked.is_empty(), "{asked:?}");
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(!r.1 && r.2.contains("ran it"), "{r:?}");
    assert_eq!(
        lab.lock().last().unwrap().bot.as_ref().unwrap().id,
        sweeper.id
    );

    // Its subagents act as it.
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait": true, "tasks": [{"name": "helper", "prompt": "SUBTASK check the lab"}]}),
    )]));
    h.mock.route(
        "SUBTASK",
        Reply::Tools(vec![("machines".into(), run_on("Lab", "df -h"))]),
    );
    h.mock.route("SUBTASK", Reply::Text("helper done".into()));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, "SWEEPER start a helper").await;
    let last = lab.lock().last().cloned().unwrap();
    assert!(
        matches!(&last.action, NodeAction::Run { command, .. } if command == "df -h"),
        "{last:?}"
    );
    assert_eq!(
        last.bot.as_ref().unwrap().id,
        sweeper.id,
        "the helper acts as the sweeper"
    );

    // Its scheduled duties run as it, with nobody there to ask.
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Nightly sweep".into(),
                        schedule: "0 3 * * *".into(),
                        prompt: "NIGHTLY check the lab".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: Some(sweeper.id.clone()),
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
    h.mock.route(
        "NIGHTLY",
        Reply::Tools(vec![("machines".into(), run_on("Lab", "free -m"))]),
    );
    h.mock.route("NIGHTLY", Reply::Text("swept".into()));
    let duty: SessionInfo = serde_json::from_value(
        h.engine
            .handle(Request::RunJob { id: job.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    h.settle(&duty.id).await;
    let r = tool_results(&h.entries(&duty.id).await).remove(0);
    assert!(!r.1 && r.2.contains("ran it"), "{r:?}");
    let last = lab.lock().last().cloned().unwrap();
    assert_eq!(last.bot.as_ref().unwrap().name, "Sweeper");
    let recent = &machines(&h).await[0].recent;
    assert_eq!(recent[0].bot.as_deref(), Some("Sweeper"), "{recent:?}");
    assert_eq!(recent[0].outcome, "done");

    // Another device can't loosen it: it has no request for that, and a
    // machine's own reports (`node_scope`) count only on its own connection.
    let (phone, _rx) = h.engine.node_conn("dev_phone");
    let wide = MachineScope {
        bots: None,
        ask: MachineAsk::Chat,
        ..code.clone()
    };
    assert!(
        h.engine
            .node_request(
                Request::NodeScope {
                    scope: wide.clone(),
                    name: None,
                },
                &phone
            )
            .await
            .is_err()
    );
    let m = machines(&h)
        .await
        .into_iter()
        .find(|m| m.id == "dev_lab")
        .unwrap();
    assert_eq!(m.scope.bots, Some(vec![sweeper.id.clone()]), "unchanged");
    // A device saying hello becomes a machine itself, with its own id: never this one.
    h.engine
        .node_request(
            Request::NodeHello {
                name: "Lab".into(),
                os: "linux".into(),
                version: "0.1.0".into(),
                scope: wide,
                generation: 1,
                sandbox: "on".into(),
                sandbox_ok: true,
                home: None,
                computer: None,
                host: None,
                features: None,
                guard_key: None,
            },
            &phone,
        )
        .await
        .unwrap();
    let all = machines(&h).await;
    let lab = all.iter().find(|m| m.id == "dev_lab").unwrap();
    assert_eq!(
        lab.scope.bots,
        Some(vec![sweeper.id.clone()]),
        "still only the sweeper"
    );
    assert!(
        all.iter()
            .any(|m| m.id == "dev_phone" && m.name == "Lab (2)"),
        "names stay unique, so approvals can't be confused: {all:?}"
    );
}

#[tokio::test]
async fn asking_first_follows_the_stricter_of_the_machine_and_the_chat() {
    let h = harness(vec![], 100_000).await;
    let scope = |ask| MachineScope {
        folders: vec![FolderAccess {
            path: "/home/me/code".into(),
            write: true,
        }],
        commands: true,
        ask,
        ..Default::default()
    };
    let risky = machine(&h, "dev_r", "Risky", scope(MachineAsk::Risky)).await;
    let every = machine(&h, "dev_e", "Every", scope(MachineAsk::Always)).await;
    let c = chat(&h, None).await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: c.id.clone(),
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
    // Only risky ones: an everyday command runs in a YOLO chat unasked…
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Risky", "cargo test"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    assert!(run(&h, &c.id, "one").await.is_empty());
    // …a risky one asks, even in YOLO, saying why.
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Risky", "git push origin main"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &c.id, "two").await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].reason.contains("pushes") && asked[0].reason.contains("asks before risky actions"),
        "{}",
        asked[0].reason
    );
    assert_eq!(risky.lock().len(), 2);
    assert!(risky.lock()[1].approved && !risky.lock()[0].approved);
    // Remote operands cannot be authorized by the server's filesystem.
    // Read pipelines now require the normal command question too.
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: c.id.clone(),
                permission: Some(PermissionMode::Ask),
                title: None,
                model: None,
                thinking: None,
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
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on(
            "Risky",
            "cat own-file | head; grep 'git push' own-log | wc -l; ls && sha256sum own-file",
        ),
    )]));
    h.mock.push(Reply::Text("done".into()));
    assert_eq!(run(&h, &c.id, "read chain").await.len(), 1);
    assert_eq!(risky.lock().len(), 3);
    assert!(risky.lock()[2].approved);
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Risky", "cat .netrc | head -20"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    assert_eq!(run(&h, &c.id, "credential read").await.len(), 1);
    assert!(risky.lock()[3].approved);
    // Every action: even a read asks.
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "read", "machine": "Every", "path": "/home/me/code/a.txt"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    assert_eq!(run(&h, &c.id, "three").await.len(), 1);
    assert!(every.lock()[0].approved);
}

fn read_on(machine: &str, path: &str) -> serde_json::Value {
    json!({"action": "read", "machine": machine, "path": path})
}

#[tokio::test]
async fn a_bots_own_access_only_narrows_what_the_machine_allows() {
    let h = harness(vec![], 100_000).await;
    let open = MachineScope {
        folders: vec![FolderAccess {
            path: "/home/me/code".into(),
            write: true,
        }],
        commands: true,
        ask: MachineAsk::Chat,
        ..Default::default()
    };
    let lab = machine(&h, "dev_lab", "Lab", open.clone()).await;

    // A new bot saved without the setting starts with no access at all.
    let keeper = save_bot(
        &h,
        Bot {
            machines: None,
            ..bot("Keeper")
        },
    )
    .await;
    assert_eq!(
        keeper.machines,
        Some(BotMachines::default()),
        "least privilege"
    );
    assert_eq!(keeper.machine_access("dev_lab"), MachineAccess::NoAccess);
    let k = chat(&h, Some(&keeper)).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Lab", "uptime"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &k.id, "KEEPER check the lab").await;
    assert!(
        asked.is_empty(),
        "refused before anyone is asked: {asked:?}"
    );
    let results = tool_results(&h.entries(&k.id).await);
    assert!(
        results
            .iter()
            .all(|r| r.1 && r.2.contains("Unknown tool `machines`")),
        "{results:?}"
    );
    let request = h.mock.agent_requests().last().unwrap().clone();
    assert!(
        !request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "machines")
    );
    assert!(lab.lock().is_empty(), "nothing reached the machine");

    // Read only: it reads and lists there; commands and changes are refused
    // here, before anyone is asked, and never reach the machine.
    let mut ro = keeper.clone();
    ro.machines = Some(BotMachines {
        default: MachineAccess::NoAccess,
        each: [("dev_lab".to_string(), MachineAccess::ReadOnly)].into(),
    });
    let ro = save_bot(&h, ro).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        read_on("Lab", "/home/me/code/notes.txt"),
    )]));
    h.mock
        .push(Reply::Tools(vec![("machines".into(), run_on("Lab", "ls"))]));
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "write", "machine": "Lab", "path": "/home/me/code/a.txt", "content": "x"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &k.id, "KEEPER look and change").await;
    assert!(asked.is_empty(), "{asked:?}");
    let results = tool_results(&h.entries(&k.id).await);
    let n = results.len();
    let (list, read, cmd, write) = (
        &results[n - 4],
        &results[n - 3],
        &results[n - 2],
        &results[n - 1],
    );
    assert!(
        !list.1 && list.2.contains("this bot may only read here"),
        "{list:?}"
    );
    assert!(!read.1 && read.2.contains("ran it"), "{read:?}");
    for r in [cmd, write] {
        assert!(
            r.1 && r
                .2
                .contains("this bot may only read files and list folders there"),
            "{r:?}"
        );
    }
    {
        let seen = lab.lock();
        assert_eq!(seen.len(), 1, "only the read reached it: {seen:?}");
        assert!(matches!(seen[0].action, NodeAction::Read { .. }));
        // The machine is told too, so it holds the bot to reading files.
        assert!(seen[0].files_only, "{:?}", seen[0]);
    }
    // A client that leaves the setting out of an edit doesn't loosen it.
    let renamed = save_bot(
        &h,
        Bot {
            machines: None,
            role: "keeps the lab tidy".into(),
            ..ro.clone()
        },
    )
    .await;
    assert_eq!(renamed.machines, ro.machines, "kept as it was");

    // As the machine allows: the command runs.
    let mut full = ro.clone();
    full.machines = Some(BotMachines {
        default: MachineAccess::NoAccess,
        each: [("dev_lab".to_string(), MachineAccess::Allowed)].into(),
    });
    save_bot(&h, full).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Lab", "make"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &k.id, "KEEPER build").await;
    let r = tool_results(&h.entries(&k.id).await).pop().unwrap();
    assert!(!r.1 && r.2.contains("ran it"), "{r:?}");
    assert_eq!(lab.lock().len(), 2);

    // Never above the machine: commands off there stay off for it.
    let quiet = machine(
        &h,
        "dev_quiet",
        "Quiet",
        MachineScope {
            commands: false,
            ..open.clone()
        },
    )
    .await;
    let mut everywhere = renamed.clone();
    everywhere.machines = Some(BotMachines {
        default: MachineAccess::Allowed,
        ..Default::default()
    });
    save_bot(&h, everywhere).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Quiet", "make"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &k.id, "KEEPER build quietly").await;
    let r = tool_results(&h.entries(&k.id).await).pop().unwrap();
    assert!(r.1 && r.2.contains("Commands are off on Quiet"), "{r:?}");
    assert!(quiet.lock().is_empty());
    // …and a machine only for other bots stays closed to it.
    let other = save_bot(&h, bot("Other")).await;
    let _only = machine(
        &h,
        "dev_only",
        "Only",
        MachineScope {
            bots: Some(vec![other.id.clone()]),
            ..open.clone()
        },
    )
    .await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Only", "make"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &k.id, "KEEPER build elsewhere").await;
    let r = tool_results(&h.entries(&k.id).await).pop().unwrap();
    assert!(r.1 && r.2.contains("isn't among them"), "{r:?}");

    // The user's own chats are unchanged: as the machine allows.
    let plain = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Lab", "make"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &plain.id, "PLAIN build").await;
    let r = tool_results(&h.entries(&plain.id).await).pop().unwrap();
    assert!(!r.1 && r.2.contains("ran it"), "{r:?}");
}

#[tokio::test]
async fn asking_a_bot_with_more_machine_access_asks_first() {
    let h = harness(vec![], 100_000).await;
    let _lab = machine(
        &h,
        "dev_lab",
        "Lab",
        MachineScope {
            commands: true,
            ask: MachineAsk::Chat,
            ..Default::default()
        },
    )
    .await;
    let mut helper = bot("Helper");
    helper.tools = ToolPreset::Assistant;
    helper.machines = Some(BotMachines::default());
    let helper = save_bot(&h, helper).await;
    let builder = save_bot(&h, bot("Builder")).await;
    assert_eq!(builder.machine_access("dev_lab"), MachineAccess::Allowed);
    let c = chat(&h, Some(&helper)).await;
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@builder", "message": "BUILDER please build", "wait": true}),
    )]));
    h.mock
        .route("BUILDER please build", Reply::Text("Built.".into()));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &c.id, "HELPER get it built").await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0]
            .reason
            .contains("@builder may do more on Lab than this chat"),
        "{}",
        asked[0].reason
    );
}

/// What counts is what each may really do on a machine, its own "who can
/// use it" included: a chat it keeps out (a plain chat, or a bot not on
/// its list) asking a bot it lets in asks first.
#[tokio::test]
async fn handing_work_to_a_bot_a_machine_lets_in_asks_from_chats_it_keeps_out() {
    let h = harness(vec![], 100_000).await;
    let builder = save_bot(&h, bot("Builder")).await;
    let _only = machine(
        &h,
        "dev_only",
        "Only",
        MachineScope {
            commands: true,
            ask: MachineAsk::Chat,
            bots: Some(vec![builder.id.clone()]),
            ..Default::default()
        },
    )
    .await;
    let helper = save_bot(&h, bot("Helper")).await;
    for (who, c) in [
        ("PLAIN", chat(&h, None).await),
        ("HELPER", chat(&h, Some(&helper)).await),
    ] {
        h.mock.push(Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot": "@builder", "message": "BUILDER please build", "wait": true}),
        )]));
        h.mock
            .route("BUILDER please build", Reply::Text("Built.".into()));
        h.mock.push(Reply::Text("done".into()));
        let asked = run(&h, &c.id, &format!("{who} get it built")).await;
        assert_eq!(asked.len(), 1, "{who}: {asked:?}");
        assert!(
            asked[0]
                .reason
                .contains("@builder may do more on Only than this chat"),
            "{who}: {}",
            asked[0].reason
        );
    }
}

/// In YOLO a bot asks for nothing, but a limit the user set on a machine
/// isn't an approval: handing work to a bot that may do more there is
/// still asked.
#[tokio::test]
async fn even_in_yolo_handing_work_to_a_bot_with_more_machine_access_asks() {
    let h = harness(vec![], 100_000).await;
    let _lab = machine(
        &h,
        "dev_lab",
        "Lab",
        MachineScope {
            commands: true,
            ask: MachineAsk::Chat,
            ..Default::default()
        },
    )
    .await;
    let mut helper = bot("Helper");
    helper.tools = ToolPreset::Assistant;
    helper.permission = PermissionMode::Auto;
    helper.machines = Some(BotMachines::default());
    let helper = save_bot(&h, helper).await;
    save_bot(&h, bot("Builder")).await;
    let c = chat(&h, Some(&helper)).await;
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@builder", "message": "BUILDER please build", "wait": true}),
    )]));
    h.mock
        .route("BUILDER please build", Reply::Text("Built.".into()));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &c.id, "HELPER get it built").await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].reason.contains("@builder may do more on Lab"),
        "{}",
        asked[0].reason
    );
}

/// A removed bot's duties stop (switched off, kept), and one that still
/// runs is refused rather than run with a plain chat's reach.
#[tokio::test]
async fn a_removed_bots_duties_stop() {
    let h = harness(vec![], 100_000).await;
    let b = save_bot(&h, bot("Sweeper")).await;
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "nightly sweep".into(),
                        schedule: "0 3 * * *".into(),
                        prompt: "sweep".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        permission: PermissionMode::Default,
                        enabled: true,
                        bot_id: Some(b.id.clone()),
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
    h.engine
        .handle(Request::DeleteBot { id: b.id.clone() }, None)
        .await
        .unwrap();
    let jobs: Vec<Job> =
        serde_json::from_value(h.engine.handle(Request::ListJobs, None).await.unwrap()).unwrap();
    let kept = jobs
        .iter()
        .find(|j| j.id == job.id)
        .expect("the duty is kept");
    assert!(!kept.enabled, "switched off");
    let run = h
        .engine
        .handle(Request::RunJob { id: job.id.clone() }, None)
        .await;
    assert!(run.is_err(), "{run:?}");
}

/// A bot that may only read files on a machine doesn't see its screen
/// either: looking is more than reading files (a read-only chat may look).
#[tokio::test]
async fn a_bot_that_may_only_read_files_there_gets_no_screenshot() {
    let h = harness(vec![], 100_000).await;
    let (conn, mut rx) = h.engine.node_conn("dev_pc");
    h.engine
        .node_request(
            Request::NodeHello {
                name: "PC".into(),
                os: "windows x86_64".into(),
                version: "0.1.0".into(),
                scope: MachineScope {
                    commands: true,
                    desktop: true,
                    ask: MachineAsk::Chat,
                    ..Default::default()
                },
                generation: 1,
                sandbox: "off".into(),
                sandbox_ok: true,
                home: None,
                computer: None,
                host: None,
                features: Some(vec!["desktop".into()]),
                guard_key: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::<NodeCall>::new()));
    let keep = seen.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                keep.lock().push(call);
            }
        }
    });
    let mut reader = bot("Reader");
    reader.permission = PermissionMode::Auto;
    reader.machines = Some(BotMachines {
        default: MachineAccess::NoAccess,
        each: [("dev_pc".to_string(), MachineAccess::ReadOnly)].into(),
    });
    let reader = save_bot(&h, reader).await;
    let s = chat(&h, Some(&reader)).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot", "machine": "PC"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "look at the screen").await;
    assert!(asked.is_empty(), "{asked:?}");
    assert!(seen.lock().is_empty(), "{:?}", seen.lock());
    let results = tool_results(&h.entries(&s.id).await);
    let r = results.last().unwrap();
    assert!(r.1 && r.2.contains("Unknown tool `desktop`"), "{r:?}");
    drop(conn);
}

/// An answer given after the chat's permissions changed while it waited
/// allows nothing: it was asked under the old ones (asked again under the
/// new ones).
#[tokio::test]
async fn an_answer_given_after_the_chats_permissions_changed_allows_nothing() {
    let h = harness(vec![], 100_000).await;
    let every = machine(
        &h,
        "dev_e",
        "Every",
        MachineScope {
            folders: vec![FolderAccess {
                path: "/home/me/code".into(),
                write: true,
            }],
            commands: true,
            ask: MachineAsk::Always,
            ..Default::default()
        },
    )
    .await;
    let c = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Every", "make"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&c.id, "build it").await;
    let request = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("asked first");
    // The chat goes read-only before the answer comes.
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: c.id.clone(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(PermissionMode::ReadOnly),
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
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: c.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&c.id).await;
    assert!(every.lock().is_empty(), "{:?}", every.lock());
    let entries = h.entries(&c.id).await;
    assert!(
        entries.iter().any(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text.contains("changed while it waited"))),
        "{:?}",
        tool_results(&entries)
    );
}

/// What a guarded test machine's guard says when asked.
#[derive(Clone, Copy)]
enum Says {
    Signs(tessera_core::guard::State),
    Forges(tessera_core::guard::State),
    Nothing,
}

/// A machine with Tessera Guard: says hello with `key` (hex) and answers the
/// server's `node_attest` as `says` holds at the time, signed with `private`.
async fn guarded_machine(
    h: &Harness,
    device: &str,
    key: &str,
    private: Vec<u8>,
    says: Arc<Mutex<Says>>,
) -> Arc<Mutex<Vec<NodeCall>>> {
    let (conn, mut rx) = h.engine.node_conn(device);
    h.engine
        .node_request(guarded_hello(key), &conn)
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, keep) = (h.engine.clone(), seen.clone());
    let (other_private, _) = tessera_secure::sign::generate().unwrap();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                Event::NodeAttest { nonce } => {
                    let said = *says.lock();
                    let (state, with) = match said {
                        Says::Signs(s) => (s, &private),
                        Says::Forges(s) => (s, &other_private),
                        Says::Nothing => continue,
                    };
                    let message = tessera_core::guard::attest_message(&nonce, state);
                    let signature = tessera_core::guard::hex(
                        &tessera_secure::sign::sign(with, &message).unwrap(),
                    );
                    let _ = engine
                        .node_request(
                            Request::NodeAttestation {
                                nonce,
                                stopped: state.stopped,
                                stops: state.stops,
                                generation: state.generation,
                                signature,
                            },
                            &conn,
                        )
                        .await;
                }
                Event::NodeCall { call } => {
                    keep.lock().push(call.clone());
                    let _ = engine
                        .node_request(
                            Request::NodeReply {
                                call_id: call.call_id,
                                result: NodeResult {
                                    output: "ran it".into(),
                                    ok: true,
                                    refused: false,
                                    exit_code: Some(0),
                                    image: None,
                                },
                            },
                            &conn,
                        )
                        .await;
                }
                _ => {}
            }
        }
    });
    seen
}

/// The guarded test machine's hello, with guard key `key` (hex).
fn guarded_hello(key: &str) -> Request {
    Request::NodeHello {
        name: "Guarded".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: MachineScope {
            folders: vec![FolderAccess {
                path: "/home/me/code".into(),
                write: true,
            }],
            commands: true,
            ask: MachineAsk::Chat,
            ..Default::default()
        },
        generation: 1,
        sandbox: "on".into(),
        sandbox_ok: true,
        home: Some("/home/me".into()),
        computer: None,
        host: None,
        features: None,
        guard_key: Some(key.into()),
    }
}

/// One `make` on the guarded machine, run to its end.
async fn attempt(h: &Harness, id: &str) {
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_on("Guarded", "make"),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(h, id, "build it").await;
}

#[tokio::test]
async fn a_machine_with_tessera_guard_acts_only_on_its_guards_fresh_word() {
    use tessera_core::guard::State;
    let h = harness(vec![], 100_000).await;
    let (private, public) = tessera_secure::sign::generate().unwrap();
    let key = tessera_core::guard::hex(&public);
    let not_stopped = |generation| State {
        stopped: false,
        stops: generation as u32,
        generation,
    };
    let says = Arc::new(Mutex::new(Says::Signs(not_stopped(2))));
    let reached = guarded_machine(&h, "dev_g", &key, private.clone(), says.clone()).await;
    let c = chat(&h, None).await;
    // Its guard says not stopped, freshly: the action goes.
    attempt(&h, &c.id).await;
    assert_eq!(reached.lock().len(), 1);
    // Stopped there: refused (whatever the machine itself would do).
    *says.lock() = Says::Signs(State {
        stopped: true,
        stops: 3,
        generation: 3,
    });
    attempt(&h, &c.id).await;
    assert_eq!(reached.lock().len(), 1, "a Stop the guard signed holds");
    // Something else answering for the guard (another key): refused.
    *says.lock() = Says::Forges(not_stopped(9));
    attempt(&h, &c.id).await;
    assert_eq!(
        reached.lock().len(),
        1,
        "a word not signed by the pinned key"
    );
    // An older word than one already seen (a replay of before the Stop).
    *says.lock() = Says::Signs(not_stopped(2));
    attempt(&h, &c.id).await;
    assert_eq!(reached.lock().len(), 1, "older than the Stop seen");
    // No answer at all: refused.
    *says.lock() = Says::Nothing;
    attempt(&h, &c.id).await;
    assert_eq!(reached.lock().len(), 1, "no word, no action");
    // Resumed there (approved on that computer): a newer word lets it go.
    *says.lock() = Says::Signs(not_stopped(4));
    attempt(&h, &c.id).await;
    assert_eq!(reached.lock().len(), 2);
    let text = serde_json::to_string(&h.entries(&c.id).await).unwrap();
    for why in [
        "Tessera was stopped on that computer",
        "wasn't Tessera Guard's own",
        "older than one already seen",
        "didn't confirm that the computer isn't stopped",
    ] {
        assert!(text.contains(why), "{why}");
    }
    // The machine now says another key (or none): nothing runs there until
    // it is added again.
    let (other_private, other_public) = tessera_secure::sign::generate().unwrap();
    let says2 = Arc::new(Mutex::new(Says::Signs(not_stopped(5))));
    let again = guarded_machine(
        &h,
        "dev_g",
        &tessera_core::guard::hex(&other_public),
        other_private,
        says2,
    )
    .await;
    attempt(&h, &c.id).await;
    assert!(again.lock().is_empty(), "a changed guard key is refused");
    assert!(
        serde_json::to_string(&h.entries(&c.id).await)
            .unwrap()
            .contains("isn't the one it had")
    );
}

#[tokio::test]
async fn a_guarded_machines_word_from_a_connection_it_replaced_lets_nothing_through() {
    use tessera_core::guard::State;
    let h = harness(vec![], 100_000).await;
    let (private, public) = tessera_secure::sign::generate().unwrap();
    let key = tessera_core::guard::hex(&public);
    let fine = State {
        stopped: false,
        stops: 1,
        generation: 1,
    };
    // The first connection answers the ask only after the machine has said
    // hello again on a second one, and on the first: a good word, from a
    // connection the call would no longer go to.
    let (asked_tx, asked_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let first = Arc::new(Mutex::new(Vec::<NodeCall>::new()));
    {
        let (conn, mut rx) = h.engine.node_conn("dev_r");
        h.engine
            .node_request(guarded_hello(&key), &conn)
            .await
            .unwrap();
        let (engine, keep, private) = (h.engine.clone(), first.clone(), private.clone());
        tokio::spawn(async move {
            let mut waits = Some((asked_tx, go_rx));
            while let Some(ev) = rx.recv().await {
                match ev {
                    Event::NodeAttest { nonce } => {
                        if let Some((asked, go)) = waits.take() {
                            let _ = asked.send(());
                            let _ = go.await;
                        }
                        let message = tessera_core::guard::attest_message(&nonce, fine);
                        let signature = tessera_core::guard::hex(
                            &tessera_secure::sign::sign(&private, &message).unwrap(),
                        );
                        let _ = engine
                            .node_request(
                                Request::NodeAttestation {
                                    nonce,
                                    stopped: false,
                                    stops: 1,
                                    generation: 1,
                                    signature,
                                },
                                &conn,
                            )
                            .await;
                    }
                    Event::NodeCall { call } => keep.lock().push(call),
                    _ => {}
                }
            }
        });
    }
    let c = chat(&h, None).await;
    let says = Arc::new(Mutex::new(Says::Signs(fine)));
    let reconnect = async {
        asked_rx.await.unwrap();
        let second = guarded_machine(&h, "dev_r", &key, private.clone(), says.clone()).await;
        go_tx.send(()).unwrap();
        second
    };
    let ((), second) = tokio::join!(attempt(&h, &c.id), reconnect);
    assert!(first.lock().is_empty(), "sent on the old connection");
    assert!(second.lock().is_empty(), "sent on a connection not asked");
    // Refused for want of a word on the connection it has now (the old
    // one's answer isn't taken), never sent anywhere.
    let text = serde_json::to_string(&h.entries(&c.id).await).unwrap();
    assert!(
        text.contains("didn't confirm that the computer isn't stopped")
            || text.contains("reconnected, or its Tessera Guard changed, while it was asked"),
        "{text}"
    );
    // Asked again, on the connection it has now: it goes there.
    attempt(&h, &c.id).await;
    assert!(first.lock().is_empty());
    assert_eq!(second.lock().len(), 1);
}
