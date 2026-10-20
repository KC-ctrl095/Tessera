//! Helper limits per chat (`set_chat_helpers`): how many helpers work at
//! once in each chat, how long each may work, "no limit", the server's
//! capacity over all chats, and helpers waiting their turn shown as queued
//! (`SessionInfo::helper_queued`), also across a restart.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

const SLOW: Duration = Duration::from_secs(60);

/// Model requests made by helpers whose task contains `key`.
fn started(mock: &Mock, key: &str) -> usize {
    mock.state
        .requests
        .lock()
        .iter()
        .filter(|r| {
            r["messages"]
                .as_array()
                .and_then(|m| m.iter().find(|m| m["role"] == "user"))
                .is_some_and(|m| m["content"].to_string().contains(key))
        })
        .count()
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !ok() {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
}

async fn info(engine: &Engine, id: &str) -> SessionInfo {
    let v = engine
        .handle(
            Request::GetSession {
                session_id: id.into(),
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value::<SessionSnapshot>(v).unwrap().info
}

async fn set_chat(engine: &Engine, id: &str, limits: HelperLimits) -> SessionInfo {
    let v = engine
        .handle(
            Request::SetChatHelpers {
                session_id: id.into(),
                limits,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

/// The parent's two scripted replies: start these helpers in the
/// background, then say so.
fn start(h: &Harness, tasks: &[(&str, &str)]) {
    let tasks: Vec<Value> = tasks
        .iter()
        .map(|(name, prompt)| json!({"name": name, "prompt": prompt}))
        .collect();
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait": false, "tasks": tasks}),
    )]));
    h.mock.push(Reply::Text("They are working.".into()));
}

/// The helper of `chat` named `name`.
fn kid(h: &Harness, chat: &str, name: &str) -> SessionInfo {
    h.engine
        .store()
        .children_of(chat)
        .unwrap()
        .into_iter()
        .find(|k| k.title == format!("↳ {name}"))
        .unwrap_or_else(|| panic!("no helper {name}"))
}

async fn stop(engine: &Engine, id: &str) {
    engine
        .handle(
            Request::Abort {
                session_id: id.into(),
            },
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn each_chat_has_its_own_limit_and_waiting_helpers_show_as_queued() {
    // The server's default: one helper at a time in a chat.
    let h = harness_agent(vec![], 100_000, "max_subagents = 1").await;
    for key in ["<A0>", "<A1>", "<A2>", "<A3>", "<B0>", "<B1>"] {
        h.mock.route(key, Reply::Slow(SLOW, "done".into()));
    }
    let a = h.session().await;
    let b = h.session().await;
    // Chat A takes three at once; old apps still see a plain running chat.
    let set = set_chat(
        &h.engine,
        &a.id,
        HelperLimits {
            at_once: Some(3),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(set.helper_limits.as_ref().unwrap().at_once, Some(3));
    start(
        &h,
        &[
            ("a0", "<A0>"),
            ("a1", "<A1>"),
            ("a2", "<A2>"),
            ("a3", "<A3>"),
        ],
    );
    h.send(&a.id, "Split the audit four ways.").await;
    h.settle(&a.id).await;
    until("three of chat A's helpers working", || {
        started(&h.mock, "<A") == 3
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(started(&h.mock, "<A"), 3, "a fourth waits its turn");
    let i = (0..4)
        .find(|i| started(&h.mock, &format!("<A{i}>")) == 0)
        .expect("one waiting helper");
    let fourth = kid(&h, &a.id, &format!("a{i}"));
    let waiting = info(&h.engine, &fourth.id).await;
    assert!(waiting.helper_queued, "shown as queued");
    assert_eq!(waiting.status, SessionStatus::Running, "busy for old apps");

    // Chat B keeps the server's default (1), whatever chat A runs.
    start(&h, &[("b0", "<B0>"), ("b1", "<B1>")]);
    h.send(&b.id, "Check both halves.").await;
    h.settle(&b.id).await;
    until("chat B's first helper working", || {
        started(&h.mock, "<B") == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(started(&h.mock, "<B"), 1, "chat B: one at a time");
    assert_eq!(started(&h.mock, "<A"), 3, "chat A unchanged");

    // No limit for chat A: its fourth starts at once, no longer queued.
    set_chat(
        &h.engine,
        &a.id,
        HelperLimits {
            at_once: Some(0),
            ..Default::default()
        },
    )
    .await;
    until("chat A's fourth helper working", || {
        started(&h.mock, "<A") == 4
    })
    .await;
    until("no longer shown queued", || {
        h.engine
            .store()
            .get_session(&fourth.id)
            .unwrap()
            .is_some_and(|(i, _)| !i.helper_queued)
    })
    .await;
    // Chat B's own number: two at once.
    set_chat(
        &h.engine,
        &b.id,
        HelperLimits {
            at_once: Some(2),
            ..Default::default()
        },
    )
    .await;
    until("chat B's second helper working", || {
        started(&h.mock, "<B") == 2
    })
    .await;
    // Back to the server's default: an empty set clears the chat's own.
    let back = set_chat(&h.engine, &b.id, HelperLimits::default()).await;
    assert!(back.helper_limits.is_none());
    // A helper can't set limits for itself.
    let err = h
        .engine
        .handle(
            Request::SetChatHelpers {
                session_id: fourth.id.clone(),
                limits: HelperLimits::default(),
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        err.message.contains("can't start helpers"),
        "{}",
        err.message
    );
    // Ordinary whole numbers have no preset-derived ceiling.
    let large = set_chat(
        &h.engine,
        &a.id,
        HelperLimits {
            steps: Some(100_001),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(large.helper_limits.unwrap().steps, Some(100_001));
    stop(&h.engine, &a.id).await;
    stop(&h.engine, &b.id).await;
}

#[tokio::test]
async fn the_servers_capacity_holds_every_chat_whatever_it_chose() {
    let h = harness_agent(vec![], 100_000, "max_subagents = 4\nhelper_capacity = 2").await;
    for key in ["<C0>", "<C1>", "<D0>"] {
        h.mock.route(key, Reply::Slow(SLOW, "done".into()));
    }
    let settings: HelperSettings =
        serde_json::from_value(h.engine.handle(Request::GetHelpers, None).await.unwrap()).unwrap();
    assert_eq!(settings.capacity, Some(2));
    let c = h.session().await;
    let d = h.session().await;
    set_chat(
        &h.engine,
        &d.id,
        HelperLimits {
            at_once: Some(0),
            ..Default::default()
        },
    )
    .await;
    start(&h, &[("c0", "<C0>"), ("c1", "<C1>")]);
    h.send(&c.id, "Two things at once.").await;
    h.settle(&c.id).await;
    until("chat C's two helpers working", || {
        started(&h.mock, "<C") == 2
    })
    .await;
    // Chat D has no limit of its own, but the server is full.
    start(&h, &[("d0", "<D0>")]);
    h.send(&d.id, "One more thing.").await;
    h.settle(&d.id).await;
    let d0 = kid(&h, &d.id, "d0");
    until("chat D's helper shown queued", || {
        h.engine
            .store()
            .get_session(&d0.id)
            .unwrap()
            .is_some_and(|(i, _)| i.helper_queued)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(started(&h.mock, "<D0>"), 0, "it waits for room");
    // Chat C stops: room again, and D's helper starts.
    stop(&h.engine, &c.id).await;
    until("chat D's helper working", || started(&h.mock, "<D0>") == 1).await;
    stop(&h.engine, &d.id).await;
}

#[tokio::test]
async fn steps_and_minutes_per_chat_including_no_limit() {
    // The server's default: one tool call per helper.
    let h = harness_agent(vec![], 100_000, "subagent_steps = 1").await;
    let glob = || Reply::Tools(vec![("glob".into(), json!({"pattern": "*"}))]);
    // Chat E: no step or time limit; its helper takes three steps freely.
    for _ in 0..3 {
        h.mock.route("<E0>", glob());
    }
    h.mock.route("<E0>", Reply::Text("Report E".into()));
    // Chat F: two steps, then it is asked for its report.
    for _ in 0..2 {
        h.mock.route("<F0>", glob());
    }
    h.mock.route("<F0>", Reply::Text("Report F".into()));
    for (name, own) in [("E", Some(0)), ("F", Some(2))] {
        let chat = h.session().await;
        set_chat(
            &h.engine,
            &chat.id,
            HelperLimits {
                steps: own,
                minutes: own.map(|_| 0),
                ..Default::default()
            },
        )
        .await;
        let key = format!("<{name}0>");
        h.mock.push(Reply::Tools(vec![(
            "agent".into(),
            json!({"wait": true, "tasks": [{"name": format!("{name}0"), "prompt": key}]}),
        )]));
        h.mock.push(Reply::Text("Thanks.".into()));
        h.send(&chat.id, "Look around.").await;
        h.settle(&chat.id).await;
        let helper = kid(&h, &chat.id, &format!("{name}0"));
        let entries = h.entries(&helper.id).await;
        let budget_told = entries.iter().any(|e| {
            matches!(&e.body, EntryBody::User { content, .. } if plain_text(content).contains("[Budget reached]"))
        });
        let steps = h.info(&helper.id).await.steps;
        match name {
            "E" => assert!(steps == 3 && !budget_told, "E: {steps} steps"),
            _ => assert!(steps == 2 && budget_told, "F: {steps} steps"),
        }
        let result = tool_results(&h.entries(&chat.id).await).remove(0);
        assert!(result.2.contains(&format!("Report {name}")), "{result:?}");
    }
}

#[tokio::test]
async fn a_chats_limits_and_its_queue_survive_a_restart() {
    let h = harness_agent(vec![], 100_000, "max_subagents = 4").await;
    for key in ["<R0>", "<R1>"] {
        h.mock.route(key, Reply::Slow(SLOW, "done".into()));
    }
    let chat = h.session().await;
    let limits = HelperLimits {
        at_once: Some(1),
        steps: Some(0),
        minutes: Some(90),
    };
    set_chat(&h.engine, &chat.id, limits.clone()).await;
    start(&h, &[("r0", "<R0>"), ("r1", "<R1>")]);
    h.send(&chat.id, "Two helpers, one at a time.").await;
    h.settle(&chat.id).await;
    until("one helper working", || started(&h.mock, "<R") == 1).await;
    // An update restarts the server while one works and one waits.
    h.engine.shutdown().await;
    for key in ["<R0>", "<R1>"] {
        h.mock.route(key, Reply::Slow(SLOW, "done".into()));
    }
    let before = started(&h.mock, "<R");
    // (`scheduler`: the process that continues what a restart cut off.)
    let engine = Engine::open(Paths::at(h.dir.path()), EngineOptions { scheduler: true })
        .await
        .unwrap();
    assert_eq!(info(&engine, &chat.id).await.helper_limits, Some(limits));
    let kids = engine.store().children_of(&chat.id).unwrap();
    assert_eq!(kids.len(), 2);
    // Both go on by themselves, still one at a time: the other shows queued.
    until("one helper working again", || {
        started(&h.mock, "<R") == before + 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(started(&h.mock, "<R"), before + 1);
    let mut queued = 0;
    for k in &kids {
        let i = info(&engine, &k.id).await;
        if i.helper_queued {
            queued += 1;
            assert_eq!(i.status, SessionStatus::Running);
        }
    }
    assert_eq!(queued, 1, "the other one waits, shown as queued");
    stop(&engine, &chat.id).await;
}

#[tokio::test]
async fn no_step_limit_outlasts_the_chats_four_round_ceiling() {
    let h = harness_agent(vec![], 100_000, "max_steps = 2").await;
    let chat = h.session().await;
    set_chat(
        &h.engine,
        &chat.id,
        HelperLimits {
            steps: Some(0),
            minutes: Some(0),
            ..Default::default()
        },
    )
    .await;
    for i in 0..10 {
        h.mock.route(
            "<LONG>",
            Reply::Tools(vec![(
                "glob".into(),
                json!({"pattern": format!("file{i}*")}),
            )]),
        );
    }
    h.mock
        .route("<LONG>", Reply::Text("All ten checks completed.".into()));
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait": true, "tasks": [{"name": "long", "prompt": "Check <LONG>"}]}),
    )]));
    h.mock.push(Reply::Text("Thanks.".into()));
    h.send(&chat.id, "Do all ten checks.").await;
    h.settle(&chat.id).await;
    let child = kid(&h, &chat.id, "long");
    assert_eq!(h.info(&child.id).await.steps, 10);
    assert!(h.entries(&child.id).await.iter().any(|e| matches!(&e.body,
        EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p,
            AssistantPart::Text { text } if text.contains("All ten checks completed."))))));
}

#[tokio::test]
async fn unlimited_helpers_still_wait_for_a_single_slot_model() {
    let mut h = harness(vec![], 100_000).await;
    h.engine.shutdown().await;
    let path = h.dir.path().join("config.toml");
    let config = std::fs::read_to_string(&path)
        .unwrap()
        .replace("parallel = 4", "parallel = 1");
    std::fs::write(path, config).unwrap();
    h.engine = Engine::open(Paths::at(h.dir.path()), EngineOptions::default())
        .await
        .unwrap();
    for key in ["<S0>", "<S1>"] {
        h.mock.route(key, Reply::Slow(SLOW, "done".into()));
    }
    let chat = h.session().await;
    set_chat(
        &h.engine,
        &chat.id,
        HelperLimits {
            at_once: Some(0),
            ..Default::default()
        },
    )
    .await;
    start(&h, &[("s0", "<S0>"), ("s1", "<S1>")]);
    h.send(&chat.id, "Delegate both checks.").await;
    until("one model request", || started(&h.mock, "<S") == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(started(&h.mock, "<S"), 1);
    let children = h.engine.store().children_of(&chat.id).unwrap();
    assert_eq!(children.iter().filter(|i| i.helper_queued).count(), 1);
    stop(&h.engine, &chat.id).await;
}

#[tokio::test]
async fn settings_tool_changes_only_this_chat_after_user_approval() {
    let h = harness(vec![], 100_000).await;
    let chat = h.session().await;
    let other = h.session().await;
    for (decision, expected) in [
        (ApprovalDecision::Deny, None),
        (ApprovalDecision::Allow, Some(0)),
    ] {
        h.mock.push(Reply::Tools(vec![(
            "settings".into(),
            json!({"action": "set", "key": "chat.helper_steps", "value": "no limit"}),
        )]));
        h.mock.push(Reply::Text("Done.".into()));
        let mut events = h.engine.subscribe();
        h.send(&chat.id, "Remove the helper step limit for this chat.")
            .await;
        let request = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Event::ApprovalRequested { request, .. } = events.recv().await.unwrap() {
                    break request;
                }
            }
        })
        .await
        .unwrap();
        assert!(info(&h.engine, &chat.id).await.helper_limits.is_none());
        h.engine
            .handle(
                Request::RespondApproval {
                    session_id: chat.id.clone(),
                    request_id: request.id,
                    decision,
                },
                None,
            )
            .await
            .unwrap();
        h.settle(&chat.id).await;
        assert_eq!(
            info(&h.engine, &chat.id)
                .await
                .helper_limits
                .and_then(|l| l.steps),
            expected
        );
        assert!(info(&h.engine, &other.id).await.helper_limits.is_none());
    }
    let defaults: HelperSettings =
        serde_json::from_value(h.engine.handle(Request::GetHelpers, None).await.unwrap()).unwrap();
    assert_eq!(defaults.steps, Some(40));
}

async fn native_step_batch(concurrent: bool) {
    let h = harness(vec![], 100_000).await;
    let chat = h.session().await;
    set_chat(
        &h.engine,
        &chat.id,
        HelperLimits {
            steps: Some(1),
            ..Default::default()
        },
    )
    .await;
    let calls = if concurrent {
        vec![
            ("glob".into(), json!({"pattern":"first*"})),
            ("glob".into(), json!({"pattern":"second*"})),
        ]
    } else {
        vec![
            ("write".into(), json!({"path":"first.txt", "content":"one"})),
            (
                "write".into(),
                json!({"path":"second.txt", "content":"two"}),
            ),
        ]
    };
    h.mock.route("<BATCH>", Reply::Tools(calls));
    h.mock.route(
        "<BATCH>",
        Reply::Text("Final report: one action ran.".into()),
    );
    start(&h, &[("batch", "<BATCH>")]);
    h.send(&chat.id, "Delegate the bounded task.").await;
    h.settle(&chat.id).await;
    let child = kid(&h, &chat.id, "batch");
    // The parent settles after launching background work, not after its
    // helpers finish. Wait for the terminal accounting boundary before
    // asserting the persisted budget and report.
    until("native helper budget closed", || {
        h.engine
            .store()
            .get_session(&child.id)
            .unwrap()
            .unwrap()
            .1
            .helper_budget
            .is_some_and(|b| b.closed)
    })
    .await;
    let entries = h.entries(&child.id).await;
    assert_eq!(h.info(&child.id).await.steps, 1);
    let (_, internal) = h.engine.store().get_session(&child.id).unwrap().unwrap();
    let budget = internal.helper_budget.unwrap();
    assert!(
        budget.closed && budget.calls.is_empty(),
        "terminal native helpers retain only the aggregate"
    );
    assert_eq!(budget.used, 1);
    let results: Vec<_> = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult {
                content, is_error, ..
            } => Some((content, is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results.iter().filter(|(_, error)| !**error).count(), 1);
    assert!(results.iter().any(|(c, e)| **e
        && c.iter().any(|p| matches!(p,
        ContentPart::Text { text } if text.contains("Budget exhausted; not executed")))));
    assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Assistant { content, .. }
        if content.iter().any(|p| matches!(p, AssistantPart::Text { text } if text.contains("Final report"))))));
    if !concurrent {
        assert!(h.work.join("first.txt").exists());
        assert!(!h.work.join("second.txt").exists());
    }
}

#[tokio::test]
async fn helper_budget27_concurrent_native_calls_consume_only_one_step() {
    native_step_batch(true).await;
}

#[tokio::test]
async fn helper_budget27_sequential_native_calls_consume_only_one_step() {
    native_step_batch(false).await;
}

#[tokio::test]
async fn disabled_defaults_round_trip_never_become_unlimited() {
    let h = harness_agent(vec![], 100_000, "max_subagents = 0").await;
    let get = |v| serde_json::from_value::<HelperSettings>(v).unwrap();
    let original = get(h.engine.handle(Request::GetHelpers, None).await.unwrap());
    assert_eq!((original.enabled, original.at_once), (Some(false), 0));
    for enabled in [Some(false), None] {
        let saved = get(h
            .engine
            .handle(
                Request::SetHelpers {
                    at_once: original.at_once,
                    enabled,
                    steps: Some(7),
                    minutes: Some(2),
                },
                None,
            )
            .await
            .unwrap());
        assert_eq!((saved.enabled, saved.at_once), (Some(false), 0));
        assert_eq!((saved.steps, saved.minutes), (Some(7), Some(2)));
        assert!(
            std::fs::read_to_string(h.dir.path().join("config.toml"))
                .unwrap()
                .contains("max_subagents = 0")
        );
    }
    let unlimited = get(h
        .engine
        .handle(
            Request::SetHelpers {
                at_once: 0,
                enabled: Some(true),
                steps: None,
                minutes: None,
            },
            None,
        )
        .await
        .unwrap());
    assert_eq!((unlimited.enabled, unlimited.at_once), (Some(true), 0));
}

#[tokio::test]
async fn built_in_role_cannot_widen_the_current_chat_steps() {
    let h = harness(vec![], 100_000).await;
    let chat = h.session().await;
    set_chat(
        &h.engine,
        &chat.id,
        HelperLimits {
            steps: Some(1),
            ..Default::default()
        },
    )
    .await;
    h.mock.route(
        "<ROLE>",
        Reply::Tools(vec![
            ("glob".into(), json!({"pattern":"one*"})),
            ("glob".into(), json!({"pattern":"two*"})),
        ]),
    );
    h.mock
        .route("<ROLE>", Reply::Text("Role final report.".into()));
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait":true,"tasks":[{"name":"role", "agent":"explore", "prompt":"Check <ROLE>"}]}),
    )]));
    h.mock.push(Reply::Text("Done.".into()));
    h.send(&chat.id, "Delegate the bounded exploration.").await;
    h.settle(&chat.id).await;
    assert_eq!(h.info(&kid(&h, &chat.id, "role").id).await.steps, 1);
}

#[tokio::test]
async fn server_disabled_holds_explicit_chat_limits_at_the_start_boundary() {
    for override_limit in [0, 2] {
        let h = harness_agent(vec![], 100_000, "max_subagents = 2\nhelper_capacity = 1").await;
        h.mock
            .route("<DISABLE0>", Reply::Slow(SLOW, "working".into()));
        h.mock
            .route("<DISABLE1>", Reply::Slow(SLOW, "queued".into()));
        let chat = h.session().await;
        set_chat(
            &h.engine,
            &chat.id,
            HelperLimits {
                at_once: Some(override_limit),
                ..Default::default()
            },
        )
        .await;
        start(&h, &[("d0", "<DISABLE0>"), ("d1", "<DISABLE1>")]);
        h.send(&chat.id, "Check both.").await;
        h.settle(&chat.id).await;
        until("one helper in global slot", || {
            started(&h.mock, "<DISABLE") == 1
        })
        .await;
        let working_name = if started(&h.mock, "<DISABLE0>") == 1 {
            "d0"
        } else {
            "d1"
        };
        let working = kid(&h, &chat.id, working_name);
        let waiting_name = if working_name == "d0" { "d1" } else { "d0" };
        let waiting = kid(&h, &chat.id, waiting_name);
        until("the other helper is queued", || {
            h.engine
                .store()
                .get_session(&waiting.id)
                .unwrap()
                .unwrap()
                .0
                .helper_queued
        })
        .await;
        h.engine
            .handle(
                Request::SetHelpers {
                    at_once: 0,
                    enabled: Some(false),
                    steps: None,
                    minutes: None,
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            info(&h.engine, &working.id).await.status,
            SessionStatus::Running
        );
        stop(&h.engine, &working.id).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            started(&h.mock, "<DISABLE"),
            1,
            "disabled wins over the chat override after global-slot wait"
        );
        assert!(info(&h.engine, &waiting.id).await.helper_queued);
        h.engine
            .handle(
                Request::SetHelpers {
                    at_once: 2,
                    enabled: Some(true),
                    steps: None,
                    minutes: None,
                },
                None,
            )
            .await
            .unwrap();
        until("reenable admits the waiting helper", || {
            started(&h.mock, "<DISABLE") == 2
        })
        .await;
        stop(&h.engine, &waiting.id).await;
    }
}
