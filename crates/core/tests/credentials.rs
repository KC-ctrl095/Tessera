//! Passwords & keys: no reply ever carries a value; editing keeps what is
//! stored unless replaced; who can use each one (any chat, or only some
//! bots) and where (this server, some machines) holds for every kind of
//! session, and the agent is told only about what it may use.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::auth::{SecretPlace, SecretUse};
use tessera_proto::*;

const ANY: &str = "any-value-0a1b2c3d";
const SWEEP: &str = "sweep-value-4e5f6a7b";
const LAB: &str = "lab-value-8c9d0e1f";
const PASS: &str = "router-pass-2a3b4c";
const PASS2: &str = "router-pass-5d6e7f";

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
        // These tests are about keys, not machine access: the bots may use
        // whatever a machine allows (new bots start with none otherwise).
        machines: Some(BotMachines {
            default: MachineAccess::Allowed,
            ..Default::default()
        }),
        // Likewise every password open to any chat (new bots get none):
        // `a_bots_own_choice_of_open_passwords` tests that choice.
        secrets: Some(BotSecrets {
            default: true,
            ..Default::default()
        }),
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

async fn chat(h: &Harness, bot: Option<&Bot>, private: bool) -> SessionInfo {
    let v = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: Some(h.work.display().to_string()),
                model: None,
                title: None,
                bot_id: bot.map(|b| b.id.clone()),
                private,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

/// `save_secret` as a client sends it; the raw reply.
#[allow(clippy::too_many_arguments)]
async fn save(
    h: &Harness,
    name: &str,
    kind: SecretKind,
    fields: &[(&str, &str)],
    bots: Option<Option<Vec<String>>>,
    on_server: Option<bool>,
    on_machines: Option<Vec<String>>,
    new_name: Option<&str>,
) -> Result<Value, ApiError> {
    h.engine
        .handle(
            Request::SaveSecret {
                name: name.into(),
                kind,
                description: format!("{name} for the homelab"),
                fields: fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                remove_fields: vec![],
                ask: true,
                hosts: vec![],
                bots,
                on_server,
                on_machines,
                new_name: new_name.map(str::to_string),
                machines_default: None,
            },
            None,
        )
        .await
}

fn names(u: &SecretUse) -> Vec<&str> {
    u.env.iter().map(|(k, _)| k.as_str()).collect()
}

/// Sends `text`, allows every approval, waits for the run to end. Returns
/// the approvals asked.
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

/// What the agent was told (system prompt and tool list) in the requests
/// whose conversation holds `marker`.
fn told(h: &Harness, marker: &str) -> String {
    h.mock
        .agent_requests()
        .iter()
        .filter(|r| r.to_string().contains(marker))
        .map(|r| format!("{}\n{}", r["messages"][0]["content"], r["tools"]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prints whether each variable reached the command, never a value.
fn probe(vars: &[&str]) -> Value {
    let cmd = vars
        .iter()
        .map(|v| format!("[ -n \"${v}\" ] && echo {v}:set || echo {v}:unset"))
        .collect::<Vec<_>>()
        .join("; ");
    json!({ "command": cmd })
}

#[tokio::test]
async fn no_reply_carries_a_value_and_blank_fields_keep_what_is_stored() {
    let h = harness(vec![], 100_000).await;
    let mut events = h.engine.subscribe();
    let s = h.session().await;
    let created = save(
        &h,
        "ROUTER",
        SecretKind::Login,
        &[("username", "admin-kim"), ("password", PASS)],
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let env = |cmd: &str| h.engine.secret_env(&s.id, cmd, SecretPlace::Server).env;
    assert_eq!(
        env("$ROUTER_PASSWORD"),
        [("ROUTER_PASSWORD".to_string(), PASS.to_string())]
    );
    // Edited with the value fields left blank: everything else changes,
    // the values stay.
    let edited = save(
        &h,
        "ROUTER",
        SecretKind::Login,
        &[("username", ""), ("password", "")],
        Some(Some(vec![])),
        Some(true),
        Some(vec![]),
        None,
    )
    .await
    .unwrap();
    assert_eq!(edited["bots"], json!([]));
    assert!(env("$ROUTER_PASSWORD").is_empty(), "nobody may use it now");
    save(
        &h,
        "ROUTER",
        SecretKind::Login,
        &[],
        Some(None),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        env("$ROUTER_PASSWORD $ROUTER_USERNAME"),
        [
            ("ROUTER_PASSWORD".to_string(), PASS.to_string()),
            ("ROUTER_USERNAME".to_string(), "admin-kim".to_string())
        ]
    );
    // Replaced: the new value.
    let replaced = save(
        &h,
        "ROUTER",
        SecretKind::Login,
        &[("password", PASS2)],
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        env("$ROUTER_PASSWORD"),
        [("ROUTER_PASSWORD".to_string(), PASS2.to_string())]
    );
    // Renamed: same values under the new name only.
    let renamed = save(
        &h,
        "ROUTER",
        SecretKind::Login,
        &[],
        None,
        None,
        None,
        Some("LAN_ROUTER"),
    )
    .await
    .unwrap();
    assert_eq!(renamed["name"], "LAN_ROUTER");
    assert!(env("$ROUTER_PASSWORD").is_empty());
    assert_eq!(env("$LAN_ROUTER_PASSWORD")[0].1, PASS2);
    let listed = h.engine.handle(Request::ListSecrets, None).await.unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    // Nothing the server answered or announced holds a value.
    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        seen.push(e);
    }
    let all = serde_json::to_string(&(created, edited, replaced, renamed, listed, &seen)).unwrap();
    for v in [PASS, PASS2, "admin-kim"] {
        assert!(!all.contains(v), "{v} came back: {all}");
    }
}

#[tokio::test]
async fn who_can_use_it_holds_for_chats_bots_duties_and_helpers() {
    let h = harness(vec![], 100_000).await;
    let sentinel = save_bot(&h, bot("Sentinel")).await;
    let other = save_bot(&h, bot("Other")).await;
    let only_sentinel = Some(Some(vec![sentinel.id.clone()]));
    save(
        &h,
        "ANY_KEY",
        SecretKind::Token,
        &[("value", ANY)],
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    save(
        &h,
        "SWEEP_KEY",
        SecretKind::Token,
        &[("value", SWEEP)],
        only_sentinel.clone(),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    save(
        &h,
        "LAB_KEY",
        SecretKind::Token,
        &[("value", LAB)],
        only_sentinel,
        Some(false),
        Some(vec!["m_lab".into()]),
        None,
    )
    .await
    .unwrap();
    let all = "$ANY_KEY $SWEEP_KEY $LAB_KEY";

    // An ordinary chat: any-chat keys, after asking; the bot's aren't there.
    let normal = chat(&h, None, false).await;
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        probe(&["ANY_KEY", "SWEEP_KEY", "LAB_KEY"]),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &normal.id, "NORMAL-CHAT check the keys").await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].reason.contains("$ANY_KEY") && !asked[0].reason.contains("SWEEP"),
        "{asked:?}"
    );
    let out = tool_results(&h.entries(&normal.id).await).remove(0).2;
    assert!(
        out.contains("ANY_KEY:set")
            && out.contains("SWEEP_KEY:unset")
            && out.contains("LAB_KEY:unset"),
        "{out}"
    );
    // Its instructions and tools never mention the bot's keys.
    let sent = told(&h, "NORMAL-CHAT");
    assert!(sent.contains("$ANY_KEY"), "listed: {sent}");
    assert!(
        !sent.contains("SWEEP_KEY") && !sent.contains("LAB_KEY"),
        "{sent}"
    );
    let everything = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    for v in [ANY, SWEEP, LAB] {
        assert!(!everything.contains(v), "{v} reached a model");
    }

    // The listed bot: its own keys without asking.
    let s_chat = chat(&h, Some(&sentinel), false).await;
    h.mock
        .push(Reply::Tools(vec![("bash".into(), probe(&["SWEEP_KEY"]))]));
    // On this server the machine-only key is refused, saying where it works.
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "echo $LAB_KEY"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s_chat.id, "SENTINEL-CHAT sweep").await;
    assert!(asked.is_empty(), "{asked:?}");
    let results = tool_results(&h.entries(&s_chat.id).await);
    assert!(results[0].2.contains("SWEEP_KEY:set"), "{results:?}");
    assert!(
        results[1].1 && results[1].2.contains("$LAB_KEY can't be used"),
        "{results:?}"
    );
    let sent = told(&h, "SENTINEL-CHAT");
    assert!(
        sent.contains("$SWEEP_KEY") && sent.contains("$ANY_KEY"),
        "{sent}"
    );

    // Its subagents act as it.
    let sub_key = "SUBAGENT-TASK";
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait": true, "tasks": [{"name": "helper", "prompt": format!("{sub_key} check")}]}),
    )]));
    h.mock.route(
        sub_key,
        Reply::Tools(vec![("bash".into(), probe(&["SWEEP_KEY"]))]),
    );
    h.mock.route(sub_key, Reply::Text("helper done".into()));
    h.mock.push(Reply::Text("ok".into()));
    let mut ev = h.engine.subscribe();
    run(&h, &s_chat.id, "SENTINEL-CHAT start a helper").await;
    let mut sub = None;
    while let Ok(e) = ev.try_recv() {
        if let Event::SessionUpdated { session } = e
            && session.kind == SessionKind::Subagent
            && session.parent_id.as_deref() == Some(s_chat.id.as_str())
        {
            sub = Some(session.id);
        }
    }
    let sub = sub.expect("a subagent");
    let out = tool_results(&h.entries(&sub).await).remove(0).2;
    assert!(out.contains("SWEEP_KEY:set"), "{out}");

    // Its scheduled duties run as it, with nobody there to ask.
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Nightly sweep".into(),
                        schedule: "0 3 * * *".into(),
                        prompt: "NIGHTLY-DUTY check everything".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: Some(sentinel.id.clone()),
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
        "NIGHTLY-DUTY",
        Reply::Tools(vec![("bash".into(), probe(&["SWEEP_KEY"]))]),
    );
    h.mock.route("NIGHTLY-DUTY", Reply::Text("swept".into()));
    let duty: SessionInfo = serde_json::from_value(
        h.engine
            .handle(Request::RunJob { id: job.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    h.settle(&duty.id).await;
    let out = tool_results(&h.entries(&duty.id).await).remove(0).2;
    assert!(out.contains("SWEEP_KEY:set"), "{out}");

    // The same check every tool makes, for each kind of session.
    let other_chat = chat(&h, Some(&other), false).await;
    let private = chat(&h, None, true).await;
    let at = |id: &str, place| h.engine.secret_env(id, all, place);
    assert_eq!(names(&at(&normal.id, SecretPlace::Server)), ["ANY_KEY"]);
    assert_eq!(names(&at(&other_chat.id, SecretPlace::Server)), ["ANY_KEY"]);
    for id in [&s_chat.id, &sub, &duty.id] {
        let u = at(id, SecretPlace::Server);
        assert_eq!(names(&u), ["ANY_KEY", "SWEEP_KEY"], "{id}");
        assert!(u.used.contains(&("SWEEP_KEY".to_string(), false, vec![])));
        assert!(u.used.contains(&("ANY_KEY".to_string(), true, vec![])));
        assert_eq!(names(&at(id, SecretPlace::Machine("m_lab"))), ["LAB_KEY"]);
        assert!(at(id, SecretPlace::Machine("m_other")).env.is_empty());
    }
    for id in [&normal.id, &other_chat.id] {
        let u = at(id, SecretPlace::Machine("m_lab"));
        assert!(
            u.env.is_empty() && u.elsewhere.iter().all(|e| e.0 == "ANY_KEY"),
            "{u:?}"
        );
    }
    let p = at(&private.id, SecretPlace::Server);
    assert!(p.env.is_empty() && p.used.is_empty() && p.elsewhere.is_empty());
}

#[tokio::test]
async fn handing_work_to_a_bot_or_its_duty_does_not_get_around_it() {
    let h = harness(vec![], 100_000).await;
    // With every tool, `schedule` included.
    let sentinel = save_bot(
        &h,
        Bot {
            tools: ToolPreset::Full,
            ..bot("Sentinel")
        },
    )
    .await;
    save(
        &h,
        "SWEEP_KEY",
        SecretKind::Token,
        &[("value", SWEEP)],
        Some(Some(vec![sentinel.id.clone()])),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Nightly sweep".into(),
                        schedule: "0 3 * * *".into(),
                        prompt: "check the switches".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: Some(sentinel.id.clone()),
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
    // An ordinary chat can't rewrite or remove the bot's duty…
    let normal = chat(&h, None, false).await;
    h.mock.push(Reply::Tools(vec![(
        "schedule".into(),
        json!({"action": "update", "id": job.id, "prompt": "print $SWEEP_KEY"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "schedule".into(),
        json!({"action": "delete", "id": job.id}),
    )]));
    // …and asking the bot itself is shown to the user first.
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": format!("@{}", sentinel.handle), "message": "hello", "wait": true}),
    )]));
    h.mock.push(Reply::Text("Hi.".into()));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &normal.id, "HANDOFF try").await;
    let results = tool_results(&h.entries(&normal.id).await);
    assert!(
        results[0].1 && results[0].2.contains("is a bot's duty"),
        "{results:?}"
    );
    assert!(
        results[1].1 && results[1].2.contains("is a bot's duty"),
        "{results:?}"
    );
    let jobs: Vec<Job> =
        serde_json::from_value(h.engine.handle(Request::ListJobs, None).await.unwrap()).unwrap();
    assert_eq!(jobs[0].prompt, "check the switches");
    assert!(
        asked.iter().any(|a| a
            .reason
            .contains("can use saved passwords or keys this chat can't")),
        "{asked:?}"
    );
    // Its own chat may change its duties.
    let own = chat(&h, Some(&sentinel), false).await;
    h.mock.push(Reply::Tools(vec![(
        "schedule".into(),
        json!({"action": "update", "id": job.id, "prompt": "check the switches and the NAS"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &own.id, "OWN-DUTY update").await;
    let jobs: Vec<Job> =
        serde_json::from_value(h.engine.handle(Request::ListJobs, None).await.unwrap()).unwrap();
    assert_eq!(jobs[0].prompt, "check the switches and the NAS");

    // A deleted bot is forgotten: nobody uses its keys any more.
    h.engine
        .handle(
            Request::DeleteBot {
                id: sentinel.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let info = h.engine.auth().list_secrets().remove(0);
    assert_eq!(info.bots, Some(vec![]));
    assert!(
        h.engine
            .secret_env(&own.id, "$SWEEP_KEY", SecretPlace::Server)
            .env
            .is_empty()
    );
}

#[tokio::test]
async fn a_value_typed_where_it_may_not_be_used_reveals_nothing() {
    let h = harness(vec![], 100_000).await;
    let sentinel = save_bot(&h, bot("Sentinel")).await;
    save(
        &h,
        "ANY_KEY",
        SecretKind::Token,
        &[("value", ANY)],
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    save(
        &h,
        "SWEEP_KEY",
        SecretKind::Token,
        &[("value", SWEEP)],
        Some(Some(vec![sentinel.id.clone()])),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let typed = format!("use {ANY} and {SWEEP} please");
    let mut stored = Vec::new();
    for b in [None, Some(&sentinel)] {
        let s = chat(&h, b, false).await;
        h.mock.push(Reply::Text("ok".into()));
        run(&h, &s.id, &typed).await;
        stored.push(serde_json::to_string(&h.entries(&s.id).await).unwrap());
    }
    // An ordinary chat gets neither the value nor the name of the bot's key.
    assert!(
        stored[0].contains("use $ANY_KEY and [a stored secret] please"),
        "{}",
        stored[0]
    );
    assert!(!stored[0].contains("SWEEP_KEY"), "{}", stored[0]);
    assert!(
        stored[0].contains("only certain bots may use it"),
        "{}",
        stored[0]
    );
    // The bot's own chat gets its name.
    assert!(
        stored[1].contains("use $ANY_KEY and $SWEEP_KEY please"),
        "{}",
        stored[1]
    );
    let sent = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    for v in [ANY, SWEEP] {
        assert!(
            !stored.iter().any(|s| s.contains(v)) && !sent.contains(v),
            "{v} was kept or sent"
        );
    }
}

/// A small stand-in for the `claude` CLI (the same stream-json and in-band
/// MCP protocol as crates/core/tests/claude_code.rs): a message
/// `tessera NAME {json}` calls Tessera's tool NAME through the bridge, as
/// Claude Code does. It logs what it was told and what the tool said.
const CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
def log(kind, value):
    with open(os.path.join(HERE, "calls.jsonl"), "a") as f:
        f.write(json.dumps({"kind": kind, "value": value}) + "\n")
def arg(name):
    return args[args.index(name) + 1] if name in args else None
if args[:1] == ["--version"]:
    print("9.9.9 (Claude Code)"); sys.exit(0)
if args[:2] == ["auth", "status"]:
    print(json.dumps({"loggedIn": True, "authMethod": "claude.ai", "apiProvider": "firstParty", "subscriptionType": "max"})); sys.exit(0)
if arg("--output-format") == "json":
    sys.stdin.read()
    print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "Stub Title",
                      "usage": {"input_tokens": 1, "output_tokens": 1}, "total_cost_usd": 0}))
    sys.exit(0)
if arg("--append-system-prompt-file"):
    log("context", open(arg("--append-system-prompt-file")).read())
session = "sess-%d" % os.getpid()
def emit(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
inbox = []
def wait_for(pred):
    for i, m in enumerate(inbox):
        if pred(m):
            return inbox.pop(i)
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        m = json.loads(line)
        log("in", m)
        if pred(m):
            return m
        inbox.append(m)
k = [0]
def mcp(method, params=None):
    k[0] += 1
    rid = "mcp-%d" % k[0]
    msg = {"jsonrpc": "2.0", "id": k[0], "method": method}
    if params is not None:
        msg["params"] = params
    emit({"type": "control_request", "request_id": rid, "request": {"subtype": "mcp_message", "server_name": "tessera", "message": msg}})
    m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == rid)
    if not m or m["response"]["subtype"] != "success":
        return None
    return m["response"]["response"]["mcp_response"]
n = [0]
def say(text, tools=()):
    n[0] += 1
    mid = "msg_%d" % n[0]
    usage = {"input_tokens": 10, "output_tokens": 5}
    emit({"type": "assistant", "parent_tool_use_id": None, "message": {"id": mid, "model": "claude-test-9", "content": [{"type": "text", "text": text}], "usage": usage}})
    for tid, name, inp in tools:
        emit({"type": "assistant", "parent_tool_use_id": None, "message": {"id": mid, "model": "claude-test-9", "content": [{"type": "tool_use", "id": tid, "name": name, "input": inp}], "usage": usage}})
def result(text):
    emit({"type": "result", "subtype": "success", "is_error": False, "result": text, "session_id": session,
          "total_cost_usd": 0, "usage": {"input_tokens": 10, "output_tokens": 5}})
def read():
    # What arrived while waiting for a reply comes first.
    if inbox:
        return inbox.pop(0)
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    m = json.loads(line)
    log("in", m)
    return m
while True:
    m = read()
    if m.get("type") == "control_request" and m["request"]["subtype"] == "initialize":
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {"models": [{"value": "opus", "displayName": "Opus 9"}]}}})
        mcp("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "claude-code"}})
        mcp("notifications/initialized")
        mcp("tools/list")
        continue
    if m.get("type") != "user":
        continue
    text = " ".join(b.get("text", "") for b in m["message"]["content"] if b.get("type") == "text")
    emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-test-9", "apiKeySource": "none"})
    if "--replay-user-messages" in sys.argv:
        emit({"type": "user", "isReplay": True, "uuid": m.get("uuid"), "message": m["message"], "session_id": session})
    if "tessera " in text:
        name, _, js = text.split("tessera ", 1)[1].strip().partition(" ")
        inp = json.loads(js or "{}")
        tid = "toolu_%d" % n[0]
        say("Using " + name, [(tid, "mcp__tessera__" + name, inp)])
        r = mcp("tools/call", {"name": name, "arguments": inp, "_meta": {"claudecode/toolUseId": tid}})
        res = (r or {}).get("result", {})
        out = "".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
        emit({"type": "user", "parent_tool_use_id": None, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid, "content": out, "is_error": res.get("isError", False)}]}})
        log("called", {"name": name, "text": out})
        say("Done.")
    else:
        say("ok")
    result("ok")
"#;

/// A Claude Code chat calls Tessera's tools through the same dispatch, so
/// who can use a key holds there exactly as in Tessera's own chats.
#[tokio::test]
async fn claude_code_chats_get_exactly_what_the_same_chat_would() {
    let fake = tempfile::tempdir().unwrap();
    let claude = fake.path().join("claude");
    std::fs::write(&claude, CLAUDE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mock = Mock::start(vec![]).await;
    let home = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"default_model = "claude-code/opus"
[agent]
review_interval = 0
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{}"
discover = false
[[provider.models]]
id = "m"
context_window = 100000
[[provider]]
id = "claude-code"
name = "Claude"
kind = "claude_code"
base_url = "{}"
[[provider.models]]
id = "opus"
"#,
            mock.url,
            claude.display()
        ),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(home.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    let call = |req: Request| {
        let e = engine.clone();
        async move { e.handle(req, None).await.unwrap() }
    };
    let sentinel: Bot = serde_json::from_value(
        call(Request::SaveBot {
            bot: bot("Sentinel"),
        })
        .await,
    )
    .unwrap();
    for (name, value, bots) in [
        ("ANY_KEY", ANY, None),
        ("SWEEP_KEY", SWEEP, Some(Some(vec![sentinel.id.clone()]))),
    ] {
        call(Request::SaveSecret {
            name: name.into(),
            kind: SecretKind::Token,
            description: String::new(),
            fields: [("value".to_string(), value.to_string())].into(),
            remove_fields: vec![],
            ask: false,
            hosts: vec![],
            bots,
            on_server: None,
            on_machines: None,
            new_name: None,
            machines_default: None,
        })
        .await;
    }
    let logged = |kind: &str| -> Vec<Value> {
        std::fs::read_to_string(fake.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|c| c["kind"] == kind)
            .map(|c| c["value"].clone())
            .collect()
    };
    let cmd = probe(&["ANY_KEY", "SWEEP_KEY"]);
    let mut seen = Vec::new();
    for bot_id in [None, Some(sentinel.id.clone())] {
        let s: SessionInfo = serde_json::from_value(
            call(Request::CreateSession {
                cwd: Some(work.display().to_string()),
                model: Some(ModelRef {
                    provider: "claude-code".into(),
                    model: "opus".into(),
                }),
                title: None,
                bot_id: bot_id.clone(),
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
            permission: Some(PermissionMode::Auto),
            pinned: None,
            archived: None,
            cwd: None,
            members: None,
            fast: None,
            subagents: None,
        })
        .await;
        call(Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text(format!("tessera bash {cmd}"))],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        })
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        if tokio::time::timeout(Duration::from_secs(20), engine.wait_idle(&s.id))
            .await
            .is_err()
        {
            let snap = engine
                .handle(
                    Request::GetSession {
                        session_id: s.id.clone(),
                        since_seq: None,
                    },
                    None,
                )
                .await
                .unwrap();
            panic!(
                "the Claude Code turn didn't finish:\n{}\n{snap}",
                std::fs::read_to_string(fake.path().join("calls.jsonl")).unwrap_or_default()
            );
        }
        let out = logged("called").pop().expect("the tool was called")["text"]
            .as_str()
            .unwrap()
            .to_string();
        let context = logged("context").pop().unwrap_or_default();
        // The same identity a Tessera turn in this chat would get.
        let direct = names(&engine.secret_env(&s.id, "$ANY_KEY $SWEEP_KEY", SecretPlace::Server))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        seen.push((out, context.to_string(), direct));
    }
    let (out, context, direct) = &seen[0];
    assert!(
        out.contains("ANY_KEY:set") && out.contains("SWEEP_KEY:unset"),
        "{out}"
    );
    assert!(
        context.contains("$ANY_KEY") && !context.contains("SWEEP_KEY"),
        "{context}"
    );
    assert_eq!(direct, &["ANY_KEY"]);
    let (out, context, direct) = &seen[1];
    assert!(
        out.contains("ANY_KEY:set") && out.contains("SWEEP_KEY:set"),
        "{out}"
    );
    assert!(context.contains("$SWEEP_KEY"), "{context}");
    assert_eq!(direct, &["ANY_KEY", "SWEEP_KEY"]);
    for (out, context, _) in &seen {
        for v in [ANY, SWEEP] {
            assert!(
                !out.contains(v) && !context.contains(v),
                "{v} reached Claude Code"
            );
        }
    }
}

/// A machine connected to the engine in-process (the server's socket left
/// out): what the node asks goes to `Engine::node_request`, what the engine
/// sends it arrives as `LinkEvent`s.
struct EngineLink {
    engine: tessera_core::Engine,
    conn: std::sync::Arc<tessera_core::engine::NodeConn>,
}

impl tessera_core::node::Link for EngineLink {
    fn call(&self, req: Request) -> futures::future::BoxFuture<'static, Result<Value, ApiError>> {
        let (engine, conn) = (self.engine.clone(), self.conn.clone());
        Box::pin(async move { engine.node_request(req, &conn).await })
    }
}

/// The `machines` tool on a real machine: a key kept for one bot and one
/// machine reaches that bot's commands there and nowhere else, and its value
/// never comes back, not even from the machine's own files.
#[tokio::test]
async fn keys_reach_a_machine_only_for_who_and_where_they_are_for() {
    use tessera_core::node::{LinkEvent, Node, NodeConfig};
    if !tessera_core::sandbox::confines_writes() {
        eprintln!(
            "machine commands need Landlock; sandbox here: {}",
            tessera_core::sandbox::describe()
        );
        return;
    }
    let h = harness(vec![], 100_000).await;
    // The machine: its own Tessera home, a work folder it allows.
    let mdir = tempfile::tempdir().unwrap();
    let root = mdir.path().canonicalize().unwrap();
    let mpaths = tessera_core::config::Paths::at(root.join("node-home"));
    mpaths.ensure().unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut c = NodeConfig::load(&mpaths).unwrap();
    c.enabled = true;
    c.name = "Desktop".into();
    c.generation = 1;
    c.scope = MachineScope {
        folders: vec![FolderAccess {
            path: work.display().to_string(),
            write: true,
        }],
        commands: true,
        ask: MachineAsk::Chat,
        timeout_secs: 60,
        ..Default::default()
    };
    c.save(&mpaths).unwrap();
    let (conn, mut from_engine) = h.engine.node_conn("dev_desktop");
    let link = std::sync::Arc::new(EngineLink {
        engine: h.engine.clone(),
        conn: std::sync::Arc::new(conn),
    });
    let (to_node, events) = tokio::sync::mpsc::unbounded_channel();
    let node = Node::open_at(mpaths.clone(), Some(root.clone())).unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    tokio::spawn(node.serve(link, events, stop.clone()));
    {
        let to_node = to_node.clone();
        tokio::spawn(async move {
            while let Some(e) = from_engine.recv().await {
                if to_node.send(LinkEvent::Event(e)).is_err() {
                    break;
                }
            }
        });
    }
    to_node
        .send(LinkEvent::Up(Some("in-process".into())))
        .unwrap();
    let mut online = false;
    for _ in 0..200 {
        let list: Vec<MachineInfo> =
            serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap())
                .unwrap();
        if list.iter().any(|m| m.online) {
            online = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(online, "the machine came online");

    let sentinel = save_bot(&h, bot("Sentinel")).await;
    let only = Some(Some(vec![sentinel.id.clone()]));
    // For Sentinel, on Desktop only; for Sentinel, on another machine; and
    // one any chat may use, on this server only.
    for (name, value, machines) in [
        ("LAB_KEY", LAB, vec!["dev_desktop".to_string()]),
        ("SWEEP_KEY", SWEEP, vec!["dev_other".to_string()]),
    ] {
        save(
            &h,
            name,
            SecretKind::Token,
            &[("value", value)],
            only.clone(),
            Some(false),
            Some(machines),
            None,
        )
        .await
        .unwrap();
    }
    save(
        &h,
        "ANY_KEY",
        SecretKind::Token,
        &[("value", ANY)],
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let run_there = |command: String| json!({"action": "run", "machine": "Desktop", "cwd": work.display().to_string(), "command": command});
    let probe_cmd = |vars: &[&str]| probe(vars)["command"].as_str().unwrap().to_string();

    // Sentinel on Desktop: its Desktop key, nothing else; the value, even
    // encoded, never comes back.
    let s_chat = chat(&h, Some(&sentinel), false).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_there(format!(
            "{}; echo $LAB_KEY; printf %s \"$LAB_KEY\" | base64",
            probe_cmd(&["LAB_KEY"])
        )),
    )]));
    // The key for another machine (not connected) is refused here.
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_there("echo $SWEEP_KEY".into()),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_there(probe_cmd(&["ANY_KEY"])),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s_chat.id, "MACHINE-SENTINEL check the switch").await;
    assert!(
        asked.is_empty(),
        "its own key, and Desktop follows the chat: {asked:?}"
    );
    let results = tool_results(&h.entries(&s_chat.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("LAB_KEY:set"),
        "{results:?}"
    );
    assert!(
        results[1].1 && results[1].2.contains("$SWEEP_KEY can't be used"),
        "{results:?}"
    );
    // An any-chat key kept to this server: refused there, saying where.
    assert!(
        results[2].1
            && results[2]
                .2
                .contains("$ANY_KEY can't be used on Desktop, only on this server"),
        "{results:?}"
    );

    // An ordinary chat on the same machine: nothing of Sentinel's.
    let normal = chat(&h, None, false).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        run_there(probe_cmd(&["LAB_KEY", "SWEEP_KEY"])),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &normal.id, "MACHINE-NORMAL try").await;
    let out = tool_results(&h.entries(&normal.id).await).remove(0).2;
    assert!(
        out.contains("LAB_KEY:unset") && out.contains("SWEEP_KEY:unset"),
        "{out}"
    );

    // Nowhere: not the chats, the models, or the machine's own files.
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(LAB);
    let kept = format!(
        "{}{}",
        serde_json::to_string(&h.entries(&s_chat.id).await).unwrap(),
        serde_json::to_string(&h.mock.agent_requests()).unwrap()
    );
    assert!(
        !kept.contains(LAB) && !kept.contains(&b64[..12]),
        "the value came back"
    );
    let mut files = vec![root.join("node-home")];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().flatten().map(|e| e.path()));
        } else if let Ok(bytes) = std::fs::read(&p) {
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(LAB), "{} keeps the value", p.display());
        }
    }
    stop.cancel();
}

/// Records what a machine sends back (no scrubbing on this side).
struct Recorder {
    sent: parking_lot::Mutex<Vec<String>>,
    replies: tokio::sync::mpsc::UnboundedSender<NodeResult>,
}

impl tessera_core::node::Link for Recorder {
    fn call(&self, req: Request) -> futures::future::BoxFuture<'static, Result<Value, ApiError>> {
        self.sent.lock().push(serde_json::to_string(&req).unwrap());
        let answer = match req {
            Request::NodeHello { .. } => {
                Ok(json!({"machine_id": "dev1", "paused": false, "server": "Home"}))
            }
            Request::NodeReply { result, .. } => {
                let _ = self.replies.send(result);
                Ok(Value::Null)
            }
            _ => Ok(Value::Null),
        };
        Box::pin(async move { answer })
    }
}

/// The machine itself sets the keys for that one command only and keeps
/// their values to itself: what it sends back and what it logs are
/// scrubbed there, whatever the server would do.
#[tokio::test]
async fn a_machine_scrubs_the_keys_it_was_given() {
    use tessera_core::node::{LinkEvent, Node, NodeConfig};
    if !tessera_core::sandbox::confines_writes() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths = tessera_core::config::Paths::at(root.join("node-home"));
    paths.ensure().unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut c = NodeConfig::load(&paths).unwrap();
    c.enabled = true;
    c.name = "Desktop".into();
    c.generation = 1;
    c.scope = MachineScope {
        folders: vec![FolderAccess {
            path: work.display().to_string(),
            write: true,
        }],
        commands: true,
        ask: MachineAsk::Chat,
        timeout_secs: 60,
        ..Default::default()
    };
    c.save(&paths).unwrap();
    let (rtx, mut replies) = tokio::sync::mpsc::unbounded_channel();
    let link = std::sync::Arc::new(Recorder {
        sent: Default::default(),
        replies: rtx,
    });
    let (events, rx) = tokio::sync::mpsc::unbounded_channel();
    let stop = tokio_util::sync::CancellationToken::new();
    let node = Node::open_at(paths.clone(), Some(root.clone())).unwrap();
    tokio::spawn(node.serve(link.clone(), rx, stop.clone()));
    events.send(LinkEvent::Up(Some("test".into()))).unwrap();
    let call = |id: &str, command: &str, env: &[(&str, &str)]| {
        LinkEvent::Event(Event::NodeCall {
            call: NodeCall {
                call_id: id.into(),
                action: NodeAction::Run {
                    command: command.into(),
                    cwd: work.display().to_string(),
                    timeout_secs: None,
                    admin: false,
                },
                session_id: "s1".into(),
                chat: "Sweep".into(),
                mode: PermissionMode::Default,
                approved: false,
                bot: None,
                env: SecretEnv(
                    env.iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                ),
                stops: None,
                files_only: false,
            },
        })
    };
    // Printed plainly, base64'd and as hex: none of it comes back.
    events
        .send(call(
            "c1",
            "echo got:$LAB_KEY; printf %s \"$LAB_KEY\" | base64; printf %s \"$LAB_KEY\" | od -An -tx1 | tr -d ' \\n'; echo; [ -n \"$LD_PRELOAD\" ] && echo preload:set || echo preload:unset",
            &[("LAB_KEY", LAB), ("LD_PRELOAD", "/tmp/nothing.so")],
        ))
        .unwrap();
    let r = tokio::time::timeout(Duration::from_secs(20), replies.recv())
        .await
        .expect("the machine answers")
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert!(
        r.output.contains("got:") && r.output.contains("preload:unset"),
        "{}",
        r.output
    );
    // The next command doesn't inherit it.
    events
        .send(call(
            "c2",
            "[ -n \"$LAB_KEY\" ] && echo left || echo gone",
            &[],
        ))
        .unwrap();
    let r2 = tokio::time::timeout(Duration::from_secs(20), replies.recv())
        .await
        .expect("the machine answers")
        .unwrap();
    assert!(r2.output.contains("gone"), "{}", r2.output);
    stop.cancel();
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(LAB);
    let hex: String = LAB.bytes().map(|b| format!("{b:02x}")).collect();
    let sent = link.sent.lock().join("\n");
    for v in [LAB, &b64[..12], &hex] {
        assert!(!sent.contains(v), "{v} left the machine: {sent}");
    }
    let mut files = vec![root.join("node-home")];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().flatten().map(|e| e.path()));
        } else if let Ok(bytes) = std::fs::read(&p) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(LAB),
                "{} keeps the value",
                p.display()
            );
        }
    }
}

/// A machine called `name`, online in-process, allowing commands in its
/// own work folder.
struct OnlineMachine {
    root: std::path::PathBuf,
    work: std::path::PathBuf,
    stop: tokio_util::sync::CancellationToken,
    _dir: tempfile::TempDir,
}

async fn online_machine(h: &Harness, device: &str, name: &str) -> OnlineMachine {
    use tessera_core::node::{LinkEvent, Node, NodeConfig};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mpaths = tessera_core::config::Paths::at(root.join("node-home"));
    mpaths.ensure().unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut c = NodeConfig::load(&mpaths).unwrap();
    c.enabled = true;
    c.name = name.into();
    c.generation = 1;
    c.scope = MachineScope {
        folders: vec![FolderAccess {
            path: work.display().to_string(),
            write: true,
        }],
        commands: true,
        ask: MachineAsk::Chat,
        timeout_secs: 60,
        ..Default::default()
    };
    c.save(&mpaths).unwrap();
    let (conn, mut from_engine) = h.engine.node_conn(device);
    let link = std::sync::Arc::new(EngineLink {
        engine: h.engine.clone(),
        conn: std::sync::Arc::new(conn),
    });
    let (to_node, events) = tokio::sync::mpsc::unbounded_channel();
    let node = Node::open_at(mpaths.clone(), Some(root.clone())).unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    tokio::spawn(node.serve(link, events, stop.clone()));
    {
        let to_node = to_node.clone();
        tokio::spawn(async move {
            while let Some(e) = from_engine.recv().await {
                if to_node.send(LinkEvent::Event(e)).is_err() {
                    break;
                }
            }
        });
    }
    to_node
        .send(LinkEvent::Up(Some("in-process".into())))
        .unwrap();
    for _ in 0..200 {
        let list: Vec<MachineInfo> =
            serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap())
                .unwrap();
        if list.iter().any(|m| m.id == device && m.online) {
            return OnlineMachine {
                root,
                work,
                stop,
                _dir: dir,
            };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{name} didn't come online");
}

async fn set_machines(
    h: &Harness,
    mode: SecretMachines,
    picked: Option<Vec<String>>,
) -> SecurityInfo {
    let v = h
        .engine
        .handle(
            Request::SetSecurity {
                sandbox: None,
                folders: None,
                ask_outside: None,
                redact_patterns: None,
                save_secrets_from_chat: None,
                secret_saving: None,
                secret_machines: Some(mode),
                secret_machines_picked: picked,
                bot_handoff: None,
                default_permission: None,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

/// "Let saved passwords be used on my machines": off by default (only a
/// password that picks a machine reaches it), then the machines picked, or
/// all of them; a password that chose for itself keeps its choice; the
/// machine's own rules still decide what runs there; and the value never
/// comes back, nor is it written anywhere on either side.
#[tokio::test]
async fn the_setting_for_all_passwords_lets_them_onto_machines() {
    if !tessera_core::sandbox::confines_writes() {
        eprintln!("machine commands need Landlock");
        return;
    }
    let h = harness(vec![], 100_000).await;
    let m = online_machine(&h, "dev_desktop", "Desktop").await;
    // Follows the setting / this server only, whatever the setting.
    for (name, value, machines_default) in
        [("PLAIN_KEY", ANY, None), ("HOME_KEY", PASS, Some(false))]
    {
        h.engine
            .handle(
                Request::SaveSecret {
                    name: name.into(),
                    kind: SecretKind::Token,
                    description: String::new(),
                    fields: [("value".to_string(), value.to_string())].into(),
                    remove_fields: vec![],
                    ask: true,
                    hosts: vec![],
                    bots: None,
                    on_server: None,
                    on_machines: None,
                    new_name: None,
                    machines_default,
                },
                None,
            )
            .await
            .unwrap();
    }
    let list: Vec<SecretInfo> =
        serde_json::from_value(h.engine.handle(Request::ListSecrets, None).await.unwrap()).unwrap();
    let info = |name: &str| list.iter().find(|s| s.name == name).unwrap().clone();
    assert!(info("PLAIN_KEY").machines_default);
    assert!(!info("HOME_KEY").machines_default);
    let sec: SecurityInfo =
        serde_json::from_value(h.engine.handle(Request::GetSecurity, None).await.unwrap()).unwrap();
    assert_eq!(sec.secret_machines, SecretMachines::Off, "off by default");

    let cwd = m.work.display().to_string();
    let there = |command: String| json!({"action": "run", "machine": "Desktop", "cwd": cwd, "command": command});
    let probe_cmd = |vars: &[&str]| probe(vars)["command"].as_str().unwrap().to_string();
    // One turn: the command, then "done"; the result of that command.
    let try_it = |label: &'static str, command: String| {
        let h = &h;
        let call = there(command);
        async move {
            let s = chat(h, None, false).await;
            h.mock.push(Reply::Tools(vec![("machines".into(), call)]));
            h.mock.push(Reply::Text("done".into()));
            run(h, &s.id, label).await;
            tool_results(&h.entries(&s.id).await).remove(0)
        }
    };

    // Off: refused, saying where it works and how the user can allow it.
    let r = try_it("MACHINES-OFF", probe_cmd(&["PLAIN_KEY"])).await;
    assert!(
        r.1 && r
            .2
            .contains("$PLAIN_KEY can't be used on Desktop, only on this server")
            && r.2
                .contains("Settings → Privacy & safety → Passwords & keys"),
        "{r:?}"
    );
    // The agent is told it works on this server only.
    let t = told(&h, "MACHINES-OFF");
    assert!(t.contains("PLAIN_KEY") && !t.contains(ANY), "{t}");

    // Only machines I pick: another one picked, then Desktop.
    let other = online_machine(&h, "dev_nas", "nas").await;
    let s = set_machines(
        &h,
        SecretMachines::Picked,
        Some(vec!["dev_nas".into(), "gone".into()]),
    )
    .await;
    assert_eq!(
        s.secret_machines_picked,
        ["dev_nas"],
        "unknown machines aren't kept"
    );
    let r = try_it("MACHINES-PICKED-NAS", probe_cmd(&["PLAIN_KEY"])).await;
    assert!(r.1 && r.2.contains("or on nas (`machines` run)"), "{r:?}");
    set_machines(&h, SecretMachines::Picked, Some(vec!["dev_desktop".into()])).await;
    let r = try_it(
        "MACHINES-PICKED-DESKTOP",
        format!(
            "{}; echo $PLAIN_KEY; printf %s \"$PLAIN_KEY\" | base64",
            probe_cmd(&["PLAIN_KEY"])
        ),
    )
    .await;
    assert!(!r.1 && r.2.contains("PLAIN_KEY:set"), "{r:?}");

    // All my machines: there too; the one kept to this server is not.
    set_machines(&h, SecretMachines::All, None).await;
    let r = try_it("MACHINES-ALL", probe_cmd(&["PLAIN_KEY"])).await;
    assert!(!r.1 && r.2.contains("PLAIN_KEY:set"), "{r:?}");
    let r = try_it("MACHINES-ALL-HOME", probe_cmd(&["HOME_KEY"])).await;
    assert!(
        r.1 && r
            .2
            .contains("$HOME_KEY can't be used on Desktop, only on this server"),
        "{r:?}"
    );
    // The machine's own rules still decide: outside its folders, nothing runs.
    let s = chat(&h, None, false).await;
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "run", "machine": "Desktop", "cwd": m.root.join("node-home").display().to_string(), "command": "echo $PLAIN_KEY"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &s.id, "MACHINES-REFUSED").await;
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        r.1 && r.2.contains("outside the folders Desktop allows"),
        "{r:?}"
    );

    // A machine taken away is forgotten by the setting.
    set_machines(
        &h,
        SecretMachines::Picked,
        Some(vec!["dev_desktop".into(), "dev_nas".into()]),
    )
    .await;
    h.engine
        .handle(
            Request::RevokeMachine {
                id: "dev_nas".into(),
            },
            None,
        )
        .await
        .unwrap();
    set_machines(&h, SecretMachines::Picked, None).await;
    let s: SecurityInfo =
        serde_json::from_value(h.engine.handle(Request::GetSecurity, None).await.unwrap()).unwrap();
    assert_eq!(s.secret_machines_picked, ["dev_desktop"]);

    // Nowhere: not the chats, the models, the machine's files, nor the
    // server's own (except the password store itself).
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(ANY);
    let mut seen = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    let sessions: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    for s in &sessions {
        seen.push_str(&serde_json::to_string(&h.entries(&s.id).await).unwrap());
    }
    assert!(
        !seen.contains(ANY) && !seen.contains(&b64[..12]),
        "the value came back"
    );
    m.stop.cancel();
    other.stop.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut files = vec![m.root.join("node-home"), h.dir.path().to_path_buf()];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().flatten().map(|e| e.path()));
        } else if p.file_name().is_some_and(|n| n != "auth.json")
            && let Ok(bytes) = std::fs::read(&p)
        {
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(ANY), "{} keeps the value", p.display());
        }
    }
}

/// A machine played by the test (no sandbox needed): it records what the
/// server sends and answers. `reply` gives the output for a command and
/// whether the machine refuses it; a misbehaving one may echo a value.
async fn played_machine(
    h: &Harness,
    device: &str,
    name: &str,
    commands: bool,
    reply: fn(&NodeCall) -> (String, bool),
) -> std::sync::Arc<parking_lot::Mutex<Vec<NodeCall>>> {
    let (conn, mut rx) = h.engine.node_conn(device);
    h.engine
        .node_request(
            Request::NodeHello {
                name: name.into(),
                os: "linux x86_64".into(),
                version: "0.1.0".into(),
                scope: MachineScope {
                    folders: vec![FolderAccess {
                        path: "/home/me/work".into(),
                        write: true,
                    }],
                    commands,
                    ask: MachineAsk::Chat,
                    timeout_secs: 60,
                    ..Default::default()
                },
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
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let (engine, keep) = (h.engine.clone(), seen.clone());
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                keep.lock().push(call.clone());
                let (output, refused) = reply(&call);
                let _ = engine
                    .node_request(
                        Request::NodeReply {
                            call_id: call.call_id,
                            result: NodeResult {
                                output,
                                ok: !refused,
                                refused,
                                exit_code: (!refused).then_some(0),
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

/// The same rules without a real machine (runs on any kernel): which
/// passwords go out with a command, to which machine, and that a value a
/// machine sends back never reaches a chat or a model.
#[tokio::test]
async fn which_passwords_go_to_which_machine() {
    let h = harness(vec![], 100_000).await;
    // Desktop runs commands, and (misbehaving) echoes every value it got; nas
    // refuses anything in /home/me/work/private; the lab has commands off.
    let desktop = played_machine(&h, "dev_desktop", "Desktop", true, |c| {
        (
            format!(
                "ran with {}",
                c.env
                    .0
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            false,
        )
    })
    .await;
    let nas = played_machine(&h, "dev_nas", "nas", true, |c| match &c.action {
        NodeAction::Run { cwd, .. } if cwd.ends_with("/private") => {
            ("that folder is private here".into(), true)
        }
        _ => ("ran it".into(), false),
    })
    .await;
    let lab = played_machine(&h, "dev_lab", "lab", false, |_| ("ran it".into(), false)).await;
    for (name, value, on_machines, machines_default) in [
        ("PLAIN_KEY", ANY, None, None),
        ("HOME_KEY", PASS, None, Some(false)),
        ("LAB_KEY", LAB, Some(vec!["dev_nas".to_string()]), None),
    ] {
        h.engine
            .handle(
                Request::SaveSecret {
                    name: name.into(),
                    kind: SecretKind::Token,
                    description: String::new(),
                    fields: [("value".to_string(), value.to_string())].into(),
                    remove_fields: vec![],
                    ask: false,
                    hosts: vec![],
                    bots: None,
                    on_server: None,
                    on_machines,
                    new_name: None,
                    machines_default,
                },
                None,
            )
            .await
            .unwrap();
    }
    let sent = |m: &std::sync::Arc<parking_lot::Mutex<Vec<NodeCall>>>| -> Vec<Vec<String>> {
        m.lock()
            .iter()
            .map(|c| c.env.0.keys().cloned().collect())
            .collect()
    };
    let turn =
        |label: &'static str, machine: &'static str, cwd: &'static str, command: &'static str| {
            let h = &h;
            async move {
                let s = chat(h, None, false).await;
                h.mock.push(Reply::Tools(vec![(
                    "machines".into(),
                    json!({"action": "run", "machine": machine, "cwd": cwd, "command": command}),
                )]));
                h.mock.push(Reply::Text("done".into()));
                run(h, &s.id, label).await;
                tool_results(&h.entries(&s.id).await).remove(0)
            }
        };
    let work = "/home/me/work";

    // Off: the one that follows the setting goes nowhere; nothing is sent.
    let r = turn("PLAYED-OFF", "Desktop", work, "curl -u x:$PLAIN_KEY lan").await;
    assert!(
        r.1 && r.2.contains("$PLAIN_KEY can't be used on Desktop"),
        "{r:?}"
    );
    assert!(desktop.lock().is_empty(), "nothing went to Desktop");
    // A password's own machine: only there, only it.
    let r = turn("PLAYED-OWN", "nas", work, "echo $LAB_KEY $PLAIN_KEY_NOT").await;
    assert!(!r.1, "{r:?}");
    assert_eq!(sent(&nas), [vec!["LAB_KEY".to_string()]]);
    let r = turn("PLAYED-OWN-ELSEWHERE", "Desktop", work, "echo $LAB_KEY").await;
    assert!(r.1 && r.2.contains("or on nas (`machines` run)"), "{r:?}");

    // All my machines: Desktop gets it; the value it echoes is scrubbed.
    set_machines(&h, SecretMachines::All, None).await;
    let r = turn("PLAYED-ALL", "Desktop", work, "curl -u x:$PLAIN_KEY lan").await;
    assert!(
        !r.1 && r.2.contains("ran with PLAIN_KEY=") && !r.2.contains(ANY),
        "{r:?}"
    );
    assert_eq!(sent(&desktop), [vec!["PLAIN_KEY".to_string()]]);
    assert_eq!(
        desktop.lock()[0].env.0["PLAIN_KEY"],
        ANY,
        "the machine got the value"
    );
    // One kept to this server: still refused there.
    let r = turn("PLAYED-HOME", "Desktop", work, "echo $HOME_KEY").await;
    assert!(
        r.1 && r.2.contains("$HOME_KEY can't be used on Desktop"),
        "{r:?}"
    );
    assert_eq!(desktop.lock().len(), 1, "nothing more went to Desktop");
    // A command that names none gets none.
    turn("PLAYED-NONE", "Desktop", work, "uptime").await;
    assert_eq!(sent(&desktop)[1], Vec::<String>::new());

    // The machine still decides: commands off there (refused before
    // anything is sent), or the machine itself refusing.
    let r = turn("PLAYED-LAB", "lab", work, "echo $PLAIN_KEY").await;
    assert!(r.1 && r.2.contains("Commands are off on lab"), "{r:?}");
    assert!(lab.lock().is_empty(), "nothing went to the lab");
    let r = turn(
        "PLAYED-REFUSED",
        "nas",
        "/home/me/work/private",
        "echo $PLAIN_KEY",
    )
    .await;
    assert!(r.1 && r.2.contains("nas refused this"), "{r:?}");

    // Only machines picked.
    set_machines(&h, SecretMachines::Picked, Some(vec!["dev_nas".into()])).await;
    let r = turn("PLAYED-PICKED", "Desktop", work, "echo $PLAIN_KEY").await;
    assert!(r.1 && r.2.contains("can't be used on Desktop"), "{r:?}");
    assert_eq!(desktop.lock().len(), 2, "nothing more went to Desktop");

    // The value is in no chat and no request to a model.
    let mut seen = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    let sessions: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    for s in &sessions {
        seen.push_str(&serde_json::to_string(&h.entries(&s.id).await).unwrap());
    }
    for v in [ANY, PASS, LAB] {
        assert!(!seen.contains(v), "{v} came back");
    }
}

const GITHUB: &str = "ghp_Zq7d9Kx2Lm4Np6Rs8Tv0Wx3Yz5Ab7Cd9Ef1G";

#[tokio::test]
async fn a_bots_own_choice_of_open_passwords() {
    let h = harness(vec![], 100_000).await;
    let stored = async |b: &Bot| -> Option<BotSecrets> {
        let all: Vec<Bot> =
            serde_json::from_value(h.engine.handle(Request::ListBots, None).await.unwrap())
                .unwrap();
        all.into_iter().find(|x| x.id == b.id).unwrap().secrets
    };
    // A new bot saved without the choice: none of the open ones.
    let nova = save_bot(
        &h,
        Bot {
            secrets: None,
            ..bot("Nova")
        },
    )
    .await;
    assert_eq!(nova.secrets, Some(BotSecrets::default()));
    // An edit from an app that doesn't send it keeps it.
    let ticked = BotSecrets {
        default: false,
        each: [("OPEN_KEY".to_string(), true)].into(),
    };
    let nova = save_bot(
        &h,
        Bot {
            secrets: Some(ticked.clone()),
            ..nova
        },
    )
    .await;
    let nova = save_bot(
        &h,
        Bot {
            secrets: None,
            role: "a new role".into(),
            ..nova
        },
    )
    .await;
    assert_eq!(nova.secrets, Some(ticked));
    // Renamed, the choice follows the password; deleted, it goes.
    save(
        &h,
        "OPEN_KEY",
        SecretKind::Token,
        &[("value", ANY)],
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    save(
        &h,
        "OPEN_KEY",
        SecretKind::Token,
        &[],
        None,
        None,
        None,
        Some("HOME_KEY"),
    )
    .await
    .unwrap();
    assert_eq!(
        stored(&nova).await.unwrap().each,
        [("HOME_KEY".to_string(), true)].into()
    );
    h.engine
        .handle(
            Request::DeleteSecret {
                name: "HOME_KEY".into(),
            },
            None,
        )
        .await
        .unwrap();
    assert!(stored(&nova).await.unwrap().each.is_empty());
    // A key typed in its own chat and saved for every chat is the bot's to
    // use, though it gets none of the others.
    let s = chat(&h, Some(&nova), false).await;
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, &format!("here is the token {GITHUB}")).await;
    let saved: Vec<SecretInfo> =
        serde_json::from_value(h.engine.handle(Request::ListSecrets, None).await.unwrap()).unwrap();
    let new = saved
        .iter()
        .find(|x| x.bots.is_none() && x.chat.is_none())
        .expect("saved for every chat");
    assert_eq!(
        stored(&nova).await.unwrap().each,
        [(new.name.clone(), true)].into()
    );
    let entries = serde_json::to_string(&h.entries(&s.id).await).unwrap();
    assert!(entries.contains(&format!("${}", new.name)), "{entries}");
    assert!(!entries.contains(GITHUB));
}
