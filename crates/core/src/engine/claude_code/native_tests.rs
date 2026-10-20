//! Regression tests and reproducible byte measurements, with an empty native
//! test engine (no user configuration, memory or models).
use super::*;

#[tokio::test]
async fn native27_tools_and_context_measurement() {
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    {
        let mut st = rt.st.lock();
        st.internal.small = None;
        st.internal.lean_start = None;
    }
    let native = engine.0.callable_tools(&rt.st.lock());
    let tools = engine.0.claude_bridge_tools(&rt.st.lock());
    let names: Vec<_> = tools.iter().map(|t| t.name.clone()).collect();
    for name in [
        "write",
        "edit",
        "glob",
        "web_fetch",
        "note",
        "whiteboard",
        "memory",
        "notify",
        "share_file",
        "docs",
        "recall",
        "task",
        "schedule",
        "agent",
        "agents",
        "save_secret",
    ] {
        let a = native.iter().find(|t| t.name == name).unwrap();
        let b = tools.iter().find(|t| t.name == name).unwrap();
        if name == "agent" {
            assert_eq!(a.description, b.description);
        }
        if name != "save_secret" {
            assert_eq!(
                a.parameters, b.parameters,
                "{name} preserves its native schema"
            );
        }
    }
    let management = tools.iter().find(|t| t.name == "agents").unwrap();
    assert!(
        !management.parameters["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("start"))
    );
    assert!(management.parameters["properties"].get("tasks").is_none());
    assert!(management.parameters["properties"].get("wait").is_none());
    assert!(management.description.contains("Start with `agent`"));
    let edit = tools.iter().find(|t| t.name == "edit").unwrap();
    for rule in [
        "Only lines a read or grep",
        "`tag`",
        "older tag",
        "match once",
        "`all`",
    ] {
        assert!(edit.description.contains(rule), "edit keeps {rule}");
    }
    let ctx = cc::context(&names, None, &[]);
    assert!(ctx.len() < 1000, "bridge guidance {} bytes", ctx.len());
    assert!(ctx.contains("including cleared goals"));
    assert!(ctx.contains("never put credentials"));
    let cc::Rpc::Reply(reply) = cc::mcp_request(&json!({"id":1,"method":"tools/list"}), &tools)
    else {
        panic!()
    };
    let list = reply["result"]["tools"].as_array().unwrap();
    let mut eager = Vec::new();
    let mut deferred = Vec::new();
    for t in list {
        if t["_meta"]["anthropic/alwaysLoad"] == true {
            eager.push(t.clone());
        } else {
            deferred.push(t.clone());
        }
    }
    assert_eq!(eager.len(), 4);
    let name_bytes: usize = tools.iter().map(|t| t.name.len()).sum();
    let description_bytes: usize = tools.iter().map(|t| t.description.len()).sum();
    let schema_bytes: usize = tools
        .iter()
        .map(|t| serde_json::to_vec(&t.parameters).unwrap().len())
        .sum();
    // Reconstruct both prior lists on this registry, including the removed
    // bridge-only start schema; keep all measurements comparable.
    let mut review_before = tools.clone();
    let agent = native.iter().find(|t| t.name == "agent").unwrap();
    for t in &mut review_before {
        if t.name == "agents" {
            t.description = "Tessera helpers (not CLI Agent), sharing permissions, slots and worktrees. Start: tasks [{name,prompt,model:provider/model}], wait false (background) or true (join). Model: task, then type, then server default; a Claude default needs an explicit choice. Also status, stop, report, message, apply, discard. docs: subagents.".into();
            t.parameters["properties"]["action"]["enum"] = json!([
                "status", "stop", "report", "message", "apply", "discard", "approve", "decline",
                "start"
            ]);
            t.parameters["properties"]["tasks"] = agent.parameters["properties"]["tasks"].clone();
            t.parameters["properties"]["wait"] = agent.parameters["properties"]["wait"].clone();
        } else if t.name == "edit" {
            t.description = t.description.replace(
                "Returns the new tag, changed lines/numbers and new language-server errors.",
                "The result shows the new tag, the changed lines with their new numbers, and new errors a language server reports.",
            );
        } else if t.name == "read" {
            // Batch 27's multi-file read schema was not in the measured
            // 2838c49 list. Reconstruct that list, not today's schema.
            t.parameters["properties"]
                .as_object_mut()
                .unwrap()
                .remove("paths");
            t.parameters["required"] = json!(["path"]);
            t.description = "Read a text file as `[path#TAG]` plus numbered `N:content` lines (the TAG and numbers are what `edit` \
                takes), view an image, or list a directory. `offset` (1-based) and `limit` (default 1000) page through \
                large files.".into();
        } else if t.name == "write" {
            t.description = "Create or overwrite a file with `content` (parent directories are created). Use `edit` to change \
                existing files.".into();
        } else if t.name == "schedule" {
            t.description = "Manage scheduled jobs that run a prompt automatically in a fresh session (reports, checks, maintenance). `schedule` is a cron expression (\"0 9 * * 1-5\" = weekdays 09:00, server local time) or `@every 30m` / `@every 6h`. Actions: list, create, update, delete. Results appear as sessions in every client.".into();
            t.parameters = json!({"type":"object", "properties": {
                "action":{"type":"string", "enum":["list","create","update","delete"]},
                "id":{"type":"string"}, "name":{"type":"string"}, "schedule":{"type":"string"},
                "prompt":{"type":"string", "description":"Self-contained instructions for each run."},
                "enabled":{"type":"boolean"},
                "unattended":{"type":"boolean", "description":"Run without approval prompts (auto permission, if this session has it). Default true."}
            }, "required":["action"]});
        } else if t.name == "settings" {
            t.description = "Tessera's own settings: `get`, or `set` a `key` (e.g. memory.enabled, models.fast) to a JSON `value` (null resets), after the user approves. Security, providers and credentials are the user's.".into();
        } else if t.name == "browser" {
            // Reconstruct the historical schema and wording, not today's
            // upload action or the shorter prompt-budget description.
            t.description = "Real browser with the user's logins, in background tabs. `show` a tab only if the user asks to see it; \
                `needs_user` + `reason` hands it to them (a sign-in with no stored login, a CAPTCHA) and waits. \
                `web_fetch` is cheaper for plain pages. `open` a url, `snapshot` for refs to `click`/`type`; read \
                `docs browser`.".into();
            t.parameters["properties"]
                .as_object_mut()
                .unwrap()
                .remove("files");
            t.parameters["properties"]["action"]["enum"]
                .as_array_mut()
                .unwrap()
                .retain(|v| v != "upload");
        } else if t.name == "notify" {
            t.description = "Notify the user on their phone and computers, even with Tessera closed (tapping opens this chat). For what they asked to be told about, or a result they're waiting for; not progress. `at` sends it later. Recurring: `schedule` a job that calls notify.".into();
        }
        // Per-helper Daybreak Blue came later.
        if let Some(p) = t
            .parameters
            .pointer_mut("/properties/tasks/items/properties")
            .and_then(|p| p.as_object_mut())
        {
            p.remove("daybreak");
        }
    }
    let cc::Rpc::Reply(review_reply) =
        cc::mcp_request(&json!({"id":1,"method":"tools/list"}), &review_before)
    else {
        panic!()
    };
    let review_bytes = serde_json::to_vec(&review_reply).unwrap().len();
    assert_eq!(
        review_bytes, 18160,
        "2838c49 tools/list reconstruction: {review_reply}"
    );
    assert!(serde_json::to_vec(&reply).unwrap().len() < review_bytes);
    println!(
        "native27 REVIEW BEFORE bytes: descriptions={} schemas={} tools_list={review_bytes}",
        review_before
            .iter()
            .map(|t| t.description.len())
            .sum::<usize>(),
        review_before
            .iter()
            .map(|t| serde_json::to_vec(&t.parameters).unwrap().len())
            .sum::<usize>()
    );
    let mut before = review_before;
    before.retain(|t| !["write", "edit", "glob", "web_fetch", "agent"].contains(&t.name.as_str()));
    for t in &mut before {
        if t.name == "bash" {
            let base = &native
                .iter()
                .find(|n| n.name == "bash")
                .unwrap()
                .description;
            t.description = format!(
                "{base} Use this for ordinary commands too, instead of Bash or PowerShell, so permissions, live output and background jobs stay in Tessera. For a stored credential name its $VARIABLE; Tessera supplies and redacts the value."
            );
        } else if t.name == "agents" {
            t.description = "Start helpers on other models (GPT, local) instead of your own Agent, or to save Claude usage. Use action: start, tasks: [{name, prompt, model: provider/model}], wait: false (default) for background reports, true to wait. Model: task, else agent type, else server default; a Claude Code default needs an explicit choice. Same permissions, helper slots and worktrees. Also status, stop, report, message, apply, discard. See docs: subagents.".into();
        }
    }
    let cc::Rpc::Reply(old_reply) =
        cc::mcp_request(&json!({"id":1,"method":"tools/list"}), &before)
    else {
        panic!()
    };
    println!(
        "native27 BEFORE bytes: tools={} names={} descriptions={} schemas={} tools_list={}",
        before.len(),
        before.iter().map(|t| t.name.len()).sum::<usize>(),
        before.iter().map(|t| t.description.len()).sum::<usize>(),
        before
            .iter()
            .map(|t| serde_json::to_vec(&t.parameters).unwrap().len())
            .sum::<usize>(),
        serde_json::to_vec(&old_reply).unwrap().len()
    );
    let ledger = TaskLedger {
        goal: "Repair parser".into(),
        constraints: vec!["No deploy".into()],
        plan: vec![tessera_proto::PlanItem {
            text: "Test parser".into(),
            status: tessera_proto::PlanStatus::InProgress,
        }],
        done_criteria: vec!["Tests pass".into()],
        ..Default::default()
    };
    let (_, empty_ledger) = ledger_context(None);
    let (_, full_ledger) = ledger_context(Some(&ledger));
    let nudge = crate::tools::meta::completion_note(&ledger).unwrap();
    println!(
        "native27 unchanged injection bytes: empty_ledger={} sample_ledger={} completion_nudge={}",
        empty_ledger.len(),
        full_ledger.len(),
        nudge.len()
    );
    println!(
        "native27 bytes: context={} bare_context={} tools={} names={} descriptions={} schemas={} tools_list={} eager_4={} deferred_{}={}",
        ctx.len(),
        cc::context(&[], None, &[]).len(),
        tools.len(),
        name_bytes,
        description_bytes,
        schema_bytes,
        serde_json::to_vec(&reply).unwrap().len(),
        serde_json::to_vec(&eager).unwrap().len(),
        deferred.len(),
        serde_json::to_vec(&deferred).unwrap().len()
    );
}

#[tokio::test]
async fn native27_attendance_matches_provider_policy() {
    let (_engine, _dir, rt) = super::super::test_support::engine().await;
    let mut info = rt.st.lock().info.clone();
    info.kind = tessera_proto::SessionKind::Chat;
    info.permission = PermissionMode::Default;
    info.internal = false;
    info.working_for = None;
    assert!(!limits::unattended(&info, Some(1_000_000), 1_000_100));
    assert!(!limits::unattended(&info, None, 1_000_100));
    assert!(limits::unattended(&info, Some(1), 1_000_100));
    info.permission = PermissionMode::Auto;
    assert!(limits::unattended(&info, None, 1_000_100));
    info.permission = PermissionMode::Default;
    for kind in [
        tessera_proto::SessionKind::Job,
        tessera_proto::SessionKind::Subagent,
    ] {
        info.kind = kind;
        assert!(limits::unattended(&info, Some(1_000_000), 1_000_100));
    }
}

#[tokio::test]
async fn native27_non_main_direct_mcp_never_creates_helpers() {
    for provenance in ["unknown", "child", "denied-hook"] {
        for name in ["agent", "agents"] {
            let (engine, _dir, rt) = super::super::test_support::engine().await;
            let (mut turn, cwd, tools) = {
                let st = rt.st.lock();
                let cwd = PathBuf::from(&st.info.cwd);
                let turn = Turn::new(
                    st.info.id.clone(),
                    st.info.model.clone(),
                    cwd.clone(),
                    PermissionMode::Auto,
                    None,
                );
                (turn, cwd, engine.0.claude_bridge_tools(&st))
            };
            assert!(tools.iter().any(|t| t.name == name));
            turn.bridge = Some(Bridge {
                tools,
                done: Default::default(),
                cancel: CancellationToken::new(),
            });
            let mut args = json!({"tasks":[{"name":"must not exist", "prompt":"Read only", "readonly":true, "model":"fixture/model"}], "wait":true});
            if name == "agents" {
                args["action"] = json!("start");
            }
            let hook = json!({"agent_id":"cli-child", "agent_type":"Explore", "tool_name":format!("mcp__tessera__{name}"), "input":args});
            if provenance != "unknown" {
                turn.policy_hooks.insert("child-call".into(), hook.clone());
            }
            if provenance == "denied-hook" {
                assert!(matches!(
                    engine.0.claude_policy(
                        &rt,
                        &cwd,
                        &format!("mcp__tessera__{name}"),
                        &args,
                        &hook
                    ),
                    Policy::Deny(_)
                ));
            }
            // No main-call admission; directly send even after the hook denied.
            let before = engine.0.store.list_sessions(false).unwrap().len();
            let (tx, mut rx) = mpsc::unbounded_channel();
            engine.0.claude_mcp(&rt, &mut turn, "request".into(), &json!({"server_name":cc::SERVER, "message":{"id":1,"method":"tools/call","params":{"name":name,"arguments":args,"_meta":{"claudecode/toolUseId":"child-call"}}}}), Some(&tx));
            let line = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(
                reply.pointer("/response/response/mcp_response/result/isError"),
                Some(&json!(true)),
                "{provenance}/{name}: {reply}"
            );
            assert!(line.contains("Delegation requires"), "{line}");
            assert!(turn.bridge.as_ref().unwrap().done.lock().is_empty());
            assert_eq!(
                engine.0.store.list_sessions(false).unwrap().len(),
                before,
                "{provenance}/{name}: no helper session created"
            );
            let id = rt.st.lock().info.id.clone();
            assert!(engine.0.subagents_of(&id).is_empty());
        }
    }
}

fn test_turn(engine: &Engine, rt: &Rt) -> Turn {
    let st = rt.st.lock();
    let mut t = Turn::new(
        st.info.id.clone(),
        st.info.model.clone(),
        PathBuf::from(&st.info.cwd),
        st.info.permission,
        None,
    );
    t.bridge = Some(Bridge {
        tools: engine.0.claude_bridge_tools(&st),
        done: Default::default(),
        cancel: CancellationToken::new(),
    });
    t
}

async fn policy_hook(
    engine: &Engine,
    rt: &Arc<Rt>,
    t: &mut Turn,
    name: &str,
    args: &Value,
) -> String {
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Production adapter inserts null for absent metadata. Do not bypass it
    // with a hand-written policy_req that omits those keys.
    engine.0.claude_hook(
        rt,
        t,
        "admission".into(),
        &json!({
            "callback_id":cc::HOOK_POLICY, "tool_use_id":"main-call",
            "input":{"tool_name":format!("mcp__tessera__{name}"), "tool_input":args}
        }),
        Some(&tx),
    );
    rx.recv().await.unwrap()
}

async fn admit_main(engine: &Engine, rt: &Arc<Rt>, t: &mut Turn, name: &str, args: &Value) {
    let answer = policy_hook(engine, rt, t, name, args).await;
    assert!(!answer.contains("permissionDecision\":\"deny"), "{answer}");
    assert!(t.admissions.lock().admitted.contains_key("main-call"));
}

async fn direct_call(
    engine: &Engine,
    rt: &Arc<Rt>,
    t: &mut Turn,
    name: &str,
    args: Value,
    id: Value,
) -> Value {
    let (tx, mut rx) = mpsc::unbounded_channel();
    engine.0.claude_mcp(rt, t, "direct".into(), &json!({"server_name":cc::SERVER,"message":{"id":1,"method":"tools/call","params":{"name":name,"arguments":args,"_meta":{"claudecode/toolUseId":id}}}}), Some(&tx));
    let line = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    v.pointer("/response/response/mcp_response/result")
        .unwrap()
        .clone()
}

fn state_snapshot(engine: &Engine) -> Value {
    let sessions = engine.0.store.list_sessions(false).unwrap();
    let histories: Vec<_> = sessions
        .iter()
        .map(|s| (s.id.clone(), engine.0.store.entries(&s.id, 0).unwrap()))
        .collect();
    json!({"sessions":sessions,"jobs":engine.0.store.jobs().unwrap(),"histories":histories})
}

async fn configure_delegation_destination(engine: &Engine, rt: &Rt) {
    let (model, cwd) = {
        let st = rt.st.lock();
        (st.info.model.clone(), st.info.cwd.clone())
    };
    let bot: Bot = serde_json::from_value(
        engine
            .handle(
                Request::SaveBot {
                    bot: Bot {
                        id: String::new(),
                        name: "Fixture".into(),
                        handle: "fixture".into(),
                        avatar: String::new(),
                        color: String::new(),
                        role: String::new(),
                        personality: String::new(),
                        instructions: String::new(),
                        model: Some(model),
                        thinking: None,
                        permission: PermissionMode::Auto,
                        tools: ToolPreset::Full,
                        allowed_tools: vec![],
                        cwd: Some(cwd),
                        own_memory: false,
                        share_user_profile: false,
                        can_message: vec![],
                        askable: None,
                        knowledge: vec![],
                        starters: vec![],
                        machines: None,
                        secrets: None,
                        created_at: 0,
                        updated_at: 0,
                    },
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    // A real configured destination and existing chat make history assertions
    // meaningful without sending it to any model.
    engine
        .handle(
            Request::CreateSession {
                cwd: None,
                model: None,
                title: None,
                bot_id: Some(bot.id),
                private: false,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn native27_delegation_requires_exact_main_admission_not_an_open_card() {
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    rt.st.lock().info.permission = PermissionMode::Auto;
    configure_delegation_destination(&engine, &rt).await;
    let good =
        json!({"action":"create","schedule":"@every 1h","prompt":"No inference; fixture only"});
    let mut t = test_turn(&engine, &rt);
    t.open.push(("main-call".into(), "schedule".into()));
    admit_main(&engine, &rt, &mut t, "schedule", &good).await;
    let result = direct_call(
        &engine,
        &rt,
        &mut t,
        "schedule",
        good.clone(),
        json!("main-call"),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(engine.0.store.jobs().unwrap().len(), 1);
    assert!(
        t.admissions.lock().admitted.is_empty(),
        "single-use admission"
    );
    let before_replay = state_snapshot(&engine);
    let replay = direct_call(
        &engine,
        &rt,
        &mut t,
        "schedule",
        good.clone(),
        json!("main-call"),
    )
    .await;
    assert_eq!(replay["isError"], true);
    assert_eq!(state_snapshot(&engine), before_replay);
    let job = engine.0.store.jobs().unwrap()[0].id.clone();
    let cases = vec![
        (
            "agent",
            json!({"tasks":[{"name":"bad","prompt":"bad","model":"fixture/model"}]}),
        ),
        (
            "agents",
            json!({"action":"start","tasks":[{"name":"bad","prompt":"bad","model":"fixture/model"}]}),
        ),
        (
            "agents",
            json!({"action":"message","name":"existing","text":"bad"}),
        ),
        (
            "agents",
            json!({"action":"resume","name":"existing","prompt":"bad"}),
        ),
        (
            "message_bot",
            json!({"bot":"@fixture","message":"must not reach history","wait":false}),
        ),
        (
            "message_bot",
            json!({"bot":"@fixture","message":"must not reach history","wait":true}),
        ),
        ("schedule", good),
        (
            "schedule",
            json!({"action":"update","id":job,"prompt":"bad"}),
        ),
        (
            "schedule",
            json!({"action":"update","id":job,"enabled":true}),
        ),
        ("schedule", json!({"action":"delete","id":job})),
    ];
    for (name, args) in cases {
        for fault in [
            "missing-hook",
            "no-admission",
            "missing-id",
            "empty-id",
            "numeric-id",
            "malformed-input",
            "id",
            "tool",
            "hook-id",
            "args",
            "child",
            "empty-child",
            "malformed-actor",
            "changed-main-actor",
            "regate-changed-actor",
            "denied",
            "actual-child-denial",
            "mode",
            "replay",
            "closed-reply",
        ] {
            let mut t = test_turn(&engine, &rt);
            t.open.push(("main-call".into(), name.into()));
            let mut sent = args.clone();
            let mut id = json!("main-call");
            if fault == "closed-reply" {
                let (tx, rx) = mpsc::unbounded_channel();
                drop(rx);
                engine.0.claude_gate(&rt, &mut t, "closed".into(), json!({"tool_use_id":"main-call","tool_name":format!("mcp__tessera__{name}"),"input":args}), tx, true);
            } else {
                admit_main(&engine, &rt, &mut t, name, &args).await;
            }
            match fault {
                "missing-hook" => {
                    t.policy_hooks.clear();
                }
                "no-admission" | "replay" => {
                    t.admissions.lock().admitted.clear();
                }
                "missing-id" => {
                    id = Value::Null;
                }
                "empty-id" => {
                    id = json!("");
                }
                "numeric-id" => {
                    id = json!(42);
                }
                "malformed-input" => {
                    sent = Value::Null;
                }
                "id" => {
                    id = json!("another-call");
                    t.open.push(("another-call".into(), name.into()));
                }
                "tool" => {
                    t.policy_hooks.get_mut("main-call").unwrap()["tool_name"] =
                        json!("mcp__tessera__docs");
                }
                "hook-id" => {
                    t.policy_hooks.get_mut("main-call").unwrap()["tool_use_id"] =
                        json!("other-call");
                }
                "args" => {
                    sent["extra"] = json!(true);
                }
                "child" => {
                    t.policy_hooks.get_mut("main-call").unwrap()["agent_id"] = json!("child");
                }
                "empty-child" => {
                    t.policy_hooks.get_mut("main-call").unwrap()["agent_id"] = json!("");
                }
                "malformed-actor" => {
                    t.policy_hooks.get_mut("main-call").unwrap()["permission_mode"] = json!({});
                }
                "changed-main-actor" => {
                    t.policy_hooks.get_mut("main-call").unwrap()["permission_mode"] = json!("plan");
                }
                "regate-changed-actor" => {
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    engine.0.claude_gate(&rt, &mut t, "changed".into(), json!({"tool_use_id":"main-call","tool_name":format!("mcp__tessera__{name}"),"input":args,"permission_mode":"plan"}), tx, false);
                    rx.recv().await.unwrap();
                    assert!(
                        t.admissions.lock().admitted.is_empty(),
                        "actor mismatch cannot renew admission"
                    );
                }
                "denied" => {
                    t.denied
                        .lock()
                        .insert("main-call".into(), ApprovalOutcome::Rule("declined".into()));
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    engine.0.claude_gate(&rt, &mut t, "denied".into(), json!({"tool_use_id":"main-call","tool_name":format!("mcp__tessera__{name}"),"input":args}), tx, true);
                    assert!(
                        rx.recv()
                            .await
                            .unwrap()
                            .contains("permissionDecision\":\"deny")
                    );
                    assert!(t.admissions.lock().admitted.is_empty());
                }
                "actual-child-denial" => {
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    engine.0.claude_gate(&rt, &mut t, "child-denied".into(), json!({"tool_use_id":"main-call","tool_name":format!("mcp__tessera__{name}"),"input":args,"agent_id":"child","agent_type":"Explore"}), tx, true);
                    assert!(
                        rx.recv()
                            .await
                            .unwrap()
                            .contains("permissionDecision\":\"deny")
                    );
                    assert!(t.denied.lock().contains_key("main-call"));
                    assert!(t.admissions.lock().admitted.is_empty());
                    // A later can_use_tool without child metadata cannot undo
                    // a mandatory-hook refusal by borrowing the open card.
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    engine.0.claude_gate(&rt, &mut t, "later".into(), json!({"tool_use_id":"main-call","tool_name":format!("mcp__tessera__{name}"),"input":args}), tx, false);
                    assert!(rx.recv().await.unwrap().contains("\"behavior\":\"deny\""));
                    assert!(t.admissions.lock().admitted.is_empty());
                }
                "mode" => {
                    rt.st.lock().info.permission = PermissionMode::ReadOnly;
                }
                _ => {}
            }
            let before = state_snapshot(&engine);
            let result = direct_call(&engine, &rt, &mut t, name, sent, id).await;
            assert_eq!(result["isError"], true, "{name}/{fault}: {result}");
            assert!(
                result.to_string().contains("Delegation requires")
                    || (name == "schedule"
                        && fault == "malformed-input"
                        && result.to_string().contains("Unknown schedule action")),
                "{name}/{fault}: {result}"
            );
            assert_eq!(
                state_snapshot(&engine),
                before,
                "{name}/{fault}: sessions/jobs/history unchanged"
            );
            assert!(t.bridge.as_ref().unwrap().done.lock().is_empty());
            rt.st.lock().info.permission = PermissionMode::Auto;
        }
    }
}

#[tokio::test]
async fn native27_schedule_actions_are_exact_for_every_caller_and_dispatch() {
    use crate::tools::Tool;
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    rt.st.lock().info.permission = PermissionMode::Auto;
    let (session_id, cwd, admission) = {
        let st = rt.st.lock();
        (
            st.info.id.clone(),
            PathBuf::from(&st.info.cwd),
            engine.0.action_admission(&st),
        )
    };
    let ctx = ToolCtx {
        session_id,
        call_id: "schedule".into(),
        seq: 0,
        cwd,
        cancel: CancellationToken::new(),
        host: Arc::new(Host(engine.0.clone(), false)),
        progress: Arc::new(|_| {}),
        depth: 0,
        cleared: None,
        admission,
    };
    for action in [
        Value::Null,
        json!("Create"),
        json!("UPDATE"),
        json!("Delete"),
        json!("list "),
        json!("unknown"),
        json!(42),
    ] {
        let args = json!({"action":action,"schedule":"@every 1h","prompt":"bad"});
        assert!(matches!(
            crate::tools::meta::Schedule.assess(&args, &ctx),
            Risk::Block { .. }
        ));
        let before = state_snapshot(&engine);
        assert!(
            crate::tools::meta::Schedule
                .run(args.clone(), &ctx)
                .await
                .is_error
        );
        for main in [false, true] {
            let mut t = test_turn(&engine, &rt);
            if main {
                t.open.push(("main-call".into(), "schedule".into()));
            }
            let out = direct_call(
                &engine,
                &rt,
                &mut t,
                "schedule",
                args.clone(),
                json!("main-call"),
            )
            .await;
            assert_eq!(out["isError"], true);
            assert!(out.to_string().contains("Unknown schedule action"));
        }
        assert_eq!(state_snapshot(&engine), before);
    }
}

// Keep the real tool name/schema and production hook/MCP path, but replace
// inference/side effects with an exact dispatch counter in the temp engine.
struct DelegationProbe {
    tool: Arc<dyn crate::tools::Tool>,
    calls: Arc<Mutex<Vec<Value>>>,
    started: Arc<tokio::sync::Notify>,
    release: Option<Arc<tokio::sync::Notify>>,
}

impl crate::tools::Tool for DelegationProbe {
    fn name(&self) -> &'static str {
        self.tool.name()
    }
    fn description(&self) -> String {
        self.tool.description()
    }
    fn schema(&self) -> Value {
        self.tool.schema()
    }
    fn kind(&self) -> crate::tools::ToolKind {
        self.tool.kind()
    }
    fn run<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolCtx,
    ) -> futures::future::BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            self.calls.lock().push(args);
            self.started.notify_one();
            if let Some(release) = &self.release {
                release.notified().await;
            }
            ToolOutput::text("Fixture delegation dispatched")
        })
    }
}

#[tokio::test]
async fn native27_consumed_delegation_hook_and_mcp_replays_never_rearm() {
    for (name, args) in [
        (
            "schedule",
            json!({"action":"create","schedule":"@every 1h","prompt":"fixture"}),
        ),
        (
            "message_bot",
            json!({"bot":"@fixture","message":"fixture","wait":false}),
        ),
        (
            "message_bot",
            json!({"bot":"@fixture","message":"fixture","wait":true}),
        ),
        (
            "agent",
            json!({"tasks":[{"name":"fixture","prompt":"fixture","model":"fixture/model"}]}),
        ),
    ] {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        // Inventory admission offers message_bot only with a reachable bot.
        // Configure the destination while the probe still prevents inference.
        configure_delegation_destination(&engine, &rt).await;
        let tool = engine.0.registry.read().get(name).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        engine.0.registry.write().add(Arc::new(DelegationProbe {
            tool,
            calls: calls.clone(),
            started: Arc::new(tokio::sync::Notify::new()),
            release: None,
        }));
        let mut t = test_turn(&engine, &rt);
        t.open.push(("main-call".into(), name.into()));
        admit_main(&engine, &rt, &mut t, name, &args).await;
        let first = direct_call(&engine, &rt, &mut t, name, args.clone(), json!("main-call")).await;
        assert_eq!(first["isError"], false, "{name}: {first}");
        assert_eq!(*calls.lock(), vec![args.clone()]);
        assert!(t.admissions.lock().consumed.contains("main-call"));
        assert!(t.admissions.lock().in_flight.is_empty());
        let before = state_snapshot(&engine);
        let hook = policy_hook(&engine, &rt, &mut t, name, &args).await;
        assert!(
            hook.contains("permissionDecision\":\"deny"),
            "{name}: {hook}"
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        engine.0.claude_permission(
            &rt,
            &mut t,
            "repeat-permission".into(),
            json!({
                "tool_use_id":"main-call", "tool_name":format!("mcp__tessera__{name}"), "input":args
            }),
            Some(&tx),
            &CancellationToken::new(),
        );
        assert!(rx.recv().await.unwrap().contains("\"behavior\":\"deny\""));
        let replay = direct_call(&engine, &rt, &mut t, name, args, json!("main-call")).await;
        assert_eq!(replay["isError"], true, "{name}: {replay}");
        assert_eq!(calls.lock().len(), 1, "{name}: no second dispatch");
        assert!(t.admissions.lock().admitted.is_empty());
        assert_eq!(state_snapshot(&engine), before);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native27_concurrent_late_gate_answer_cannot_rearm_in_flight_or_consumed_call() {
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    let args = json!({"action":"create","schedule":"@every 1h","prompt":"fixture"});
    let calls = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let tool = engine.0.registry.read().get("schedule").unwrap();
    engine.0.registry.write().add(Arc::new(DelegationProbe {
        tool,
        calls: calls.clone(),
        started: started.clone(),
        release: Some(release.clone()),
    }));
    let mut t = test_turn(&engine, &rt);
    t.open.push(("main-call".into(), "schedule".into()));
    admit_main(&engine, &rt, &mut t, "schedule", &args).await;
    let late_admission = MainAdmission {
        tool: "mcp__tessera__schedule".into(),
        input: args.clone(),
        actor: t.policy_hooks["main-call"].clone(),
        mode: PermissionMode::Auto,
    };
    let admissions = t.admissions.clone();
    let (gate_send, gate_answer) = tokio::sync::oneshot::channel();
    let (late_tx, mut late_rx) = mpsc::unbounded_channel();
    // Capture a gate answer before dispatch, then release it on another worker
    // only after MCP has consumed the original. Use the production publisher.
    let late = tokio::spawn(async move {
        gate_answer.await.unwrap();
        admissions.lock().publish("main-call".into(), Some(late_admission), |available| {
            late_tx.send(cc::hook_line("late", if available { json!({}) } else {
                json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"}})
            })).is_ok()
        });
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    engine.0.claude_mcp(
        &rt,
        &mut t,
        "running".into(),
        &json!({
            "server_name":cc::SERVER,"message":{"id":1,"method":"tools/call","params":{
                "name":"schedule","arguments":args,"_meta":{"claudecode/toolUseId":"main-call"}
            }}
        }),
        Some(&tx),
    );
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    assert!(t.admissions.lock().in_flight.contains("main-call"));
    gate_send.send(()).unwrap();
    late.await.unwrap();
    assert!(
        late_rx
            .recv()
            .await
            .unwrap()
            .contains("permissionDecision\":\"deny")
    );
    assert!(t.admissions.lock().admitted.is_empty());
    assert!(
        policy_hook(&engine, &rt, &mut t, "schedule", &args)
            .await
            .contains("permissionDecision\":\"deny")
    );
    let replay = direct_call(
        &engine,
        &rt,
        &mut t,
        "schedule",
        args.clone(),
        json!("main-call"),
    )
    .await;
    assert_eq!(replay["isError"], true);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(t.admissions.lock().in_flight.is_empty());
    assert!(t.admissions.lock().consumed.contains("main-call"));
    assert!(
        policy_hook(&engine, &rt, &mut t, "schedule", &args)
            .await
            .contains("permissionDecision\":\"deny")
    );
    assert_eq!(calls.lock().len(), 1);
}

#[tokio::test]
async fn native27_production_null_hook_provenance_rejects_explicit_conflicts() {
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    let args = json!({"action":"create","schedule":"@every 1h","prompt":"fixture"});
    // Omitted fields inherit. Explicit null is accepted only when it matches
    // the mandatory hook's actual null. Explicit child/plan/malformed never do.
    for metadata in [
        json!({}),
        json!({"agent_id":null,"permission_mode":null}),
        json!({"agent_id":"child"}),
        json!({"permission_mode":"plan"}),
        json!({"agent_id":{}}),
        json!({"permission_mode":42}),
        json!({"agent_type":[]}),
        json!({"cwd":{}}),
        json!({"mcp_server":42}),
    ] {
        let mut t = test_turn(&engine, &rt);
        t.open.push(("main-call".into(), "schedule".into()));
        admit_main(&engine, &rt, &mut t, "schedule", &args).await;
        assert_eq!(t.policy_hooks["main-call"]["agent_id"], Value::Null);
        let mut req =
            json!({"tool_use_id":"main-call","tool_name":"mcp__tessera__schedule","input":args});
        req.as_object_mut()
            .unwrap()
            .extend(metadata.as_object().unwrap().clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        engine.0.claude_permission(
            &rt,
            &mut t,
            "permission".into(),
            req,
            Some(&tx),
            &CancellationToken::new(),
        );
        let response = rx.recv().await.unwrap();
        let positive = metadata.as_object().unwrap().values().all(Value::is_null);
        assert_eq!(
            response.contains("\"behavior\":\"allow\""),
            positive,
            "{metadata}: {response}"
        );
        assert_eq!(
            t.admissions.lock().admitted.contains_key("main-call"),
            positive
        );
        assert_eq!(
            t.policy_hooks["main-call"]["agent_id"],
            Value::Null,
            "original unchanged"
        );
        let before = state_snapshot(&engine);
        let out = direct_call(
            &engine,
            &rt,
            &mut t,
            "schedule",
            args.clone(),
            json!("main-call"),
        )
        .await;
        assert_eq!(out["isError"], !positive, "{metadata}: {out}");
        if !positive {
            assert_eq!(state_snapshot(&engine), before);
        }
    }
}

#[tokio::test]
async fn native27_explicit_cli_helper_resume_is_refused_by_policy_hook() {
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    let mut t = test_turn(&engine, &rt);
    let before = state_snapshot(&engine);
    let (tx, mut rx) = mpsc::unbounded_channel();
    engine.0.claude_hook(&rt, &mut t, "resume".into(), &json!({
        "callback_id":cc::HOOK_POLICY,"tool_use_id":"resume-call","input":{
            "tool_name":"Agent","tool_input":{"resume":"existing-cli-helper","prompt":"continue"},
            "agent_id":"child","agent_type":"Explore"
        }
    }), Some(&tx));
    assert!(
        rx.recv()
            .await
            .unwrap()
            .contains("permissionDecision\":\"deny")
    );
    assert_eq!(state_snapshot(&engine), before);
}

#[tokio::test]
async fn native27_failed_delegation_dispatch_also_consumes_admission() {
    let (engine, _dir, rt) = super::super::test_support::engine().await;
    let args = json!({"action":"create","schedule":"@every 1h","prompt":"fixture"});
    let mut t = test_turn(&engine, &rt);
    t.open.push(("main-call".into(), "schedule".into()));
    admit_main(&engine, &rt, &mut t, "schedule", &args).await;
    let before = state_snapshot(&engine);
    let mut changed = args.clone();
    changed["prompt"] = json!("changed");
    let result = direct_call(
        &engine,
        &rt,
        &mut t,
        "schedule",
        changed,
        json!("main-call"),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(t.admissions.lock().consumed.contains("main-call"));
    assert!(
        policy_hook(&engine, &rt, &mut t, "schedule", &args)
            .await
            .contains("permissionDecision\":\"deny")
    );
    let replay = direct_call(&engine, &rt, &mut t, "schedule", args, json!("main-call")).await;
    assert_eq!(replay["isError"], true);
    assert_eq!(state_snapshot(&engine), before);
}
