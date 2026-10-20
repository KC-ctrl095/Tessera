//! The `desktop` tool: offered only while a machine that can use its
//! desktop has desktop control on, and each action goes the machines way
//! (the machine's "Ask first", the chat's mode, declining, the machine's own
//! say). The machine here is a stand-in on its own connection: it answers a
//! screenshot with a small picture and anything else "done", and keeps what
//! reached it.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use parking_lot::Mutex;
use serde_json::json;
use tessera_proto::*;

/// A 1×1 PNG, as a machine would send a screenshot.
const PIXEL: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

async fn machine(
    h: &Harness,
    device: &str,
    name: &str,
    scope: MachineScope,
    features: &[&str],
) -> Arc<Mutex<Vec<NodeCall>>> {
    machine_conn(h, device, name, scope, features).await.1
}

/// The same, with its connection (for its scope updates).
async fn machine_conn(
    h: &Harness,
    device: &str,
    name: &str,
    scope: MachineScope,
    features: &[&str],
) -> (
    Arc<tessera_core::engine::NodeConn>,
    Arc<Mutex<Vec<NodeCall>>>,
) {
    let (conn, mut rx) = h.engine.node_conn(device);
    let conn = Arc::new(conn);
    h.engine
        .node_request(
            Request::NodeHello {
                name: name.into(),
                os: "windows x86_64, shell Git Bash (bash)".into(),
                version: "0.1.0".into(),
                scope,
                generation: 1,
                sandbox: "off".into(),
                sandbox_ok: true,
                home: None,
                computer: None,
                host: None,
                features: Some(features.iter().map(|f| f.to_string()).collect()),
                guard_key: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, keep, mine) = (h.engine.clone(), seen.clone(), conn.clone());
    tokio::spawn(async move {
        let conn = mine;
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                keep.lock().push(call.clone());
                let image = matches!(
                    call.action,
                    NodeAction::Desktop {
                        desktop: DesktopOp::Screenshot { .. },
                        ..
                    }
                )
                .then(|| NodeImage {
                    mime: "image/png".into(),
                    data: PIXEL.into(),
                    width: 1,
                    height: 1,
                });
                let _ = engine
                    .node_request(
                        Request::NodeReply {
                            call_id: call.call_id,
                            result: NodeResult {
                                output: format!("did {}", call.action.target()),
                                ok: true,
                                refused: false,
                                exit_code: None,
                                image,
                            },
                        },
                        &conn,
                    )
                    .await;
            }
        }
    });
    (conn, seen)
}

async fn chat(h: &Harness, permission: Option<PermissionMode>) -> SessionInfo {
    let v = h
        .engine
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
        .unwrap();
    let s: SessionInfo = serde_json::from_value(v).unwrap();
    if let Some(mode) = permission {
        h.engine
            .handle(
                Request::UpdateSession {
                    session_id: s.id.clone(),
                    title: None,
                    model: None,
                    thinking: None,
                    permission: Some(mode),
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
    s
}

/// Sends `text`, answers every approval with `decision`, waits for the run
/// to end; the approvals asked.
async fn run(
    h: &Harness,
    id: &str,
    text: &str,
    decision: ApprovalDecision,
) -> Vec<ApprovalRequest> {
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
    .await;
    assert!(done.is_ok(), "the run did not finish");
    h.settle(id).await;
    asked
}

/// The tools the model was offered in its latest request.
fn offered(h: &Harness) -> Vec<String> {
    h.mock
        .agent_requests()
        .last()
        .and_then(|r| r["tools"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
        .collect()
}

fn desktop(scope_desktop: bool, ask: MachineAsk) -> MachineScope {
    MachineScope {
        commands: true,
        desktop: scope_desktop,
        ask,
        ..Default::default()
    }
}

#[tokio::test]
async fn the_tool_comes_with_a_machine_that_can_use_its_desktop() {
    let h = harness(vec![], 100_000).await;
    let s = chat(&h, None).await;
    h.mock.push(Reply::Text("hi".into()));
    run(&h, &s.id, "hello", ApprovalDecision::Allow).await;
    assert!(!offered(&h).contains(&"desktop".to_string()));

    // Desktop control off there, or a machine that can't: still not offered.
    let _a = machine(&h, "dev_a", "Old", desktop(true, MachineAsk::Chat), &[]).await;
    let _b = machine(
        &h,
        "dev_b",
        "Off",
        desktop(false, MachineAsk::Chat),
        &["desktop"],
    )
    .await;
    let s = chat(&h, None).await;
    h.mock.push(Reply::Text("hi".into()));
    run(&h, &s.id, "hello", ApprovalDecision::Allow).await;
    let tools = offered(&h);
    assert!(tools.contains(&"machines".to_string()), "{tools:?}");
    assert!(!tools.contains(&"desktop".to_string()), "{tools:?}");

    // One that can, with desktop control on.
    let _c = machine(
        &h,
        "dev_c",
        "PC",
        desktop(true, MachineAsk::Chat),
        &["desktop"],
    )
    .await;
    let s = chat(&h, None).await;
    h.mock.push(Reply::Text("hi".into()));
    run(&h, &s.id, "hello", ApprovalDecision::Allow).await;
    assert!(offered(&h).contains(&"desktop".to_string()));
    let list: Vec<MachineInfo> =
        serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap())
            .unwrap();
    let pc = list.iter().find(|m| m.name == "PC").unwrap();
    assert_eq!(pc.features, Some(vec!["desktop".to_string()]));
}

#[tokio::test]
async fn machine_features_keep_absent_empty_and_known_backends_distinct() {
    let h = harness(vec![], 100_000).await;
    let (conn, _rx) = h.engine.node_conn("dev_features");
    for (sent, expected) in [
        (None, None),
        (Some(json!([])), Some(vec![])),
        (
            Some(json!([
                "desktop",
                "desktop_session",
                "terminal",
                "admin",
                "sandbox",
                "webviews",
                "future"
            ])),
            Some(vec![
                "desktop",
                "desktop_session",
                "terminal",
                "admin",
                "sandbox",
                "webviews",
            ]),
        ),
    ] {
        let mut hello = json!({"method": "node_hello", "params": {
            "name": "PC", "os": "windows x86_64", "version": "fixture", "scope": {},
            "generation": 1, "sandbox": "off", "sandbox_ok": false
        }});
        if let Some(features) = sent {
            hello["params"]["features"] = features;
        }
        h.engine
            .node_request(serde_json::from_value(hello).unwrap(), &conn)
            .await
            .unwrap();
        let list: Vec<MachineInfo> =
            serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap())
                .unwrap();
        let pc = list.iter().find(|m| m.id == "dev_features").unwrap();
        assert_eq!(
            pc.features,
            expected.map(|fs| fs.into_iter().map(String::from).collect())
        );
    }
}

#[tokio::test]
async fn desktop_actions_go_the_machines_way() {
    let h = harness(vec![], 100_000).await;
    let pc = machine(
        &h,
        "dev_pc",
        "PC",
        desktop(true, MachineAsk::Risky),
        &["desktop"],
    )
    .await;
    let s = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "click", "x": 640, "y": 360}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "type", "text": "hello there"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "open the editor", ApprovalDecision::Allow).await;
    // On a machine that asks before risky actions, looking is risky too
    // (the screen may show anything private), and so are clicks and keys.
    assert_eq!(asked.len(), 3, "{asked:?}");
    assert!(
        asked[0].reason.contains("Looks at PC's screen")
            && asked[0]
                .reason
                .contains("shows the agent what is on that computer's screen"),
        "{}",
        asked[0].reason
    );
    assert!(
        asked[1]
            .reason
            .contains("Uses the mouse or keyboard on PC's screen: click 640,360")
            && asked[1].reason.contains("PC asks before risky actions"),
        "{}",
        asked[1].reason
    );
    let calls = pc.lock().clone();
    assert_eq!(calls.len(), 3);
    assert!(matches!(
        calls[0].action,
        NodeAction::Desktop {
            desktop: DesktopOp::Screenshot { screen: None },
            look: false
        }
    ));
    assert!(calls.iter().all(|c| c.approved), "every one was approved");
    // The screenshot reaches the model as a picture.
    let entries = h.entries(&s.id).await;
    let pictures: Vec<&ContentPart> = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult { tool, content, .. } if tool == "desktop" => Some(content),
            _ => None,
        })
        .flatten()
        .filter(|p| matches!(p, ContentPart::Image { .. }))
        .collect();
    assert_eq!(pictures.len(), 1, "{pictures:?}");
    // What was typed isn't in the machine's recent activity, only how much.
    let list: Vec<MachineInfo> =
        serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap())
            .unwrap();
    let recent = &list[0].recent;
    assert!(
        recent.iter().any(|a| a.target == "type 11 characters")
            && !recent.iter().any(|a| a.target.contains("hello")),
        "{recent:?}"
    );

    // Declined: nothing reaches the machine.
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "key", "keys": "ctrl+s"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, "save it", ApprovalDecision::Deny).await;
    assert_eq!(pc.lock().len(), 3);
    let last = tool_results(&h.entries(&s.id).await).pop().unwrap();
    assert!(last.1 && last.2.contains("declined"), "{last:?}");

    // A read-only chat may look, not click.
    let ro = chat(&h, Some(PermissionMode::ReadOnly)).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "click", "x": 1, "y": 1}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot", "screen": 2}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &ro.id, "look", ApprovalDecision::Allow).await;
    let results = tool_results(&h.entries(&ro.id).await);
    assert!(
        results[0].1 && results[0].2.contains("read-only"),
        "{results:?}"
    );
    assert!(!results[1].1, "{results:?}");
    let calls = pc.lock().clone();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[3].mode, PermissionMode::ReadOnly);
}

#[tokio::test]
async fn a_machine_without_it_says_why_before_anyone_is_asked() {
    let h = harness(vec![], 100_000).await;
    // Both need the tool offered: one machine that can, beside them.
    let _pc = machine(
        &h,
        "dev_pc",
        "PC",
        desktop(true, MachineAsk::Always),
        &["desktop"],
    )
    .await;
    let old = machine(&h, "dev_old", "Old", desktop(true, MachineAsk::Always), &[]).await;
    let off = machine(
        &h,
        "dev_off",
        "Off",
        desktop(false, MachineAsk::Always),
        &["desktop"],
    )
    .await;
    let s = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot", "machine": "Old"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot", "machine": "Off"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let asked = run(&h, &s.id, "look", ApprovalDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("older than desktop actions"),
        "{results:?}"
    );
    assert!(
        results[1].1 && results[1].2.contains("Desktop control is off on Off"),
        "{results:?}"
    );
    assert!(old.lock().is_empty() && off.lock().is_empty());
}

#[tokio::test]
async fn only_yolo_chats_on_a_machine_that_never_asks_go_without_asking() {
    let h = harness(vec![], 100_000).await;
    let pc = machine(
        &h,
        "dev_pc",
        "PC",
        desktop(true, MachineAsk::Chat),
        &["desktop"],
    )
    .await;
    // A YOLO chat, on a machine that leaves it to the chat: nothing asks.
    let yolo = chat(&h, Some(PermissionMode::Auto)).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "click", "x": 5, "y": 5}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &yolo.id, "click it", ApprovalDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
    assert_eq!(pc.lock().len(), 2);
    assert!(pc.lock().iter().all(|c| !c.approved));

    // Any other chat asks, once for using the screen ("allow for this
    // chat"), once for looking at it.
    let s = chat(&h, None).await;
    for a in [
        json!({"action": "click", "x": 1, "y": 1}),
        json!({"action": "click", "x": 2, "y": 2}),
        json!({"action": "screenshot"}),
        json!({"action": "screenshot"}),
    ] {
        h.mock.push(Reply::Tools(vec![("desktop".into(), a)]));
    }
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "click twice", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[0].reason.contains("Uses the mouse or keyboard"));
    assert!(asked[1].reason.contains("Looks at PC's screen"));
    assert_eq!(pc.lock().len(), 6);
}

#[tokio::test]
async fn turning_desktop_control_off_takes_the_tool_away_and_stops_its_use() {
    let h = harness(vec![], 100_000).await;
    let (conn, _rx) = h.engine.node_conn("dev_pc");
    let hello = |desktop_on: bool| Request::NodeHello {
        name: "PC".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: desktop(desktop_on, MachineAsk::Chat),
        generation: 1,
        sandbox: "off".into(),
        sandbox_ok: true,
        home: None,
        computer: None,
        host: None,
        features: Some(vec!["desktop".into()]),
        guard_key: None,
    };
    h.engine.node_request(hello(true), &conn).await.unwrap();
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    h.mock.push(Reply::Text("hi".into()));
    run(&h, &s.id, "hello", ApprovalDecision::Allow).await;
    assert!(offered(&h).contains(&"desktop".to_string()));
    // Switched off on that computer: it tells the server at once.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: desktop(false, MachineAsk::Chat),
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "screenshot"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, "look", ApprovalDecision::Allow).await;
    assert!(!offered(&h).contains(&"desktop".to_string()));
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(r.1, "{r:?}");
}

#[tokio::test]
async fn a_screenshot_after_acting_asks_like_any_screenshot() {
    let h = harness(vec![], 100_000).await;
    let pc = machine(
        &h,
        "dev_pc",
        "PC",
        desktop(true, MachineAsk::Risky),
        &["desktop"],
    )
    .await;
    // Even a YOLO chat: any mouse or keyboard input is risky (a move can
    // open what reacts to hovering, a scroll change a value), and looking
    // afterwards too.
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "move", "x": 3, "y": 4}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "scroll", "x": 3, "y": 4, "dy": 2}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "move", "x": 3, "y": 4, "look": true}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "move", ApprovalDecision::Deny).await;
    assert_eq!(asked.len(), 3, "{asked:?}");
    for a in &asked {
        assert!(
            a.reason.contains("uses the mouse or keyboard"),
            "{}",
            a.reason
        );
    }
    assert!(
        asked[2].reason.contains("Then shows the agent the screen")
            && asked[2]
                .reason
                .contains("shows the agent what is on that computer's screen"),
        "{}",
        asked[2].reason
    );
    // All declined: nothing reached the machine.
    assert!(pc.lock().is_empty(), "{:?}", pc.lock());
}

#[tokio::test]
async fn a_machine_that_asks_every_time_isnt_answered_by_an_earlier_allow() {
    let h = harness(vec![], 100_000).await;
    let (conn, mut rx) = h.engine.node_conn("dev_pc");
    let hello = |ask: MachineAsk| Request::NodeHello {
        name: "PC".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: desktop(true, ask),
        generation: 1,
        sandbox: "off".into(),
        sandbox_ok: true,
        home: None,
        computer: None,
        host: None,
        features: Some(vec!["desktop".into()]),
        guard_key: None,
    };
    h.engine
        .node_request(hello(MachineAsk::Chat), &conn)
        .await
        .unwrap();
    let engine = h.engine.clone();
    let conn = Arc::new(conn);
    let c2 = conn.clone();
    let reached = Arc::new(Mutex::new(0usize));
    let r2 = reached.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                *r2.lock() += 1;
                let _ = engine
                    .node_request(
                        Request::NodeReply {
                            call_id: call.call_id,
                            result: NodeResult {
                                output: "done".into(),
                                ok: true,
                                refused: false,
                                exit_code: None,
                                image: None,
                            },
                        },
                        &c2,
                    )
                    .await;
            }
        }
    });
    let click = || {
        Reply::Tools(vec![(
            "desktop".into(),
            json!({"action": "click", "x": 1, "y": 1}),
        )])
    };
    // The machine leaves it to the chat: "allow for this session" covers
    // the next click.
    let s = chat(&h, None).await;
    h.mock.push(click());
    h.mock.push(click());
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "click", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(!asked[0].once);
    // Tightened there to "every action": that earlier allow no longer counts,
    // each click asks, and allowing "for the session" isn't remembered.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: desktop(true, MachineAsk::Always),
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    h.mock.push(click());
    h.mock.push(click());
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "click again", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked.iter().all(|a| a.once), "{asked:?}");
    assert_eq!(*reached.lock(), 4);
}

#[tokio::test]
async fn a_machine_its_user_stopped_does_nothing_and_asks_again_after_a_resume() {
    let h = harness(vec![], 100_000).await;
    let (conn, mut rx) = h.engine.node_conn("dev_pc");
    let hello = Request::NodeHello {
        name: "PC".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: desktop(true, MachineAsk::Chat),
        generation: 1,
        sandbox: "off".into(),
        sandbox_ok: true,
        home: None,
        computer: None,
        host: None,
        features: Some(vec!["desktop".into()]),
        guard_key: None,
    };
    h.engine.node_request(hello, &conn).await.unwrap();
    let engine = h.engine.clone();
    let conn = Arc::new(conn);
    let c2 = conn.clone();
    let reached = Arc::new(Mutex::new(0usize));
    let r2 = reached.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                *r2.lock() += 1;
                let _ = engine
                    .node_request(
                        Request::NodeReply {
                            call_id: call.call_id,
                            result: NodeResult {
                                output: "done".into(),
                                ok: true,
                                refused: false,
                                exit_code: None,
                                image: None,
                            },
                        },
                        &c2,
                    )
                    .await;
            }
        }
    });
    let scope = |stopped: bool, stops: u32| MachineScope {
        stopped,
        stops,
        ..desktop(true, MachineAsk::Chat)
    };
    let click = || {
        Reply::Tools(vec![(
            "desktop".into(),
            json!({"action": "click", "x": 1, "y": 1}),
        )])
    };
    // Allowed for this chat, before any Stop.
    let s = chat(&h, None).await;
    h.mock.push(click());
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "click", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 1);
    assert_eq!(*reached.lock(), 1);
    // The user pressed Stop there: the tool is gone and nothing reaches it,
    // nobody is asked, and the agent is told not to retry.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: scope(true, 1),
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    h.mock.push(click());
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "run", "machine": "PC", "command": "dir", "cwd": "~"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let asked = run(&h, &s.id, "click", ApprovalDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
    assert!(!offered(&h).contains(&"desktop".to_string()));
    assert_eq!(*reached.lock(), 1);
    // Both tools are gone when this chat cannot do anything on that machine.
    let results = tool_results(&h.entries(&s.id).await);
    let n = results.len();
    assert!(results[n - 2].1, "{:?}", results[n - 2]);
    let (tool, failed, text) = &results[n - 1];
    assert!(
        tool == "machines" && *failed && text.contains("Unknown tool `machines`"),
        "{:?}",
        results[n - 1]
    );
    // Resumed there: the earlier "allow for this chat" ended with the Stop.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: scope(false, 1),
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    h.mock.push(click());
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &s.id, "click", ApprovalDecision::Allow).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(*reached.lock(), 2);
    // Even a YOLO chat asks once after a Stop there ("allow for this chat"
    // then covers it until the next Stop).
    let yolo = chat(&h, Some(PermissionMode::Auto)).await;
    h.mock.push(click());
    h.mock.push(click());
    h.mock.push(Reply::Text("done".into()));
    let asked = run(&h, &yolo.id, "click twice", ApprovalDecision::AllowSession).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0]
            .reason
            .contains("Tessera was stopped on PC earlier")
            && !asked[0].once,
        "{asked:?}"
    );
    assert_eq!(*reached.lock(), 4);
}

#[tokio::test]
async fn stopping_this_computer_stops_the_servers_own_tools_too() {
    let h = harness(vec![], 100_000).await;
    let paths = tessera_core::config::Paths::at(h.dir.path());
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    tessera_core::node::StopLatch::set(&paths, true, 0).unwrap();
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "echo hello"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, "say hello", ApprovalDecision::Allow).await;
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        r.1 && r.2.contains("stopped Tessera on this computer") && r.2.contains("Don't retry"),
        "{r:?}"
    );
    // Resumed on this computer: tools run again.
    tessera_core::node::StopLatch::set(&paths, false, 0).unwrap();
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "echo hello"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, "say hello again", ApprovalDecision::Allow).await;
    let r = tool_results(&h.entries(&s.id).await).pop().unwrap();
    assert!(!r.1 && r.2.contains("hello"), "{r:?}");
}

#[tokio::test]
async fn an_answer_given_after_a_stop_here_allows_nothing() {
    let h = harness(vec![], 100_000).await;
    let paths = tessera_core::config::Paths::at(h.dir.path());
    let pc = machine(
        &h,
        "dev_pc",
        "PC",
        desktop(true, MachineAsk::Chat),
        &["desktop"],
    )
    .await;
    let s = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "desktop".into(),
        json!({"action": "click", "x": 1, "y": 1}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    // The user stops this computer while the approval waits, then allows it.
    let mut events = h.engine.subscribe();
    h.send(&s.id, "click").await;
    let answered = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ApprovalRequested {
                session_id,
                request,
            }) = events.recv().await
            {
                tessera_core::node::StopLatch::set(&paths, true, 0).unwrap();
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
                return;
            }
        }
    })
    .await;
    assert!(answered.is_ok(), "no approval was asked");
    h.settle(&s.id).await;
    assert!(pc.lock().is_empty(), "nothing reached the machine");
}

#[tokio::test]
async fn a_command_approved_after_a_stop_here_does_not_run() {
    let h = harness(vec![], 100_000).await;
    let paths = tessera_core::config::Paths::at(h.dir.path());
    let s = chat(&h, Some(PermissionMode::Ask)).await;
    let marker = h.work.join("made-by-the-agent");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("touch {}", marker.display())}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "make it").await;
    let answered = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ApprovalRequested {
                session_id,
                request,
            }) = events.recv().await
            {
                // Stopped while the card waits, then allowed.
                tessera_core::node::StopLatch::set(&paths, true, 0).unwrap();
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
                return;
            }
        }
    })
    .await;
    assert!(answered.is_ok(), "no approval was asked");
    h.settle(&s.id).await;
    assert!(!marker.exists(), "the command ran after the Stop");
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        r.1 && r.2.contains("stopped this turn") && r.2.contains("End the turn"),
        "{r:?}"
    );
}

#[tokio::test]
async fn a_turn_caught_by_a_stop_runs_nothing_more_and_a_new_one_runs() {
    let h = harness(vec![], 100_000).await;
    let paths = tessera_core::config::Paths::at(h.dir.path());
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    let late = h.work.join("after-the-stop");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 3"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("touch {}", late.display())}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "wait, then make it").await;
    // Stopped and resumed again while the first command runs.
    tokio::time::sleep(Duration::from_millis(800)).await;
    tessera_core::node::StopLatch::set(&paths, true, 0).unwrap();
    tessera_core::node::StopLatch::set(&paths, false, 0).unwrap();
    h.settle(&s.id).await;
    assert!(
        !late.exists(),
        "a tool of the stopped turn ran after the Resume"
    );
    // A new message after the Resume works as usual (what the stopped turn
    // didn't ask the model for goes).
    h.mock.clear();
    let fresh = h.work.join("after-the-resume");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("touch {}", fresh.display())}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &s.id, "make it now", ApprovalDecision::Allow).await;
    assert!(
        fresh.exists(),
        "{:?}",
        tool_results(&h.entries(&s.id).await)
    );
}

#[tokio::test]
async fn a_stop_on_this_computers_own_machine_stops_this_server_at_once() {
    let Some(here) = tessera_secure::computer_key() else {
        return;
    };
    let h = harness(vec![], 100_000).await;
    let (conn, _rx) = h.engine.node_conn("dev_here");
    let hello = |stopped: bool| Request::NodeHello {
        name: "This PC".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: MachineScope {
            stopped,
            stops: u32::from(stopped),
            ..desktop(true, MachineAsk::Chat)
        },
        generation: 1,
        sandbox: "off".into(),
        sandbox_ok: true,
        home: None,
        computer: Some(here.clone()),
        host: None,
        features: Some(vec!["desktop".into()]),
        guard_key: None,
    };
    // Its Stop bar was pressed but nothing could be written: the machine
    // says so live, and that is enough.
    h.engine.node_request(hello(true), &conn).await.unwrap();
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    let made = h.work.join("made");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("touch {}", made.display())}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    run(&h, &s.id, "make it", ApprovalDecision::Allow).await;
    assert!(!made.exists());
    let r = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        r.1 && r.2.contains("stopped Tessera on this computer"),
        "{r:?}"
    );
}

#[tokio::test]
async fn a_turn_begun_right_after_a_resume_is_left_alone() {
    let h = harness(vec![], 100_000).await;
    let paths = tessera_core::config::Paths::at(h.dir.path());
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    // Stopped and resumed, then at once a new message: its turn belongs to
    // after the Resume, so the next look of the watcher leaves it running.
    tessera_core::node::StopLatch::set(&paths, true, 0).unwrap();
    tessera_core::node::StopLatch::set(&paths, false, 0).unwrap();
    let made = h.work.join("made-after-the-resume");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("sleep 2 && touch {}", made.display())}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &s.id, "make it", ApprovalDecision::Allow).await;
    assert!(made.exists(), "{:?}", tool_results(&h.entries(&s.id).await));
}

#[tokio::test]
async fn a_quick_stop_and_resume_on_this_computers_machine_still_ends_older_turns() {
    let Some(here) = tessera_secure::computer_key() else {
        return;
    };
    let h = harness(vec![], 100_000).await;
    let (conn, _rx) = h.engine.node_conn("dev_here");
    let hello = Request::NodeHello {
        name: "This PC".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: desktop(true, MachineAsk::Chat),
        generation: 1,
        sandbox: "off".into(),
        sandbox_ok: true,
        home: None,
        computer: Some(here.clone()),
        host: None,
        features: Some(vec!["desktop".into()]),
        guard_key: None,
    };
    h.engine.node_request(hello, &conn).await.unwrap();
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    let late = h.work.join("after-the-stop");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 3"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("touch {}", late.display())}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "wait, then make it").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Its Stop bar pressed and Resume pressed, within the same second (a
    // machine counts every Stop).
    for stopped in [true, false] {
        h.engine
            .node_request(
                Request::NodeScope {
                    scope: MachineScope {
                        stopped,
                        stops: 1,
                        ..desktop(true, MachineAsk::Chat)
                    },
                    name: None,
                },
                &conn,
            )
            .await
            .unwrap();
    }
    h.settle(&s.id).await;
    assert!(
        !late.exists(),
        "a tool of the turn from before the Stop ran"
    );
}

#[tokio::test]
async fn a_stop_seen_only_as_a_higher_count_still_ends_older_turns_here() {
    let Some(here) = tessera_secure::computer_key() else {
        return;
    };
    let h = harness(vec![], 100_000).await;
    let (conn, _rx) = h.engine.node_conn("dev_here");
    let hello = Request::NodeHello {
        name: "This PC".into(),
        os: "windows x86_64".into(),
        version: "0.1.0".into(),
        scope: desktop(true, MachineAsk::Chat),
        generation: 1,
        sandbox: "off".into(),
        sandbox_ok: true,
        home: None,
        computer: Some(here.clone()),
        host: None,
        features: Some(vec!["desktop".into()]),
        guard_key: None,
    };
    h.engine.node_request(hello, &conn).await.unwrap();
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    let late = h.work.join("after-the-stop");
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 3"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("touch {}", late.display())}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "wait, then make it").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Stop and Resume both pressed between two of its looks: the server
    // only hears of a higher count, never "stopped".
    h.engine
        .node_request(
            Request::NodeScope {
                scope: MachineScope {
                    stops: 1,
                    ..desktop(true, MachineAsk::Chat)
                },
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    assert!(
        !late.exists(),
        "a tool of the turn from before the Stop ran"
    );
}

#[tokio::test]
async fn a_turn_from_before_a_stop_on_another_computer_does_nothing_more_there() {
    let h = harness(vec![], 100_000).await;
    let (conn, seen) =
        machine_conn(&h, "dev_pc", "PC", desktop(false, MachineAsk::Chat), &[]).await;
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    // One turn: a slow command here, then a command there that asks nobody.
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 3"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "run", "machine": "PC", "command": "dir", "cwd": "~"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "wait, then list it").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Stopped and resumed there meanwhile: the server sees its new count.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: MachineScope {
                    stops: 1,
                    ..desktop(false, MachineAsk::Chat)
                },
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    assert!(seen.lock().is_empty(), "{:?}", seen.lock());
    let r = tool_results(&h.entries(&s.id).await).pop().unwrap();
    assert!(
        r.0 == "machines"
            && r.1
            && r.2
                .contains("stopped on that computer after this turn began"),
        "{r:?}"
    );
    // A new message after the Resume uses it as usual.
    h.mock.clear();
    h.mock.push(Reply::Tools(vec![(
        "machines".into(),
        json!({"action": "run", "machine": "PC", "command": "dir", "cwd": "~"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    run(&h, &s.id, "list it now", ApprovalDecision::Allow).await;
    let calls = seen.lock().clone();
    assert!(calls.len() == 1 && calls[0].stops == Some(1), "{calls:?}");
    // A report from before the Stop arriving late doesn't undo it here.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: desktop(false, MachineAsk::Chat),
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let list: Vec<MachineInfo> =
        serde_json::from_value(h.engine.handle(Request::ListMachines, None).await.unwrap())
            .unwrap();
    assert!(
        list.iter().any(|m| m.id == "dev_pc" && m.scope.stops == 1),
        "{list:?}"
    );
}

#[tokio::test]
async fn a_turn_continued_after_a_restart_is_still_from_before_a_stop_there() {
    let h = harness(vec![], 100_000).await;
    let (conn, seen) =
        machine_conn(&h, "dev_pc", "PC", desktop(false, MachineAsk::Chat), &[]).await;
    let s = chat(&h, Some(PermissionMode::Auto)).await;
    let list = || {
        Reply::Tools(vec![(
            "machines".into(),
            json!({"action": "run", "machine": "PC", "command": "dir", "cwd": "~"}),
        )])
    };
    let wait = |secs: u32| {
        Reply::Tools(vec![(
            "bash".into(),
            json!({"command": format!("sleep {secs}")}),
        )])
    };
    h.mock.push(wait(3));
    // Continued after the restart: a moment for the machine to come back,
    // then the command there.
    h.mock.push(wait(1));
    h.mock.push(list());
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "wait, then list it").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Stopped and resumed there, then the server is updated mid-turn.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: MachineScope {
                    stops: 1,
                    ..desktop(false, MachineAsk::Chat)
                },
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    h.engine.shutdown().await;
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        tessera_core::EngineOptions { scheduler: true },
    )
    .await
    .unwrap();
    // The machine comes back with the same count: nothing new to see.
    let (conn2, mut rx) = engine.node_conn("dev_pc");
    engine
        .node_request(
            Request::NodeHello {
                name: "PC".into(),
                os: "windows x86_64, shell Git Bash (bash)".into(),
                version: "0.1.0".into(),
                scope: MachineScope {
                    stops: 1,
                    ..desktop(false, MachineAsk::Chat)
                },
                generation: 1,
                sandbox: "off".into(),
                sandbox_ok: true,
                home: None,
                computer: None,
                host: None,
                features: Some(vec![]),
                guard_key: None,
            },
            &conn2,
        )
        .await
        .unwrap();
    let after = Arc::new(Mutex::new(Vec::<NodeCall>::new()));
    let keep = after.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { call } = ev {
                keep.lock().push(call);
            }
        }
    });
    let mut refused = None;
    for _ in 0..200 {
        let v = engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap();
        let snap: SessionSnapshot = serde_json::from_value(v).unwrap();
        refused = tool_results(&snap.entries)
            .into_iter()
            .find(|r| r.0 == "machines");
        if refused.is_some() && snap.info.status == SessionStatus::Idle {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let r = refused.expect("the continued turn reached the machines call");
    assert!(
        r.1 && r
            .2
            .contains("stopped on that computer after this turn began"),
        "{r:?}"
    );
    assert!(seen.lock().is_empty() && after.lock().is_empty());
    drop(conn2);
}
