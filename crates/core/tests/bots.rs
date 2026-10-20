//! Bots, bot chats, private bot-to-bot messages and multi-bot channels.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_proto::*;

fn bot(name: &str, role: &str, tools: ToolPreset, can: &[&str]) -> Bot {
    Bot {
        id: String::new(),
        name: name.into(),
        handle: String::new(),
        avatar: String::new(),
        color: String::new(),
        role: role.into(),
        personality: format!("{name} is warm and concise."),
        instructions: format!("{name} takes care of {role}."),
        model: None,
        thinking: None,
        permission: PermissionMode::Default,
        tools,
        allowed_tools: vec![],
        cwd: None,
        own_memory: true,
        share_user_profile: true,
        can_message: can.iter().map(|s| s.to_string()).collect(),
        askable: None,
        knowledge: Vec::new(),
        starters: Vec::new(),
        machines: None,
        secrets: None,
        created_at: 0,
        updated_at: 0,
    }
}

async fn save(h: &Harness, b: Bot) -> Bot {
    serde_json::from_value(
        h.engine
            .handle(Request::SaveBot { bot: b }, None)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn chat_with(h: &Harness, b: &Bot) -> SessionInfo {
    let req = Request::CreateSession {
        cwd: Some(h.work.display().to_string()),
        model: None,
        title: None,
        bot_id: Some(b.id.clone()),
        private: false,
        lean: false,
    };
    serde_json::from_value(h.engine.handle(req, None).await.unwrap()).unwrap()
}

fn tool_names(req: &serde_json::Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn bots_are_validated_and_unique() {
    let h = harness(vec![], 100_000).await;
    let nova = save(
        &h,
        bot(
            "Nova Prime",
            "personal assistant",
            ToolPreset::Assistant,
            &[],
        ),
    )
    .await;
    assert_eq!(nova.handle, "nova-prime");
    assert!(nova.id.starts_with("bot_"));
    assert_eq!(nova.avatar, "N");
    assert!(nova.color.starts_with('#'));
    let mut dup = bot("Other", "x", ToolPreset::Full, &[]);
    dup.handle = "@Nova-Prime".into();
    let err = h
        .engine
        .handle(Request::SaveBot { bot: dup }, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let mut reserved = bot("User", "x", ToolPreset::Full, &[]);
    reserved.handle = "user".into();
    assert!(
        h.engine
            .handle(Request::SaveBot { bot: reserved }, None)
            .await
            .is_err()
    );
    // Edits keep the id; list reflects them.
    let mut edited = nova.clone();
    edited.role = "chief of staff".into();
    let edited = save(&h, edited).await;
    assert_eq!(edited.id, nova.id);
    let list: Vec<Bot> =
        serde_json::from_value(h.engine.handle(Request::ListBots, None).await.unwrap()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].role, "chief of staff");
    // Mosaic portraits are kept whole; anything else is cut to an emoji's length.
    let mut portrait = edited.clone();
    portrait.avatar = "m:laurel-beard".into();
    assert_eq!(save(&h, portrait).await.avatar, "m:laurel-beard");
    let mut odd = edited.clone();
    odd.avatar = "m:../../x<script>".into();
    assert_eq!(save(&h, odd).await.avatar.chars().count(), 4);
}

#[tokio::test]
async fn bot_chat_uses_persona_tools_and_authorship() {
    let h = harness(vec![Reply::Text("Hi, I'm Nova.".into())], 100_000).await;
    let nova = save(
        &h,
        bot("Nova", "personal assistant", ToolPreset::Assistant, &[]),
    )
    .await;
    let s = chat_with(&h, &nova).await;
    assert_eq!(s.bot_id.as_deref(), Some(nova.id.as_str()));
    h.send(&s.id, "hello").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    let system = req["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.starts_with("You are Nova (@nova), personal assistant"),
        "{}",
        &system[..120]
    );
    assert!(system.contains("Nova is warm and concise."));
    let names = tool_names(&req);
    assert!(
        names.contains(&"web_search".to_string()) && !names.contains(&"bash".to_string()),
        "assistant preset has no shell: {names:?}"
    );
    let entries = h.entries(&s.id).await;
    assert!(
        entries.iter().any(
            |e| matches!(&e.body, EntryBody::Assistant { author: Some(a), .. } if *a == nova.id)
        )
    );
}

#[tokio::test]
async fn bots_keep_their_own_memory_but_share_the_user_profile() {
    let h = harness(
        vec![
            Reply::Tools(vec![
                ("memory".into(), json!({"target": "memory", "action": "add", "content": "The deploy key lives in the vault"})),
                ("memory".into(), json!({"target": "user", "action": "add", "content": "User's name is Alex"})),
            ]),
            Reply::Text("noted".into()),
        ],
        100_000,
    )
    .await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let s = chat_with(&h, &forge).await;
    h.send(&s.id, "remember these").await;
    h.settle(&s.id).await;
    let shared: MemoryState =
        serde_json::from_value(h.engine.handle(Request::GetMemory, None).await.unwrap()).unwrap();
    assert!(shared.user.contains("Alex"), "user profile is shared");
    assert!(
        !shared.memory.contains("deploy key"),
        "bot notes stay private to the bot"
    );
    let bot_notes = std::fs::read_to_string(
        h.dir
            .path()
            .join("bots")
            .join(&forge.id)
            .join("memory/MEMORY.md"),
    )
    .unwrap();
    assert!(bot_notes.contains("deploy key"));
}

/// A bot with its own memory is told which memory is whose (its work and
/// what the user wants from it in its own; only what every assistant needs
/// in the shared profile); a plain chat's prompt doesn't change.
#[tokio::test]
async fn a_bot_with_its_own_memory_knows_which_memory_is_whose() {
    let h = harness(
        vec![Reply::Text("ok".into()), Reply::Text("ok".into())],
        100_000,
    )
    .await;
    let scout = save(&h, bot("Scout", "job search", ToolPreset::Assistant, &[])).await;
    let s = chat_with(&h, &scout).await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let system = h.mock.agent_requests()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        system.contains("MEMORY is yours alone (@scout)")
            && system.contains("their criteria, preferences, places, sites and contacts")
            && system.contains("never the details of your own work"),
        "{system}"
    );
    let plain = h.session().await;
    h.send(&plain.id, "hi").await;
    h.settle(&plain.id).await;
    let system = h.mock.agent_requests()[1]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!system.contains("yours alone"), "{system}");
}

#[tokio::test]
async fn bots_message_each_other_privately_within_permissions() {
    let h = harness(vec![], 100_000).await;
    let nova = save(
        &h,
        bot(
            "Nova",
            "personal assistant",
            ToolPreset::Assistant,
            &["forge"],
        ),
    )
    .await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    // Nova asks Forge; Forge answers; Nova wraps up.
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@forge", "message": "Please check the build.", "wait": true}),
    )]));
    h.mock.push(Reply::Text("Build is green.".into()));
    h.mock
        .push(Reply::Text("Forge says the build is green.".into()));
    let s = chat_with(&h, &nova).await;
    h.send(&s.id, "is the build ok?").await;
    h.settle(&s.id).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1
            && results[0].2.contains("@forge replied")
            && results[0].2.contains("Build is green."),
        "{results:?}"
    );
    // The DM is a channel session the user can inspect, with authored messages.
    let sessions: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let dm = sessions
        .iter()
        .find(|x| x.kind == SessionKind::Channel)
        .expect("dm channel");
    assert_eq!(dm.bot_id.as_deref(), Some(forge.id.as_str()));
    assert_eq!(dm.members, vec![nova.id.clone(), forge.id.clone()]);
    assert_eq!(dm.title, "@nova ↔ @forge");
    let dm_entries = h.entries(&dm.id).await;
    assert!(
        matches!(&dm_entries[0].body, EntryBody::User { author: Some(a), .. } if *a == nova.id)
    );
    // Forge saw the request labelled with Nova's handle, under Forge's persona and tools.
    let forge_req = h
        .mock
        .agent_requests()
        .into_iter()
        .find(|r| {
            r["messages"][0]["content"]
                .as_str()
                .unwrap()
                .starts_with("You are Forge")
        })
        .unwrap();
    assert!(
        forge_req
            .to_string()
            .contains("[@nova]: Please check the build.")
    );
    assert!(tool_names(&forge_req).contains(&"bash".to_string()));

    // Forge may not message Nova (not in its contacts): the tool is not even offered.
    h.mock.push(Reply::Text("ok".into()));
    let s2 = chat_with(&h, &forge).await;
    h.send(&s2.id, "hi").await;
    h.settle(&s2.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    assert!(!tool_names(&req).contains(&"message_bot".to_string()));
}

#[tokio::test]
async fn channels_route_mentions_and_hand_offs() {
    let h = harness(vec![], 100_000).await;
    let nova = save(
        &h,
        bot("Nova", "personal assistant", ToolPreset::Assistant, &[]),
    )
    .await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let scout = save(&h, bot("Scout", "researcher", ToolPreset::Research, &[])).await;
    let room: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateChannel {
                    title: "War room".into(),
                    members: vec!["@nova".into(), forge.id.clone(), "scout".into()],
                    cwd: Some(h.work.display().to_string()),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(room.kind, SessionKind::Channel);
    assert_eq!(room.members.len(), 3);
    // User mentions Nova only; Nova hands off to @scout.
    h.mock.push(Reply::Text("Let me get @scout on it.".into()));
    h.mock.push(Reply::Text("Found three sources.".into()));
    h.send(&room.id, "@nova can you research flights?").await;
    h.settle(&room.id).await;
    let entries = h.entries(&room.id).await;
    let authors: Vec<String> = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { author, .. } => author.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        authors,
        vec![nova.id.clone(), scout.id.clone()],
        "mentioned bot, then the hand-off; forge stays quiet"
    );
    let scout_req = h.mock.agent_requests().pop().unwrap();
    let text = scout_req.to_string();
    assert!(
        scout_req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("You are Scout")
    );
    assert!(
        text.contains("[user]: @nova can you research flights?")
            && text.contains("[@nova]: Let me get @scout on it."),
        "{text}"
    );
    assert!(
        scout_req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("channel \\\"War room\\\"")
            || scout_req["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("War room")
    );
    let _ = scout;

    // No mention: every member answers in order.
    for t in ["n", "f", "s"] {
        h.mock.push(Reply::Text(format!("reply {t}")));
    }
    h.send(&room.id, "morning all").await;
    h.settle(&room.id).await;
    let entries = h.entries(&room.id).await;
    let n = entries
        .iter()
        .filter(|e| matches!(e.body, EntryBody::Assistant { .. }))
        .count();
    assert_eq!(n, 5);
    assert_eq!(h.info(&room.id).await.status, SessionStatus::Idle);
}

#[tokio::test]
async fn bot_duties_run_as_the_bot() {
    let h = harness(vec![Reply::Text("Here is your briefing.".into())], 100_000).await;
    let nova = save(
        &h,
        bot("Nova", "personal assistant", ToolPreset::Assistant, &[]),
    )
    .await;
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Morning briefing".into(),
                        schedule: "0 8 * * *".into(),
                        prompt: "Brief me on today.".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: Some(nova.id.clone()),
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
    let info: SessionInfo = serde_json::from_value(
        h.engine
            .handle(Request::RunJob { id: job.id }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.bot_id.as_deref(), Some(nova.id.as_str()));
    h.settle(&info.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    let system = req["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("You are Nova") && system.contains("Scheduled run"));
}

#[tokio::test]
async fn yolo_is_an_alias_for_auto() {
    let m: PermissionMode = serde_json::from_str("\"yolo\"").unwrap();
    assert_eq!(m, PermissionMode::Auto);
}

/// A refused `recall` says which rule applied, so the agent can tell the
/// user the way out (it used to say "You can't read that conversation."
/// for a private chat, a missing one and a bot's separation alike).
#[tokio::test]
async fn recall_refusals_say_which_rule_applied() {
    let h = harness(vec![], 100_000).await;
    let private: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
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
    h.mock.push(Reply::Tools(vec![
        ("recall".into(), json!({"session_id": private.id, "seq": 1})),
        (
            "recall".into(),
            json!({"session_id": "01nosuchchat0000000000", "seq": 1}),
        ),
    ]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    h.send(&s.id, "look back").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("That conversation is private"),
        "{results:?}"
    );
    assert!(
        results[1].1 && results[1].2.contains("There is no conversation"),
        "{results:?}"
    );
}

#[tokio::test]
async fn bots_cannot_recall_each_others_conversations() {
    let h = harness(vec![], 100_000).await;
    let nova = save(
        &h,
        bot("Nova", "personal assistant", ToolPreset::Assistant, &[]),
    )
    .await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    // Nova hears a secret.
    h.mock.push(Reply::Text("ok".into()));
    let s1 = chat_with(&h, &nova).await;
    h.send(&s1.id, "my bank pin is zebra-crossing").await;
    h.settle(&s1.id).await;
    // Forge tries to find it, globally and by session id.
    h.mock.push(Reply::Tools(vec![
        (
            "recall".into(),
            json!({"query": "zebra crossing", "scope": "all"}),
        ),
        ("recall".into(), json!({"session_id": s1.id, "seq": 1})),
    ]));
    h.mock.push(Reply::Text("nothing".into()));
    let s2 = chat_with(&h, &forge).await;
    h.send(&s2.id, "search history").await;
    h.settle(&s2.id).await;
    let results = tool_results(&h.entries(&s2.id).await);
    assert!(
        !results[0].2.contains(&format!("· {}]", s1.id)) && !results[0].2.contains("bank pin"),
        "only Forge's own sessions are searchable: {results:?}"
    );
    assert!(
        results[1].1
            && results[1]
                .2
                .contains("You act as Forge, a bot with its own memory"),
        "the refusal names the rule: {results:?}"
    );
    assert!(
        !h.mock
            .agent_requests()
            .last()
            .unwrap()
            .to_string()
            .contains("bank pin"),
        "the secret never reaches Forge"
    );
}

#[tokio::test]
async fn subagents_and_bot_messages_stay_within_the_sessions_permission() {
    let h = harness(vec![], 100_000).await;
    let mut ops = bot("Ops", "operations", ToolPreset::Coding, &[]);
    ops.permission = PermissionMode::Auto;
    let ops = save(&h, ops).await;
    // The user turned this chat with an auto bot down to read-only.
    let s = chat_with(&h, &ops).await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
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
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"tasks": [{"name": "a", "prompt": "run things", "readonly": false}]}),
    )]));
    h.mock.push(Reply::Text("sub done".into()));
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert!(
        !tool_names(&reqs[1]).contains(&"bash".to_string()),
        "a subagent of a read-only session got {:?}",
        tool_names(&reqs[1])
    );

    // A plain default-permission session asking the auto bot needs approval.
    let plain: SessionInfo = serde_json::from_value(
        h.engine
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
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@ops", "message": "delete the backups", "wait": true}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let mut events = h.engine.subscribe();
    h.send(&plain.id, "ask ops").await;
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("approval requested");
    assert_eq!(request.tool, "message_bot");
    assert!(request.reason.contains("auto"), "{}", request.reason);
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: plain.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Deny,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&plain.id).await;
    let results = tool_results(&h.entries(&plain.id).await);
    assert!(
        results[0].1 && results[0].2.contains("declined"),
        "{results:?}"
    );
}

#[tokio::test]
async fn bots_cannot_recall_each_others_artifacts() {
    let h = harness(vec![], 100_000).await;
    let nova = save(&h, bot("Nova", "assistant", ToolPreset::Coding, &[])).await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "seq 1 30000"}),
    )]));
    h.mock.push(Reply::Text("long".into()));
    let a = chat_with(&h, &nova).await;
    h.send(&a.id, "count").await;
    h.settle(&a.id).await;
    let out = tool_results(&h.entries(&a.id).await);
    let artifact = out[0]
        .2
        .split("artifact `")
        .nth(1)
        .and_then(|r| r.split('`').next())
        .expect("artifact id")
        .to_string();
    for (who, readable) in [(&nova, true), (&forge, false)] {
        h.mock.push(Reply::Tools(vec![(
            "recall".into(),
            json!({"artifact": artifact, "offset": 0}),
        )]));
        h.mock.push(Reply::Text("ok".into()));
        let s = chat_with(&h, who).await;
        h.send(&s.id, "look").await;
        h.settle(&s.id).await;
        let r = tool_results(&h.entries(&s.id).await);
        assert_eq!(
            !r[0].1 && r[0].2.contains("seq 1 30000]"),
            readable,
            "{}",
            r[0].2.chars().take(200).collect::<String>()
        );
    }
}

async fn set_handoff(h: &Harness, mode: BotHandoff) {
    h.engine
        .handle(
            Request::SetSecurity {
                sandbox: None,
                folders: None,
                ask_outside: None,
                redact_patterns: None,
                save_secrets_from_chat: None,
                secret_saving: None,
                secret_machines: None,
                secret_machines_picked: None,
                bot_handoff: Some(mode),
                default_permission: None,
            },
            None,
        )
        .await
        .unwrap();
}

async fn plain_chat(h: &Harness, permission: PermissionMode) -> SessionInfo {
    let s: SessionInfo = serde_json::from_value(
        h.engine
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
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
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
    s
}

/// Sends `text`, answers every approval with `decision`, waits for the
/// turn to end; the approvals asked.
async fn send_answering(
    h: &Harness,
    id: &str,
    text: &str,
    decision: ApprovalDecision,
) -> Vec<ApprovalRequest> {
    let mut events = h.engine.subscribe();
    h.send(id, text).await;
    let mut asked = Vec::new();
    let mut started = false;
    tokio::time::timeout(Duration::from_secs(30), async {
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
                                decision,
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
    .await
    .expect("the turn ended");
    h.settle(id).await;
    asked
}

/// Whether ordinary chats may hand work to bots (On by default, Ask me
/// first, Off), and a bot that takes no work from others: enforced by the
/// server, whatever the model tries.
#[tokio::test]
async fn chats_hand_work_to_bots_only_as_the_user_allows() {
    let h = harness(vec![], 100_000).await;
    // This test exercises deliberate YOLO hand-offs, independently of the
    // kernel's Automatic fallback (covered separately).
    h.engine
        .handle(
            Request::SetSecurity {
                sandbox: Some(SandboxMode::Off),
                folders: None,
                ask_outside: None,
                redact_patterns: None,
                save_secrets_from_chat: None,
                secret_saving: None,
                secret_machines: None,
                secret_machines_picked: None,
                bot_handoff: None,
                default_permission: None,
            },
            None,
        )
        .await
        .unwrap();
    let nova = save(
        &h,
        bot("Nova", "assistant", ToolPreset::Assistant, &["forge"]),
    )
    .await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let ask_forge = || {
        Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot": "@forge", "message": "Please check the build.", "wait": true}),
        )])
    };
    let dms = |h: &Harness| {
        let h = h.engine.clone();
        async move {
            let l: Vec<SessionInfo> = serde_json::from_value(
                h.handle(Request::ListSessions { archived: false }, None)
                    .await
                    .unwrap(),
            )
            .unwrap();
            l.into_iter()
                .filter(|s| s.kind == SessionKind::Channel)
                .count()
        }
    };

    // On (the default): a chat hands work over without asking.
    let s = plain_chat(&h, PermissionMode::Default).await;
    h.mock.push(ask_forge());
    h.mock.push(Reply::Text("Build is green.".into()));
    h.mock.push(Reply::Text("done".into()));
    let asked = send_answering(&h, &s.id, "HANDOFF-ON", ApprovalDecision::Deny).await;
    assert!(asked.is_empty(), "{asked:?}");
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(!r.1 && r.2.contains("@forge replied"), "{r:?}");

    // Ask me first: asked even in a chat that runs without asking.
    set_handoff(&h, BotHandoff::Ask).await;
    let s = plain_chat(&h, PermissionMode::Auto).await;
    h.mock.push(ask_forge());
    h.mock.push(Reply::Text("done".into()));
    let asked = send_answering(&h, &s.id, "HANDOFF-ASK-NO", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].tool, "message_bot");
    assert!(asked[0].reason.contains("each time"), "{}", asked[0].reason);
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        r.1 && r.2.contains("declined this message_bot action"),
        "{r:?}"
    );
    let before = dms(&h).await;
    h.mock.push(ask_forge());
    h.mock.push(Reply::Text("Still green.".into()));
    h.mock.push(Reply::Text("done".into()));
    let asked = send_answering(&h, &s.id, "HANDOFF-ASK-YES", ApprovalDecision::Allow).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    let r = tool_results(&h.entries(&s.id).await).pop().unwrap();
    assert!(!r.1 && r.2.contains("@forge replied"), "{r:?}");
    assert!(asked[0].once, "each hand-off is one-time only");
    assert_eq!(asked[0].arguments["bot"], "@forge");
    assert_eq!(asked[0].arguments["message"], "Please check the build.");
    assert!(asked[0].summary.contains("Please check the build."));
    assert_eq!(dms(&h).await, before, "the same conversation with Forge");
    // A stale session grant from before Ask was selected must not cover a
    // later message, even if the client sends AllowSession for a once card.
    for (label, message) in [
        ("HANDOFF-ASK-SESSION", "Please check the build."),
        ("HANDOFF-ASK-AGAIN", "Please check the tests next."),
    ] {
        h.mock.push(Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot":"@forge", "message":message, "wait":true}),
        )]));
        h.mock.push(Reply::Text("Green.".into()));
        h.mock.push(Reply::Text("done".into()));
        let asked = send_answering(&h, &s.id, label, ApprovalDecision::AllowSession).await;
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].once);
        assert_eq!(asked[0].arguments["message"], message);
        assert!(asked[0].summary.contains(message));
    }

    // Off: chats aren't offered the tool or told about the bots, and a
    // call anyway reaches nobody. Bots still reach the bots they may.
    set_handoff(&h, BotHandoff::Off).await;
    let before = dms(&h).await;
    let s = plain_chat(&h, PermissionMode::Auto).await;
    h.mock.push(ask_forge());
    h.mock.push(Reply::Text("done".into()));
    let asked = send_answering(&h, &s.id, "HANDOFF-OFF", ApprovalDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
    let req = h
        .mock
        .agent_requests()
        .into_iter()
        .find(|r| r.to_string().contains("HANDOFF-OFF"))
        .unwrap();
    assert!(!tool_names(&req).contains(&"message_bot".to_string()));
    assert!(
        !req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("# Other bots")
    );
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(r.1, "{r:?}");
    assert_eq!(dms(&h).await, before, "nothing reached a bot");
    let n = chat_with(&h, &nova).await;
    h.mock.push(ask_forge());
    h.mock.push(Reply::Text("Green.".into()));
    h.mock.push(Reply::Text("done".into()));
    send_answering(&h, &n.id, "HANDOFF-OFF-BOT", ApprovalDecision::Allow).await;
    let r = tool_results(&h.entries(&n.id).await).remove(0);
    assert!(!r.1 && r.2.contains("@forge replied"), "{r:?}");

    // A bot that takes work only from the user: refused for chats and bots.
    set_handoff(&h, BotHandoff::On).await;
    let mut alone = forge.clone();
    alone.askable = Some(false);
    let alone = save(&h, alone).await;
    assert_eq!(alone.askable, Some(false));
    // An app that doesn't know the switch keeps it.
    let mut old_app = alone.clone();
    old_app.askable = None;
    old_app.role = "dev bot, builds".into();
    assert_eq!(save(&h, old_app).await.askable, Some(false));
    let before = dms(&h).await;
    for (s, label) in [
        (plain_chat(&h, PermissionMode::Auto).await, "ALONE-CHAT"),
        (chat_with(&h, &nova).await, "ALONE-BOT"),
    ] {
        h.mock.push(ask_forge());
        h.mock.push(Reply::Text("done".into()));
        send_answering(&h, &s.id, label, ApprovalDecision::Allow).await;
        let r = tool_results(&h.entries(&s.id).await).remove(0);
        assert!(r.1, "{label}: {r:?}");
        let req = h
            .mock
            .agent_requests()
            .into_iter()
            .find(|r| r.to_string().contains(label))
            .unwrap();
        assert!(
            !req["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("@forge ("),
            "{label}: Forge isn't listed"
        );
    }
    assert_eq!(dms(&h).await, before, "nothing reached Forge");
}

/// Wait until `f` holds for the session's entries (or fail after 20 s).
async fn until_entries(
    h: &Harness,
    id: &str,
    what: &str,
    f: impl Fn(&[Entry]) -> bool,
) -> Vec<Entry> {
    for _ in 0..200 {
        let es = h.entries(id).await;
        if f(&es) {
            return es;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what}: never happened");
}

/// The text of the user entry `author` wrote in `entries`, if any.
fn authored(entries: &[Entry], author: &str) -> Option<String> {
    entries.iter().rev().find_map(|e| match &e.body {
        EntryBody::User {
            content,
            author: Some(a),
        } if a == author => Some(plain_text(content)),
        _ => None,
    })
}

/// "Let the agent keep working while it messages bots" (batch 17): the
/// hand-off returns at once, the bot works in its own conversation (which
/// says whom it works for), its reply comes back into the chat as a message
/// from the bot, and the chat carries on with it by itself.
#[tokio::test]
async fn a_hand_off_returns_at_once_and_the_reply_comes_back_as_a_message() {
    let h = harness(vec![], 100_000).await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let s = plain_chat(&h, PermissionMode::Default).await;
    // The chat's turns (routed by its first message) and Forge's (by its).
    h.mock.route(
        "is main green",
        Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot": "@forge", "message": "FORGE check whether main is green."}),
        )]),
    );
    h.mock.route(
        "is main green",
        Reply::Text("I asked Forge; I'll pass its answer on.".into()),
    );
    h.mock.route(
        "FORGE check",
        Reply::Slow(
            Duration::from_millis(1500),
            "Main is green: 21 passed.".into(),
        ),
    );
    h.mock.route(
        "is main green",
        Reply::Text("Forge says main is green.".into()),
    );
    h.send(&s.id, "is main green?").await;
    h.settle(&s.id).await;
    let es = h.entries(&s.id).await;
    let results = tool_results(&es);
    assert!(
        !results[0].1 && results[0].2.starts_with("Sent to @forge"),
        "{results:?}"
    );
    let details = es
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult { details, .. } => details.clone(),
            _ => None,
        })
        .expect("details");
    assert_eq!(details["background"], json!(true), "{details}");
    let dm = details["session_id"].as_str().unwrap().to_string();
    // The chat is free while Forge works, and Forge's conversation says for whom.
    let info = h.info(&dm).await;
    let wf = info.working_for.expect("working_for while it works");
    assert_eq!(wf.session_id, s.id);
    assert_eq!(info.status, SessionStatus::Running);
    assert_eq!(h.info(&s.id).await.status, SessionStatus::Idle);
    // The reply arrives as Forge's message, with its header; the chat goes on.
    let es = until_entries(&h, &s.id, "the chat's follow-up", |es| {
        authored(es, &forge.id).is_some()
            && matches!(
                es.last().map(|e| &e.body),
                Some(EntryBody::Assistant { .. })
            )
    })
    .await;
    let reply = authored(&es, &forge.id).unwrap();
    assert!(
        reply.starts_with("[Reply from @forge · ")
            && reply.contains(&format!("· session {dm}]\n\n"))
            && reply.ends_with("Main is green: 21 passed."),
        "{reply}"
    );
    h.settle(&s.id).await;
    let last = h.mock.agent_requests().pop().unwrap().to_string();
    assert!(
        last.contains("[@forge]: [Reply from @forge"),
        "the model reads it as Forge's message: {last}"
    );
    assert!(h.info(&dm).await.working_for.is_none());
}

/// Stopping the chat stops the bots working for it; a bot stopped that way
/// sends back that it was stopped, and the chat waits for the user.
#[tokio::test]
async fn stopping_a_chat_stops_the_bot_working_for_it() {
    let h = harness(vec![], 100_000).await;
    let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let s = plain_chat(&h, PermissionMode::Default).await;
    h.mock.route(
        "run the long build",
        Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot": "@forge", "message": "FORGE run the long build."}),
        )]),
    );
    h.mock
        .route("run the long build", Reply::Text("Handed to Forge.".into()));
    h.mock.route(
        "FORGE run",
        Reply::Slow(Duration::from_secs(30), "never".into()),
    );
    h.send(&s.id, "run the long build").await;
    h.settle(&s.id).await;
    let dm = until_entries(&h, &s.id, "the hand-off", |es| !tool_results(es).is_empty())
        .await
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult { details, .. } => details.clone(),
            _ => None,
        })
        .and_then(|d| d["session_id"].as_str().map(str::to_string))
        .unwrap();
    for _ in 0..50 {
        if h.info(&dm).await.status == SessionStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let before = h.mock.agent_requests().len();
    h.engine
        .handle(
            Request::Abort {
                session_id: s.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let es = until_entries(&h, &s.id, "the stopped reply", |es| {
        authored(es, &forge.id).is_some()
    })
    .await;
    let reply = authored(&es, &forge.id).unwrap();
    assert!(
        reply.contains("@forge did not finish: This attempt was stopped before it finished"),
        "{reply}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = h.entries(&s.id).await;
    assert!(
        matches!(after.last().map(|e| &e.body), Some(EntryBody::User { .. })),
        "the chat waits for the user instead of carrying on"
    );
    assert_eq!(h.mock.agent_requests().len(), before, "no new turn");
    assert_ne!(h.info(&dm).await.status, SessionStatus::Running);
}

/// A bot still working on this chat's hand-off refuses the next one, saying
/// its reply is on the way (not "try again shortly", which invites polling).
#[tokio::test]
async fn a_second_hand_off_to_a_busy_bot_says_its_reply_is_coming() {
    let h = harness(vec![], 100_000).await;
    save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let s = plain_chat(&h, PermissionMode::Default).await;
    let call = |m: &str| {
        Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot": "@forge", "message": m}),
        )])
    };
    h.mock.route("two jobs", call("FORGE first job"));
    h.mock.route("two jobs", call("FORGE second job"));
    h.mock.route("two jobs", Reply::Text("ok".into()));
    h.mock.route(
        "FORGE first",
        Reply::Slow(Duration::from_secs(3), "first done".into()),
    );
    h.send(&s.id, "two jobs").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].2.starts_with("Sent to @forge"), "{results:?}");
    assert!(
        results[1].1
            && results[1]
                .2
                .contains("still working on what this chat handed it earlier"),
        "{results:?}"
    );
}

#[tokio::test]
async fn native_rooms_and_bot_presets_offer_only_available_helpers() {
    let h = harness(vec![], 100_000).await;
    let b = save(&h, bot("Fixture", "test bot", ToolPreset::Full, &[])).await;
    let room: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateChannel {
                    title: "Fixture room".into(),
                    members: vec![b.id.clone()],
                    cwd: Some(h.work.display().to_string()),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    for enabled in [true, false, true] {
        h.engine
            .handle(
                Request::UpdateSession {
                    session_id: room.id.clone(),
                    title: None,
                    model: None,
                    thinking: None,
                    permission: None,
                    pinned: None,
                    archived: None,
                    cwd: None,
                    members: None,
                    fast: None,
                    subagents: Some(enabled),
                },
                None,
            )
            .await
            .unwrap();
        h.mock.push(Reply::Text("done".into()));
        h.send(&room.id, "inspect fixture offerings").await;
        h.settle(&room.id).await;
        let req = h.mock.agent_requests().pop().unwrap();
        for name in ["agent", "agents"] {
            assert_eq!(
                req["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["function"]["name"] == name),
                enabled
            );
        }
        assert!(
            req["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains(if enabled {
                    "Helpers are on"
                } else {
                    "Helpers are off"
                })
        );
    }
    // Updating a bot's preset also invalidates the room's cached prompt.
    save(
        &h,
        Bot {
            tools: ToolPreset::Assistant,
            ..b
        },
    )
    .await;
    h.mock.push(Reply::Text("done".into()));
    h.send(&room.id, "inspect restricted bot").await;
    h.settle(&room.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    assert!(
        !req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "agent")
    );
    assert!(
        req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Helpers are off")
    );
}

#[tokio::test]
async fn failed_bot_handoffs_label_partial_notes_instead_of_returning_a_normal_reply() {
    const NOTICE: &str = "The model did not finish its reply successfully. Partial text was kept. Send a message to try again.";
    for wait in [true, false] {
        for partial in ["", "Useful but unfinished bot notes."] {
            let h = harness(vec![], 100_000).await;
            let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
            let s = plain_chat(&h, PermissionMode::Default).await;
            // The DM's author label may include the caller's chat title.
            // Prefer the recipient's payload over that enclosing caller label.
            h.mock.route(
                "BOT failure fixture",
                Reply::Raw(vec![json!({"choices":[{
                    "index":0,"delta":{"content":partial},"finish_reason":"content_filter", "error":{"message":"provider-error-sentinel-never-display-97a1"}
                }]})]),
            );
            h.mock.route(
                "CALLER failure fixture",
                Reply::Tools(vec![(
                    "message_bot".into(),
                    json!({"bot":"@forge","message":"BOT failure fixture","wait":wait}),
                )]),
            );
            for _ in 0..3 {
                h.mock.route(
                    "CALLER failure fixture",
                    Reply::Text("Caller saw the bot result.".into()),
                );
            }
            h.send(&s.id, "CALLER failure fixture").await;
            h.settle(&s.id).await;
            let entries = if wait {
                h.entries(&s.id).await
            } else {
                until_entries(&h, &s.id, "the failed bot handoff", |es| {
                    authored(es, &forge.id).is_some()
                })
                .await
            };
            let result = if wait {
                let result = tool_results(&entries).remove(0);
                assert!(result.1, "{result:?}");
                assert!(!result.2.contains("@forge replied:"), "{result:?}");
                result.2
            } else {
                let result = authored(&entries, &forge.id).unwrap();
                assert!(result.contains("@forge did not finish"), "{result}");
                result
            };
            assert!(result.contains(NOTICE), "{result}");
            if !partial.is_empty() {
                assert!(
                    result.contains("Partial notes, not a final reply or report:"),
                    "{result}"
                );
                assert!(result.contains(partial), "{result}");
            }
            let sessions: Vec<SessionInfo> = serde_json::from_value(
                h.engine
                    .handle(Request::ListSessions { archived: false }, None)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let dm = sessions
                .iter()
                .find(|s| s.kind == SessionKind::Channel)
                .unwrap();
            assert_eq!(dm.status, SessionStatus::Error);
            h.send(&s.id, "sentinel follow-up fixture").await;
            h.settle(&s.id).await;
            let outgoing = h.mock.agent_requests();
            let first_consumer = outgoing
                .iter()
                .find(|r| {
                    r["messages"].as_array().is_some_and(|ms| {
                        ms.iter().any(|m| {
                            m["content"].to_string().contains(NOTICE)
                                && (m["role"] == "tool"
                                    || m["content"].to_string().contains("[Reply from @forge"))
                        })
                    })
                })
                .expect("the first parent request must consume the delivered bot result");
            assert!(
                !first_consumer
                    .to_string()
                    .contains("sentinel follow-up fixture"),
                "assert the first consumer, not the later explicit follow-up"
            );
            assert!(
                !first_consumer
                    .to_string()
                    .contains("provider-error-sentinel-never-display-97a1")
            );
            assert!(first_consumer.to_string().contains(NOTICE));
            assert!(
                outgoing
                    .iter()
                    .any(|r| r.to_string().contains("sentinel follow-up fixture")
                        && r.to_string().contains(NOTICE)),
                "parent must receive labelled bot result in its follow-up"
            );
            assert!(
                !serde_json::to_string(&outgoing)
                    .unwrap()
                    .contains("provider-error-sentinel-never-display-97a1")
            );
            let requests: Vec<_> = h
                .mock
                .agent_requests()
                .into_iter()
                .filter(|r| {
                    r["messages"].as_array().is_some_and(|messages| {
                        messages
                            .iter()
                            .find(|m| m["role"] == "user")
                            .is_some_and(|m| {
                                m["content"].to_string().contains("BOT failure fixture")
                            })
                    })
                })
                .collect();
            assert_eq!(requests.len(), 1);
            h.engine.shutdown().await;
        }
    }
}

#[tokio::test]
async fn stopped_sync_bot_handoff_returns_labelled_partial_notes_not_a_normal_reply() {
    let h = harness(vec![], 100_000).await;
    save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
    let s = plain_chat(&h, PermissionMode::Default).await;
    h.mock.route(
        "BOT stopped fixture",
        Reply::Hang("Unfinished bot notes.".into()),
    );
    h.mock.route(
        "CALLER stopped fixture",
        Reply::Tools(vec![(
            "message_bot".into(),
            json!({"bot":"@forge","message":"BOT stopped fixture","wait":true}),
        )]),
    );
    h.mock.route(
        "CALLER stopped fixture",
        Reply::Text("Stopped result received.".into()),
    );
    let mut events = h.engine.subscribe();
    h.send(&s.id, "CALLER stopped fixture").await;
    let dm = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Event::StreamDelta {
                session_id,
                delta: StreamDelta::Text { text, .. },
            } = events.recv().await.unwrap()
                && session_id != s.id
                && text.contains("Unfinished bot notes.")
            {
                break session_id;
            }
        }
    })
    .await
    .unwrap();
    h.engine
        .handle(Request::Abort { session_id: dm }, None)
        .await
        .unwrap();
    h.settle(&s.id).await;
    let result = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(result.1, "{result:?}");
    assert!(
        result.2.contains("stopped before it finished"),
        "{result:?}"
    );
    assert!(
        result
            .2
            .contains("Partial notes, not a final reply or report:"),
        "{result:?}"
    );
    assert!(result.2.contains("Unfinished bot notes."), "{result:?}");
    assert!(!result.2.contains("@forge replied:"), "{result:?}");
    h.engine.shutdown().await;
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn late_bot_admission_captures_the_final_attempt_not_the_first_success() {
    for wait in [true, false] {
        for stopped in [true, false] {
            let h = harness(vec![], 100_000).await;
            let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
            let s = plain_chat(&h, PermissionMode::Default).await;
            let release = std::sync::Arc::new(tokio::sync::Notify::new());
            h.mock.route(
                "BOT late fixture",
                Reply::Gate("First successful bot report.".into(), release.clone()),
            );
            h.mock.route("BOT late fixture",if stopped {Reply::Hang("Latest stopped bot notes.".into())} else {Reply::Raw(vec![json!({"choices":[{"index":0,"delta":{"content":"Latest failed bot notes."},"finish_reason":"content_filter"}]})])});
            h.mock.route(
                "CALLER late fixture",
                Reply::Tools(vec![(
                    "message_bot".into(),
                    json!({"bot":"@forge","message":"BOT late fixture","wait":wait}),
                )]),
            );
            for _ in 0..3 {
                h.mock.route(
                    "CALLER late fixture",
                    Reply::Text("Caller consumed a result.".into()),
                );
            }
            let mut events = h.engine.subscribe();
            h.send(&s.id, "CALLER late fixture").await;
            let dm = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Event::StreamDelta {
                        session_id,
                        delta: StreamDelta::Text { text, .. },
                    } = events.recv().await.unwrap()
                        && session_id != s.id
                        && text.contains("First successful bot report.")
                    {
                        break session_id;
                    }
                }
            })
            .await
            .unwrap();
            if !wait {
                h.settle(&s.id).await;
            }
            let before = h.mock.agent_requests().len();
            let admission = h.engine.hold_final_admission_for_test(&dm);
            release.notify_one();
            tokio::time::timeout(Duration::from_secs(10), admission.reached())
                .await
                .unwrap();
            let (_, pending) = h.engine.store().get_session(&dm).unwrap().unwrap();
            assert_eq!(pending.attempts.len(), 1);
            assert_eq!(
                pending.attempts[0].outcome, None,
                "Done is not committed before final admission"
            );
            h.engine
                .handle(
                    Request::Send {
                        session_id: dm.clone(),
                        content: vec![ContentPart::text("Late admitted continuation.")],
                        delivery: Delivery::FollowUp,
                        attachments: vec![],
                    },
                    None,
                )
                .await
                .unwrap();
            admission.release();
            if stopped {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let Event::StreamDelta {
                            session_id,
                            delta: StreamDelta::Text { text, .. },
                        } = events.recv().await.unwrap()
                            && session_id == dm
                            && text.contains("Latest stopped bot notes.")
                        {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                h.engine
                    .handle(
                        Request::Abort {
                            session_id: dm.clone(),
                        },
                        None,
                    )
                    .await
                    .unwrap();
            }
            h.settle(&dm).await;
            let (_, persisted) = h.engine.store().get_session(&dm).unwrap().unwrap();
            assert_eq!(
                persisted.attempts.len(),
                2,
                "the continuation must be a distinct run, not folded into the first loop"
            );
            assert_ne!(persisted.attempts[0].id, persisted.attempts[1].id);
            assert_eq!(
                persisted.attempts[0].outcome,
                Some(tessera_core::store::AttemptOutcome::Completed)
            );
            assert_eq!(
                persisted.attempts[1].outcome,
                Some(if stopped {
                    tessera_core::store::AttemptOutcome::Stopped
                } else {
                    tessera_core::store::AttemptOutcome::Failed
                })
            );
            let entries = if wait {
                h.settle(&s.id).await;
                h.entries(&s.id).await
            } else {
                until_entries(&h, &s.id, "the final late-admission handoff", |e| {
                    authored(e, &forge.id).is_some()
                })
                .await
            };
            let result = if wait {
                let r = tool_results(&entries).remove(0);
                assert!(r.1, "{r:?}");
                assert!(!r.2.contains("@forge replied:"), "{r:?}");
                r.2
            } else {
                authored(&entries, &forge.id).unwrap()
            };
            let current = result.split("## Earlier").next().unwrap();
            assert!(
                !current.contains("First successful bot report."),
                "{result}"
            );
            assert!(result.contains("Earlier report (historical)"), "{result}");
            if stopped {
                assert!(current.contains("stopped before it finished"), "{result}");
                assert!(current.contains("Latest stopped bot notes."), "{result}");
                if !wait {
                    assert!(
                        matches!(
                            h.entries(&s.id).await.last().map(|e| &e.body),
                            Some(EntryBody::User { .. })
                        ),
                        "stopped recipient must not resume the parent"
                    );
                    assert_eq!(
                        h.mock.agent_requests().len(),
                        before + 1,
                        "only the second bot attempt runs, not a new parent turn"
                    );
                }
            } else {
                assert!(
                    current.contains("The model did not finish its reply successfully"),
                    "{result}"
                );
                assert!(current.contains("Latest failed bot notes."), "{result}");
                h.settle(&s.id).await;
            }
            h.engine.shutdown().await;
        }
    }
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn abort_after_done_before_final_admission_stops_bot_delivery_in_both_wait_modes() {
    use tessera_core::store::AttemptOutcome;
    for wait in [true, false] {
        let h = harness(vec![], 100_000).await;
        let forge = save(&h, bot("Forge", "dev bot", ToolPreset::Coding, &[])).await;
        let s = plain_chat(&h, PermissionMode::Default).await;
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        h.mock.route(
            "BOT abort-final fixture",
            Reply::Gate("Answer returned before late Abort.".into(), release.clone()),
        );
        h.mock.route(
            "CALLER abort-final fixture",
            Reply::Tools(vec![(
                "message_bot".into(),
                json!({"bot":"@forge","message":"BOT abort-final fixture","wait":wait}),
            )]),
        );
        for _ in 0..2 {
            h.mock.route(
                "CALLER abort-final fixture",
                Reply::Text("Caller received stopped notes.".into()),
            );
        }
        let mut events = h.engine.subscribe();
        h.send(&s.id, "CALLER abort-final fixture").await;
        let dm = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Event::StreamDelta {
                    session_id,
                    delta: StreamDelta::Text { text, .. },
                } = events.recv().await.unwrap()
                    && session_id != s.id
                    && text.contains("Answer returned before late Abort.")
                {
                    break session_id;
                }
            }
        })
        .await
        .unwrap();
        if !wait {
            h.settle(&s.id).await;
        }
        let requests = h.mock.agent_requests().len();
        let admission = h.engine.hold_final_admission_for_test(&dm);
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), admission.reached())
            .await
            .unwrap();
        let (_, pending) = h.engine.store().get_session(&dm).unwrap().unwrap();
        assert_eq!(pending.attempts.len(), 1);
        assert_eq!(
            pending.attempts[0].outcome, None,
            "the returned Done must still be pending"
        );
        h.engine
            .handle(
                Request::Abort {
                    session_id: dm.clone(),
                },
                None,
            )
            .await
            .unwrap();
        admission.release();
        h.settle(&dm).await;
        let entries = if wait {
            h.settle(&s.id).await;
            h.entries(&s.id).await
        } else {
            until_entries(&h, &s.id, "the stopped post-Done bot reply", |e| {
                authored(e, &forge.id).is_some()
            })
            .await
        };
        let (_, saved) = h.engine.store().get_session(&dm).unwrap().unwrap();
        assert_eq!(saved.attempts.len(), 1);
        assert_eq!(saved.attempts[0].outcome, Some(AttemptOutcome::Stopped));
        assert!(saved.attempts[0].to_seq.is_some());
        let text = if wait {
            let r = tool_results(&entries).remove(0);
            assert!(r.1, "{r:?}");
            r.2
        } else {
            authored(&entries, &forge.id).unwrap()
        };
        assert!(text.contains("stopped before it finished"), "{text}");
        assert!(
            text.contains("Partial notes, not a final reply or report:"),
            "{text}"
        );
        assert!(
            text.contains("Answer returned before late Abort."),
            "{text}"
        );
        assert!(!text.contains("replied:"), "{text}");
        if !wait {
            assert_eq!(
                h.mock.agent_requests().len(),
                requests,
                "Stopped bot must not resume its parent"
            );
            assert!(matches!(
                h.entries(&s.id).await.last().map(|e| &e.body),
                Some(EntryBody::User { .. })
            ));
        }
        h.engine.shutdown().await;
    }
}
