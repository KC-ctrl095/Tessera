//! Security settings and what agents may do: work anywhere the user lets
//! them (including on Tessera itself) without reaching Tessera's own data;
//! credentials typed into a chat end up in the secret store, not the chat.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::auth::{SecretPlace, SecretUser};
use tessera_proto::*;

/// Sends `text` and answers every approval with `decision`, until the run
/// ends. Returns the approvals asked and every event seen.
async fn run(
    h: &Harness,
    id: &str,
    text: &str,
    decision: ApprovalDecision,
) -> (Vec<ApprovalRequest>, Vec<Event>) {
    let mut events = h.engine.subscribe();
    h.send(id, text).await;
    let mut asked = Vec::new();
    let mut seen = Vec::new();
    // Idle again after running (earlier runs may still report in).
    let mut started = false;
    let done = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Ok(Event::ApprovalRequested { request, .. }) => {
                    asked.push(request.clone());
                    h.engine
                        .handle(
                            Request::RespondApproval {
                                session_id: id.into(),
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
                Ok(e) => seen.push(e),
                Err(e) => panic!("events: {e}"),
            }
        }
    })
    .await;
    assert!(done.is_ok(), "the run did not finish");
    h.settle(id).await;
    (asked, seen)
}

async fn set_security(h: &Harness, folders: Vec<FolderAccess>, ask_outside: bool) -> SecurityInfo {
    let v = h
        .engine
        .handle(
            Request::SetSecurity {
                sandbox: None,
                folders: Some(folders),
                ask_outside: Some(ask_outside),
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
    serde_json::from_value(v).unwrap()
}

async fn set_auto(h: &Harness, id: &str) {
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: id.into(),
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
}

fn bash(command: &str) -> Reply {
    Reply::Tools(vec![("bash".into(), json!({ "command": command }))])
}

/// "Allow for this session" covers what was asked about and no more: a
/// second risky part chained to an allowed one still asks (the question
/// shows the whole command and every reason), and in a channel one bot's
/// answer doesn't cover another bot.
#[tokio::test]
async fn allowing_for_the_session_covers_only_what_was_asked() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    h.mock.push(bash("echo sudo fixture"));
    h.mock.push(Reply::Text("ok".into()));
    let (asked, _) = run(&h, &s.id, "one", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    h.mock.push(bash("echo sudo fixture; echo again"));
    h.mock.push(Reply::Text("ok".into()));
    let (asked, _) = run(&h, &s.id, "two", ApprovalDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
    h.mock
        .push(bash("echo sudo fixture\n\necho git push origin main\n"));
    h.mock.push(Reply::Text("ok".into()));
    let (asked, _) = run(&h, &s.id, "three", ApprovalDecision::Allow).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(
        asked[0].summary,
        "echo sudo fixture\n\necho git push origin main"
    );
    assert!(
        asked[0].reason.contains("elevated privileges")
            && asked[0].reason.contains("pushes to a remote repository"),
        "{asked:?}"
    );

    let bot = |name: &str| -> Bot {
        serde_json::from_value(json!({
            "id": "", "name": name, "handle": "", "avatar": "", "color": "",
            "role": "dev", "personality": "", "instructions": "",
            "permission": "default", "tools": "coding"
        }))
        .unwrap()
    };
    let mut ids = Vec::new();
    for name in ["Ada", "Bo"] {
        let b: Bot = serde_json::from_value(
            h.engine
                .handle(Request::SaveBot { bot: bot(name) }, None)
                .await
                .unwrap(),
        )
        .unwrap();
        ids.push(b.id);
    }
    let room: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateChannel {
                    title: "Ops".into(),
                    members: ids.clone(),
                    cwd: Some(h.work.display().to_string()),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    h.mock.push(bash("echo sudo ls"));
    h.mock.push(Reply::Text("done".into()));
    let (asked, _) = run(&h, &room.id, "@ada check", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    h.mock.push(bash("echo sudo ls"));
    h.mock.push(Reply::Text("done".into()));
    let (asked, _) = run(&h, &room.id, "@bo check", ApprovalDecision::Allow).await;
    assert_eq!(asked.len(), 1, "Ada's answer covered Bo: {asked:?}");
    h.mock.push(bash("echo sudo ls"));
    h.mock.push(Reply::Text("done".into()));
    let (asked, _) = run(&h, &room.id, "@ada again", ApprovalDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
}

/// Files the user attached to one chat are that chat's (and its
/// subagents'): another chat's file tools can't open them, and with the
/// sandbox its commands don't see them either.
#[tokio::test]
async fn a_chat_reaches_only_its_own_attached_files() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let other = h.session().await;
    let uploads = tessera_core::config::Paths::at(h.dir.path()).uploads_dir();
    std::fs::create_dir_all(uploads.join(&s.id)).unwrap();
    std::fs::create_dir_all(uploads.join(&other.id)).unwrap();
    let mine = uploads.join(&s.id).join("mine.txt");
    let theirs = uploads.join(&other.id).join("theirs.txt");
    std::fs::write(&mine, "my notes").unwrap();
    std::fs::write(&theirs, "their notes").unwrap();
    set_auto(&h, &s.id).await;
    h.mock.push(Reply::Tools(vec![
        ("read".into(), json!({ "path": mine.display().to_string() })),
        ("read".into(), json!({ "path": theirs.display().to_string() })),
        (
            "bash".into(),
            json!({ "command": format!("cat '{}'; cat '{}' 2>&1; true", mine.display(), theirs.display()) }),
        ),
    ]));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "look").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("my notes"),
        "{results:?}"
    );
    assert!(
        results[1].1 && results[1].2.contains("another chat"),
        "{results:?}"
    );
    assert!(results[2].2.contains("my notes"), "{results:?}");
    if tessera_core::sandbox::support().complete() {
        assert!(!results[2].2.contains("their notes"), "{results:?}");
    }
}

/// Whatever a tool returns, the model reads only what a person would see:
/// invisible characters (tag characters, direction overrides) are taken
/// out, and the model is told.
#[tokio::test]
async fn hidden_characters_never_reach_the_model_through_a_tool() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let page = h.work.join("page.txt");
    std::fs::write(
        &page,
        "Welcome\u{E0049}\u{E0067}\u{E006E} to the \u{202E}docs\u{202C} page\n",
    )
    .unwrap();
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({ "path": page.display().to_string() }),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "read it").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let text = &results[0].2;
    assert!(text.contains("Welcome to the docs page"), "{text}");
    assert!(
        !text
            .chars()
            .any(|c| ('\u{E0000}'..='\u{E007F}').contains(&c) || c == '\u{202E}'),
        "{text:?}"
    );
    assert!(text.contains("removed 5 hidden characters"), "{text}");
}

fn folder(p: &std::path::Path, write: bool) -> FolderAccess {
    FolderAccess {
        path: p.display().to_string(),
        write,
    }
}

/// What an agent ran into when working on Tessera and around it: each of
/// these used to ask, be refused or fail.
#[tokio::test]
async fn agents_can_work_on_tessera_and_elsewhere_without_reaching_its_data() {
    let h = harness(vec![], 100_000).await;
    let home = h.dir.path().to_path_buf();
    let base = h.work.parent().unwrap().to_path_buf();
    let other = base.join("other");
    let checkout = base.join("src/tessera");
    let skills = home.join("skills/demo");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::create_dir_all(checkout.join("crates/cli/src/bin")).unwrap();
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: demo\ndescription: d\n---\nbody\n",
    )
    .unwrap();
    let toolchain = format!("{}-toolchain", home.display());
    std::fs::create_dir_all(&toolchain).unwrap();
    let w = h.work.display().to_string();
    let calls = |extra: Vec<(String, Value)>| {
        let mut v: Vec<(String, Value)> = vec![
            (
                "write".into(),
                json!({"path": format!("{w}/src/bin/main.rs"), "content": "fn main() {}"}),
            ),
            (
                "write".into(),
                json!({"path": format!("{w}/.github/ci.yml"), "content": "x"}),
            ),
            (
                "bash".into(),
                json!({"command": format!("cd '{}' && grep -rn TESSERA_HOME . ; echo grep-ok", checkout.display())}),
            ),
            (
                "bash".into(),
                json!({"command": format!("ls '{toolchain}' && echo toolchain-ok")}),
            ),
            (
                "bash".into(),
                json!({"command": "TESSERA_HOME=$PWD/tmp true && echo home-ok"}),
            ),
        ];
        v.extend(extra);
        v
    };
    // Whole-file writes must have seen the existing skill before approval.
    let s = h.session().await;
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"path":skills.join("SKILL.md")}),
    )]));
    h.mock.push(Reply::Text("Read the current skill.".into()));
    h.send(&s.id, "inspect the skill before editing").await;
    h.settle(&s.id).await;
    h.mock.push(Reply::Tools(calls(vec![
        ("write".into(), json!({"path": other.join("b.txt"), "content": "b"})),
        ("write".into(), json!({"path": skills.join("SKILL.md"), "content": "---\nname: demo\ndescription: d\n---\nnew\n"})),
        ("bash".into(), json!({"command": format!("echo more >> '{}/SKILL.md'", skills.display())})),
        ("bash".into(), json!({"command": format!("cat '{}/config.toml'", home.display())})),
        ("settings".into(), json!({"action": "set", "key": "security.sandbox", "value": "off"})),
    ])));
    let (asked, _) = run(&h, &s.id, "go", ApprovalDecision::Deny).await;
    let results: Vec<_> = tool_results(&h.entries(&s.id).await)
        .into_iter()
        .skip(1)
        .collect();
    // Writes and commands without unknown/recursive read targets just run.
    for (i, what) in [(0, ""), (1, ""), (4, "home-ok")] {
        assert!(
            !results[i].1 && results[i].2.contains(what),
            "{:?}",
            results[i]
        );
    }
    for i in [2, 3] {
        assert!(results[i].1 && results[i].2.contains("declined this bash action"));
    }
    // Writing elsewhere asks (by default), and so does changing a skill.
    let reasons: Vec<&str> = asked.iter().map(|a| a.reason.as_str()).collect();
    assert_eq!(asked.len(), 4, "{reasons:?}");
    assert!(
        reasons[..2].iter().all(|r| r.contains("read targets")),
        "{reasons:?}"
    );
    assert!(
        reasons[2].contains("outside the working directory"),
        "{reasons:?}"
    );
    assert!(reasons[3].contains("Tessera skill"), "{reasons:?}");
    // Commands see skills read-only, and never Tessera's settings.
    if tessera_core::sandbox::support().available {
        assert!(
            results[7].1 && results[7].2.contains("Read-only"),
            "{:?}",
            results[7]
        );
    }
    assert!(
        results[8].1 && results[8].2.contains("Refused"),
        "{:?}",
        results[8]
    );
    // Security settings are the user's alone.
    assert!(
        results[9].1 && results[9].2.contains("not a setting agents may change"),
        "{:?}",
        results[9]
    );
    assert!(checkout.exists() && !other.join("b.txt").exists());

    // The user opens a folder to the agent, makes another read-only.
    let docs = base.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("tax.txt"), "keep").unwrap();
    let info = set_security(&h, vec![folder(&other, true), folder(&docs, false)], true).await;
    assert_eq!(info.folders.len(), 2);
    h.mock.push(Reply::Tools(vec![
        (
            "write".into(),
            json!({"path": other.join("b.txt"), "content": "b"}),
        ),
        (
            "write".into(),
            json!({"path": docs.join("tax.txt"), "content": "changed"}),
        ),
        (
            "bash".into(),
            json!({"command": format!("echo changed > '{}/tax.txt'", docs.display())}),
        ),
        (
            "write".into(),
            json!({"path": checkout.join("x.rs"), "content": "x"}),
        ),
    ]));
    let s = h.session().await;
    let (asked, _) = run(&h, &s.id, "again", ApprovalDecision::Deny).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(!results[0].1, "{:?}", results[0]);
    assert!(
        results[1].1 && results[1].2.contains("read-only"),
        "{:?}",
        results[1]
    );
    if tessera_core::sandbox::support().available {
        assert!(results[2].1, "{:?}", results[2]);
    }
    assert_eq!(
        std::fs::read_to_string(docs.join("tax.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        asked.len(),
        1,
        "only the checkout outside every rule asks: {asked:?}"
    );

    // Without asking outside, writes anywhere else just happen.
    set_security(&h, vec![folder(&docs, false)], false).await;
    h.mock.push(Reply::Tools(vec![(
        "write".into(),
        json!({"path": checkout.join("x.rs"), "content": "x"}),
    )]));
    let s = h.session().await;
    let (asked, _) = run(&h, &s.id, "once more", ApprovalDecision::Deny).await;
    assert!(asked.is_empty(), "{asked:?}");
    assert!(checkout.join("x.rs").exists());
}

#[tokio::test]
async fn security_settings_are_checked_and_saved() {
    let h = harness(vec![], 100_000).await;
    let v = h.engine.handle(Request::GetSecurity, None).await.unwrap();
    let info: SecurityInfo = serde_json::from_value(v).unwrap();
    // Secure defaults.
    assert_eq!(info.sandbox, SandboxMode::Auto);
    assert!(
        info.ask_outside
            && info.redact_patterns
            && info.require_e2ee
            && info.save_secrets_from_chat
    );
    assert!(info.folders.is_empty());
    assert!(
        info.protected.contains(&h.dir.path().display().to_string()),
        "{info:?}"
    );
    assert_eq!(
        info.sandbox_complete,
        tessera_core::sandbox::support().complete()
    );
    assert_eq!(info.sandbox_hint.is_some(), !info.sandbox_complete);

    let bad = |path: String| {
        let h = &h;
        async move {
            h.engine
                .handle(
                    Request::SetSecurity {
                        sandbox: None,
                        folders: Some(vec![FolderAccess { path, write: true }]),
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
                .unwrap_err()
                .message
        }
    };
    assert!(bad("relative/dir".into()).await.contains("full path"));
    assert!(bad("/no/such/folder".into()).await.contains("no folder"));
    assert!(
        bad(h.dir.path().join("skills").display().to_string())
            .await
            .contains("Tessera's own")
    );
    assert!(bad("/".into()).await.contains("whole disk"));

    let v = h
        .engine
        .handle(
            Request::SetSecurity {
                sandbox: Some(SandboxMode::Strict),
                folders: Some(vec![folder(&h.work, false), folder(&h.work, true)]),
                ask_outside: Some(false),
                redact_patterns: None,
                save_secrets_from_chat: Some(false),
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
    let info: SecurityInfo = serde_json::from_value(v).unwrap();
    assert_eq!(info.sandbox, SandboxMode::Strict);
    assert_eq!(info.folders.len(), 1, "the later rule for a folder wins");
    assert!(info.folders[0].write && !info.ask_outside && !info.save_secrets_from_chat);
    let saved = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(
        saved.contains("sandbox = \"strict\"") && saved.contains("ask_outside = false"),
        "{saved}"
    );
}

/// How much freedom new chats get: one value for all the user's devices.
/// Unset until one of them sets it (the server keeps `agent.permission`),
/// then saved, given to every new chat nothing else chooses for, and kept
/// across restarts. Agents can't change it.
#[tokio::test]
async fn one_freedom_default_for_all_devices() {
    let h = harness_agent(vec![], 100_000, "permission = \"read_only\"").await;
    let call = |raw: Value| {
        let engine = &h.engine;
        async move {
            let req: Request = serde_json::from_value(raw).unwrap();
            engine.handle(req, None).await
        }
    };
    let security = || async {
        let v = call(json!({"method": "get_security"})).await.unwrap();
        serde_json::from_value::<SecurityInfo>(v).unwrap()
    };
    let saved = || std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert_eq!(security().await.default_permission, None);
    assert_eq!(h.session().await.permission, PermissionMode::ReadOnly);
    // An older client's request leaves it unset.
    call(json!({"method": "set_security", "params": {"ask_outside": false}}))
        .await
        .unwrap();
    let info = security().await;
    assert!(info.default_permission.is_none() && !info.ask_outside);
    assert!(!saved().contains("default_permission"), "{}", saved());

    let v = call(json!({"method": "set_security", "params": {"default_permission": "ask"}}))
        .await
        .unwrap();
    let info: SecurityInfo = serde_json::from_value(v).unwrap();
    assert_eq!(info.default_permission, Some(PermissionMode::Ask));
    assert!(!info.ask_outside, "the other settings stay");
    assert!(
        saved().contains("default_permission = \"ask\""),
        "{}",
        saved()
    );
    assert_eq!(h.session().await.permission, PermissionMode::Ask);
    let setup: SetupStatus =
        serde_json::from_value(call(json!({"method": "setup_status"})).await.unwrap()).unwrap();
    assert_eq!(setup.permission, PermissionMode::Ask);
    // Changing something else keeps it; an unknown value isn't a request.
    call(json!({"method": "set_security", "params": {"ask_outside": true}}))
        .await
        .unwrap();
    assert!(
        serde_json::from_value::<Request>(
            json!({"method": "set_security", "params": {"default_permission": "anything"}})
        )
        .is_err()
    );
    assert_eq!(
        security().await.default_permission,
        Some(PermissionMode::Ask)
    );

    // An agent can't change it, even when the user would approve.
    h.mock.push(Reply::Tools(vec![(
        "settings".into(),
        json!({"action": "set", "key": "security.default_permission", "value": "auto"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    let (asked, _) = run(
        &h,
        &s.id,
        "let new chats do anything",
        ApprovalDecision::Allow,
    )
    .await;
    assert!(asked.is_empty(), "{asked:?}");
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("not a setting agents may change"),
        "{results:?}"
    );
    assert_eq!(
        security().await.default_permission,
        Some(PermissionMode::Ask)
    );

    // Kept across a restart.
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let info: SecurityInfo =
        serde_json::from_value(engine.handle(Request::GetSecurity, None).await.unwrap()).unwrap();
    assert_eq!(info.default_permission, Some(PermissionMode::Ask));
}

#[tokio::test]
async fn settings_change_only_with_the_users_approval() {
    let h = harness(vec![], 100_000).await;
    h.mock.push(Reply::Tools(vec![
        ("settings".into(), json!({"action": "get"})),
        (
            "settings".into(),
            json!({"action": "set", "key": "memory.enabled", "value": false}),
        ),
    ]));
    h.mock.push(Reply::Tools(vec![(
        "settings".into(),
        json!({"action": "set", "key": "models.fast", "value": "mock/m"}),
    )]));
    let s = h.session().await;
    // Even in auto mode, where nothing else asks.
    set_auto(&h, &s.id).await;
    let (asked, _) = run(&h, &s.id, "turn memory off", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        asked[0].summary.contains("memory.enabled = false"),
        "{asked:?}"
    );
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].2.contains("[memory]") && results[0].2.contains("settings set"),
        "{:?}",
        results[0]
    );
    assert!(results[1].1 && results[1].2.contains("declined"));
    assert!(h.engine.config().memory.enabled);

    h.mock.push(Reply::Tools(vec![(
        "settings".into(),
        json!({"action": "set", "key": "memory.enabled", "value": false}),
    )]));
    let (asked, _) = run(&h, &s.id, "please, really", ApprovalDecision::Allow).await;
    assert_eq!(asked.len(), 1);
    assert!(!h.engine.config().memory.enabled);
    let saved = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(saved.contains("enabled = false"), "{saved}");
}

const ANTHROPIC: &str = "sk-ant-api03-Zq7d9Kx2Lm4Np6Rs8Tv0Wx3Yz5Ab7Cd9Ef1Gh3Ij5Kl7Mn9Op";
const GITHUB: &str = "ghp_Zq7d9Kx2Lm4Np6Rs8Tv0Wx3Yz5Ab7Cd9Ef1G";

fn secret_names(h: &Harness) -> Vec<String> {
    h.engine
        .auth()
        .list_secrets()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

#[tokio::test]
async fn secret_approval_says_a_program_can_send_the_value_anywhere() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    h.engine
        .auth()
        .save_secret(
            "FIXTURE_KEY",
            SecretKind::Token,
            "test service",
            std::collections::BTreeMap::from([("value".into(), "fixture-value-123456".into())]),
            &[],
            true,
            vec!["service.test".into()],
        )
        .unwrap();
    h.mock.push(bash("printf '%s' \"$FIXTURE_KEY\""));
    h.mock.push(Reply::Text("done".into()));
    let (asked, _) = run(&h, &s.id, "check the fixture", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), 1);
    assert!(asked[0].reason.contains("send the value anywhere"));
    assert!(asked[0].reason.contains("not a network boundary"));
    assert!(!asked[0].reason.contains("fixture-value-123456"));
}

#[tokio::test]
async fn keys_typed_in_a_chat_go_to_the_secret_store_before_anything_sees_them() {
    let h = harness(vec![Reply::Text("noted".into())], 100_000).await;
    let s = h.session().await;
    let msg = format!("here is my anthropic key {ANTHROPIC} and gh {GITHUB} for the deploy");
    let (_, events) = run(&h, &s.id, &msg, ApprovalDecision::Deny).await;
    assert_eq!(secret_names(&h), ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]);
    let entries = h.entries(&s.id).await;
    let (user_seq, user) = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::User { content, .. } => Some((e.seq, plain_text(content))),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        user,
        "here is my anthropic key $ANTHROPIC_API_KEY and gh $GITHUB_TOKEN for the deploy"
    );
    let (notice_seq, notice) = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::Notice { text, .. } if text.starts_with("Saved to Passwords & keys: ") => {
                Some((e.seq, text.clone()))
            }
            _ => None,
        })
        .expect("a notice");
    assert!(notice_seq > user_seq, "the notice follows the message");
    assert!(
        notice.contains("ANTHROPIC_API_KEY (Anthropic API key)")
            && notice.contains("$GITHUB_TOKEN"),
        "{notice}"
    );
    // Nowhere else: not the model, not the events, not the database.
    let sent = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    let shown = serde_json::to_string(&events).unwrap();
    let db = std::fs::read(h.dir.path().join("tessera.db")).unwrap();
    for v in [ANTHROPIC, GITHUB] {
        assert!(!sent.contains(v) && !shown.contains(v), "leaked {v}");
        assert!(
            !db.windows(v.len()).any(|w| w == v.as_bytes()),
            "{v} in the database"
        );
    }
    // The agent knows them by name, and commands can use them.
    assert!(sent.contains("$ANTHROPIC_API_KEY"));
    let use_ = h.engine.auth().secrets_for(
        "curl -H \"x-api-key: $ANTHROPIC_API_KEY\"",
        &SecretUser::Chat,
        SecretPlace::Server,
        &[],
    );
    assert_eq!(
        use_.env,
        [("ANTHROPIC_API_KEY".to_string(), ANTHROPIC.to_string())]
    );

    // The same key again keeps its name; another one gets a new one.
    let other = "sk-ant-api03-Different0Key1With2Enough3Characters4Here5";
    h.send(
        &s.id,
        &format!("again {ANTHROPIC}, and the staging one {other}"),
    )
    .await;
    h.settle(&s.id).await;
    assert_eq!(
        secret_names(&h),
        ["ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY_2", "GITHUB_TOKEN"]
    );

    // A private chat keeps nothing; a device token is never kept.
    let v = h
        .engine
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
        .unwrap();
    let private: SessionInfo = serde_json::from_value(v).unwrap();
    let token = "tsr_abcdefghijklmnopqrstuvwxyz0123456789AB";
    let key = "sk-or-v1-0123456789abcdef0123456789abcdef";
    h.send(&private.id, &format!("{key} and {token}")).await;
    h.settle(&private.id).await;
    let texts: Vec<String> = h
        .entries(&private.id)
        .await
        .iter()
        .map(|e| serde_json::to_string(&e.body).unwrap())
        .collect();
    let all = texts.join("\n");
    assert!(!all.contains(key) && !all.contains(token), "{all}");
    assert!(
        all.contains("[removed: OpenRouter API key]") && all.contains("device token"),
        "{all}"
    );
    assert_eq!(secret_names(&h).len(), 3);

    // Turned off: the message goes as typed.
    h.engine
        .handle(
            Request::SetSecurity {
                sandbox: None,
                folders: None,
                ask_outside: None,
                redact_patterns: None,
                save_secrets_from_chat: Some(false),
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
    let s2 = h.session().await;
    h.send(&s2.id, &format!("debug this: {key}")).await;
    h.settle(&s2.id).await;
    let all = serde_json::to_string(&h.entries(&s2.id).await).unwrap();
    assert!(all.contains(key));
}

/// The text of the last message the user sent in `sid`.
async fn last_user_text(h: &Harness, sid: &str) -> String {
    h.entries(sid)
        .await
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: None,
            } => Some(plain_text(content)),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn two_factor_codes_in_a_chat_go_to_passwords_and_keys() {
    let h = harness(vec![Reply::Text("noted".into())], 100_000).await;
    let s = h.session().await;
    let env = |cmd: &str| {
        h.engine
            .auth()
            .secrets_for(cmd, &SecretUser::Chat, SecretPlace::Server, &[])
            .env
    };
    // A one-time code: kept as ONE_TIME_CODE, the agent gets the name.
    h.send(&s.id, "my verification code is 482 913").await;
    h.settle(&s.id).await;
    assert_eq!(
        last_user_text(&h, &s.id).await,
        "my verification code is $ONE_TIME_CODE"
    );
    assert_eq!(
        env("type $ONE_TIME_CODE"),
        [("ONE_TIME_CODE".to_string(), "482913".to_string())]
    );
    // The next one replaces it: never a pile of stale codes.
    h.send(&s.id, "new one: 2FA: 551204").await;
    h.settle(&s.id).await;
    assert_eq!(env("type $ONE_TIME_CODE")[0].1, "551204");
    assert_eq!(secret_names(&h), ["ONE_TIME_CODE"]);

    // A setup link joins the login of that name (its username stays); the agent types $BANK_OTP.
    h.engine
        .auth()
        .save_secret(
            "BANK",
            SecretKind::Login,
            "My bank",
            std::collections::BTreeMap::from([
                ("username".to_string(), "ann".to_string()),
                ("password".to_string(), "testpw2-bank".to_string()),
            ]),
            &[],
            true,
            vec!["bank.example".into()],
        )
        .unwrap();
    let seed = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    h.send(
        &s.id,
        &format!("set up 2fa with otpauth://totp/Bank:ann?secret={seed}&issuer=Bank please"),
    )
    .await;
    h.settle(&s.id).await;
    assert_eq!(
        last_user_text(&h, &s.id).await,
        "set up 2fa with $BANK_OTP please"
    );
    let bank = h
        .engine
        .auth()
        .list_secrets()
        .into_iter()
        .find(|i| i.name == "BANK")
        .unwrap();
    assert!(
        bank.fields.contains(&"totp".to_string()) && bank.fields.contains(&"username".to_string())
    );
    assert_eq!(
        (bank.description.as_str(), bank.ask, bank.hosts.clone()),
        ("My bank", true, vec!["bank.example".to_string()])
    );
    let code = env("login --code $BANK_OTP");
    assert!(
        code[0].1.len() == 6 && code[0].1.chars().all(|c| c.is_ascii_digit()),
        "{code:?}"
    );
    // A key named as one, with nothing saying whose: a login of its own.
    h.send(&s.id, "my 2fa setup key: gezd gnbv gy3t qojq").await;
    h.settle(&s.id).await;
    assert_eq!(
        last_user_text(&h, &s.id).await,
        "my 2fa setup key: $TWO_FACTOR_OTP"
    );
    assert!(secret_names(&h).contains(&"TWO_FACTOR".to_string()));
    // The notices say where they went; the model never saw a code or a key.
    let notices: Vec<String> = h
        .entries(&s.id)
        .await
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } if text.starts_with("Saved to Passwords & keys: ") => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("ONE_TIME_CODE (one-time code)")),
        "{notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|n| n.contains("BANK (two-factor key)") && n.contains("$BANK_OTP")),
        "{notices:?}"
    );
    let sent = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    for v in ["482913", "482 913", "551204", seed, "gezd gnbv"] {
        assert!(!sent.contains(v), "the model saw {v}");
    }

    // Ordinary numbers stay as they are.
    h.send(&s.id, "the error code is 500123 on port 8080").await;
    h.settle(&s.id).await;
    assert_eq!(
        last_user_text(&h, &s.id).await,
        "the error code is 500123 on port 8080"
    );

    // A private chat keeps nothing: the code is taken out.
    let v = h
        .engine
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
        .unwrap();
    let private: SessionInfo = serde_json::from_value(v).unwrap();
    h.send(&private.id, "login code: 777123").await;
    h.settle(&private.id).await;
    assert_eq!(
        last_user_text(&h, &private.id).await,
        "login code: [removed: one-time code]"
    );
    assert_eq!(env("type $ONE_TIME_CODE")[0].1, "551204");
}

#[tokio::test]
async fn the_agent_moves_a_login_out_of_the_conversation() {
    let h = harness(vec![], 100_000).await;
    let password = "Gr33n-Tea!Pot";
    h.mock.push(Reply::ThinkTools(
        format!("The user gave login ann.k / {password}; I should store it."),
        vec![(
            "save_secret".into(),
            json!({"name": "BANK", "username": "ann.k", "password": password, "url": "https://bank.example/login"}),
        )],
    ));
    // Values the user never typed can't be planted.
    h.mock.push(Reply::Tools(vec![(
        "save_secret".into(),
        json!({"name": "OTHER", "value": "made-up-value-123"}),
    )]));
    h.mock
        .push(Reply::Text(format!("Saved. I won't repeat {password}.")));
    let s = h.session().await;
    let (_, events) = run(
        &h,
        &s.id,
        &format!("my bank login is ann.k / {password} — keep it safe"),
        ApprovalDecision::Allow,
    )
    .await;
    assert_eq!(secret_names(&h), ["BANK"]);
    let info = h.engine.auth().list_secrets().remove(0);
    assert_eq!(info.kind, SecretKind::Login);
    assert_eq!(info.fields, ["password", "url", "username"]);
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("$BANK_PASSWORD"),
        "{:?}",
        results[0]
    );
    assert!(
        results[1].1 && results[1].2.contains("isn't in the user's own messages"),
        "{:?}",
        results[1]
    );
    // Every stored item is rewritten: the message, the call, the reasoning,
    // even the reply that repeated it.
    let stored = serde_json::to_string(&h.entries(&s.id).await).unwrap();
    assert!(!stored.contains(password), "{stored}");
    assert!(
        stored.contains("my bank login is $BANK_USERNAME / $BANK_PASSWORD"),
        "{stored}"
    );
    // The requests after the call no longer carry it.
    let reqs = h.mock.agent_requests();
    assert!(serde_json::to_string(&reqs[0]).unwrap().contains(password));
    for r in &reqs[1..] {
        assert!(!serde_json::to_string(r).unwrap().contains(password));
    }
    // Clients are told to replace what they show.
    let changed = events.iter().any(|e| {
        matches!(e, Event::EntriesChanged { entries, .. }
            if !serde_json::to_string(entries).unwrap().contains(password))
    });
    assert!(changed);
    // Commands get the login by name.
    let env = h
        .engine
        .auth()
        .secrets_for(
            "login $BANK_USERNAME $BANK_PASSWORD",
            &SecretUser::Chat,
            SecretPlace::Server,
            &[],
        )
        .env;
    assert!(env.contains(&("BANK_PASSWORD".to_string(), password.to_string())));
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_link_changed_while_approval_is_pending_is_checked_again() {
    use std::os::unix::fs::symlink;
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join(".netrc");
    std::fs::write(&path, "ordinary notes").unwrap();
    let protected = h.dir.path().join("auth.json");
    std::fs::write(&protected, "private fixture notes").unwrap();
    h.mock
        .push(Reply::Tools(vec![("read".into(), json!({"path":path}))]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "read the notes").await;
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Event::ApprovalRequested { request, .. } = events.recv().await.unwrap() {
                break request;
            }
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&protected, &path).unwrap();
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
    let result = tool_results(&h.entries(&s.id).await);
    assert!(result[0].1, "{result:?}");
    assert!(!result[0].2.contains("private fixture notes"));
}

#[tokio::test]
async fn native_decline_is_a_tool_result_and_read_chains_still_need_a_card() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
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
    h.mock.push(Reply::Tools(vec![
        (
            "bash".into(),
            json!({"command":"echo would-write > declined-file"}),
        ),
        ("bash".into(), json!({"command":"ls | head | wc -l"})),
        (
            "bash".into(),
            json!({"command":format!("cat '{}/config.toml' | head", h.dir.path().display())}),
        ),
    ]));
    h.mock
        .push(Reply::Text("Carried on with other work.".into()));
    let (asked, _) = run(&h, &s.id, "check", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), 2);
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1 && results[0].2.contains("declined this bash action"));
    assert!(results[1].1 && results[1].2.contains("declined this bash action"));
    assert!(results[2].1 && results[2].2.contains("Tessera's own"));
    assert!(!h.work.join("declined-file").exists());
    assert!(h.entries(&s.id).await.iter().any(|e| matches!(&e.body,
        EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p,
            tessera_proto::AssistantPart::Text { text } if text == "Carried on with other work.")))));
}

#[tokio::test]
async fn unanswered_native_approval_expires_without_decline_or_stop() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    h.mock.push(bash("echo sudo fixture"));
    h.mock.push(Reply::Text("Carried on.".into()));
    let mut rx = h.engine.subscribe();
    h.send(&s.id, "check").await;
    loop {
        if matches!(rx.recv().await.unwrap(), Event::ApprovalRequested { .. }) {
            break;
        }
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(901)).await;
    // Let network-backed mock requests run under real time again.
    tokio::time::resume();
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("nobody answered"),
        "{results:?}"
    );
    assert!(!results[0].2.contains("declined"));
    assert!(h.entries(&s.id).await.iter().any(|e| matches!(&e.body,
        EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p,
            tessera_proto::AssistantPart::Text { text } if text == "Carried on.")))));
}

#[cfg(unix)]
#[tokio::test]
async fn read_commands_authorize_targets_or_prompt_instead_of_guessing() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
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
    std::fs::write(h.work.join("ordinary.log"), "ordinary fixture\n").unwrap();
    std::fs::create_dir_all(h.dir.path().join("skills")).unwrap();
    std::fs::create_dir_all(h.dir.path().join("memory")).unwrap();
    std::fs::write(
        h.dir.path().join("memory/USER.md"),
        "protected fixture sentinel",
    )
    .unwrap();
    std::os::unix::fs::symlink(h.dir.path(), h.work.join("store-link")).unwrap();
    for path in [
        ".netrc",
        ".git-credentials",
        ".ssh/id_ed25519",
        ".aws/credentials",
        ".config/gcloud/application_default_credentials.json",
        ".azure/config",
        ".kube/config",
    ] {
        let file = h.work.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "private fixture sentinel").unwrap();
    }
    std::os::unix::fs::symlink(".netrc", h.work.join("ordinary-alias")).unwrap();
    // All rejected cards: credentials must never execute merely because
    // their program name looks read-only. No real home file is accessed.
    let mut commands: Vec<String> = [
        "cat .netrc | head -20",
        "cat ~/.netrc | head -20",
        "cat .git-credentials | head",
        "cat .ssh/id_ed25519",
        "cat .aws/credentials",
        "cat .config/gcloud/application_default_credentials.json",
        "cat .azure/config",
        "cat .kube/config",
        "cat ordinary-alias | head",
        "cat ordinary.log | head",
        "cat missing-file | head",
        "cat ordinary* | head",
        "cat ordinary?",
        "cat ordinary[12]",
        "sha256sum -c ordinary.log",
        "grep -r sentinel .",
        "grep -R sentinel .",
        "grep -f .netrc ordinary.log",
        "grep --file=.netrc ordinary.log",
        "wc --files0-from=ordinary.log",
        "ls -R .",
        "cat $(printf ordinary.log)",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let prompt_count = commands.len();
    commands.push("cat store-link/memory/USER.md | head -20".into());
    commands.push(format!(
        "cat '{}/skills/../config.toml' | head -20",
        h.dir.path().display()
    ));
    h.mock.push(Reply::Tools(
        commands
            .iter()
            .map(|c| ("bash".into(), json!({"command":c})))
            .collect(),
    ));
    h.mock.push(Reply::Text("done".into()));
    let (asked, _) = run(&h, &s.id, "review reads", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), prompt_count);
    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(results.len(), commands.len());
    for result in &results {
        assert!(
            result.1 && !result.2.contains("fixture sentinel"),
            "{result:?}"
        );
    }
    for result in &results[prompt_count..] {
        assert!(result.2.contains("Tessera's own"), "{result:?}");
    }
    assert!(
        asked
            .iter()
            .take(9)
            .all(|a| a.reason.contains("credential")),
        "{asked:?}"
    );
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
                permission: Some(PermissionMode::Default),
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
    h.mock.push(Reply::Tools(
        commands
            .iter()
            .map(|c| ("bash".into(), json!({"command":c})))
            .collect(),
    ));
    h.mock.push(Reply::Text("done".into()));
    let (asked, _) = run(
        &h,
        &s.id,
        "Default still guards uncertain reads",
        ApprovalDecision::Deny,
    )
    .await;
    assert_eq!(asked.len(), prompt_count - 1); // only the existing regular log is authorized
    let all = tool_results(&h.entries(&s.id).await);
    let results = &all[all.len() - commands.len()..];
    for (i, result) in results.iter().enumerate() {
        if commands[i] == "cat ordinary.log | head" {
            assert!(
                !result.1 && result.2.contains("ordinary fixture"),
                "{result:?}"
            );
        } else {
            assert!(
                result.1 && !result.2.contains("fixture sentinel"),
                "{result:?}"
            );
        }
    }
}

/// Saved state changes require their own approval, not a whole-tool grant.
#[tokio::test]
async fn durable_note_and_memory_changes_obey_ask_and_read_only() {
    let h = harness_agent(vec![], 100_000, "permission = \"ask\"").await;
    let s = h.session().await;
    let create = json!({"action":"create", "title":"Fixture note", "content":"Plain text"});
    h.mock
        .push(Reply::Tools(vec![("note".into(), create.clone())]));
    h.mock.push(Reply::Text("Done.".into()));
    let (asked, _) = run(
        &h,
        &s.id,
        "Save the fixture note",
        ApprovalDecision::AllowSession,
    )
    .await;
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].arguments, create);
    let notes: Vec<NoteInfo> =
        serde_json::from_value(h.engine.handle(Request::ListNotes, None).await.unwrap()).unwrap();
    assert_eq!(notes.len(), 1);
    let update = json!({"action":"update", "id":notes[0].id, "append":"A second line"});
    h.mock
        .push(Reply::Tools(vec![("note".into(), update.clone())]));
    h.mock.push(Reply::Text("Done.".into()));
    let (asked, _) = run(&h, &s.id, "Append a line", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), 1, "creating a note must not allow later edits");
    assert_eq!(asked[0].arguments, update);
    let memory = json!({"action":"add", "target":"user", "content":"Prefers plain fixture notes"});
    h.mock
        .push(Reply::Tools(vec![("memory".into(), memory.clone())]));
    h.mock.push(Reply::Text("Done.".into()));
    let (asked, _) = run(
        &h,
        &s.id,
        "Remember the fixture preference",
        ApprovalDecision::Allow,
    )
    .await;
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].arguments, memory);
    assert!(!tool_results(&h.entries(&s.id).await).last().unwrap().1);
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
    h.mock.push(Reply::Tools(vec![
        ("note".into(), update),
        ("memory".into(), memory),
        ("task".into(), json!({"goal":"Read the fixture"})),
        ("note".into(), json!({"action":"list"})),
    ]));
    h.mock.push(Reply::Text("Done.".into()));
    let (asked, _) = run(&h, &s.id, "Review without changes", ApprovalDecision::Allow).await;
    assert!(asked.is_empty());
    let results = tool_results(&h.entries(&s.id).await);
    let results = &results[results.len() - 4..];
    assert!(results[0].1 && results[0].2.contains("read-only"));
    assert!(results[1].1 && results[1].2.contains("read-only"));
    assert!(!results[2].1 && !results[3].1, "{results:?}");
}
