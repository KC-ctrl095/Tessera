//! Agents don't pop tiles: their whiteboards, notes and browsing show in the
//! chat (cards, step pictures), and only an explicit ask puts something on
//! the user's screen (`whiteboard … open: true`, an `ask` on a board, a
//! note's `show: true`, the browser's `show` and `needs_user`). What the
//! user opens themselves is placed as before. The browser parts need a
//! Chromium-family browser and are skipped without one.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use common::*;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_proto::*;

async fn call<T: serde::de::DeserializeOwned>(h: &Harness, req: Request) -> T {
    serde_json::from_value(h.engine.handle(req, None).await.unwrap()).unwrap()
}

async fn layout(h: &Harness) -> MosaicLayout {
    call(h, Request::GetMosaics).await
}

/// The board and note tiles in the shared layout.
async fn tiles(h: &Harness) -> (Vec<String>, Vec<String>) {
    let l = layout(h).await;
    let all: Vec<&MosaicTile> = l.mosaics.iter().flat_map(|m| &m.tiles).collect();
    (
        all.iter().filter_map(|t| t.board_id.clone()).collect(),
        all.iter().filter_map(|t| t.note_id.clone()).collect(),
    )
}

async fn boards(h: &Harness, session: &str) -> Vec<Whiteboard> {
    call(
        h,
        Request::ListBoards {
            session_id: Some(session.into()),
        },
    )
    .await
}

/// Each tool result's details, in order.
fn details(entries: &[Entry]) -> Vec<Value> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult { details, .. } => Some(details.clone().unwrap_or(Value::Null)),
            _ => None,
        })
        .collect()
}

fn tool(name: &str, args: Value) -> Reply {
    Reply::Tools(vec![(name.into(), args)])
}

/// Put the chat's tile in the shared layout (as an app showing it does).
async fn chat_on_screen(h: &Harness, session: &str) {
    let mut l = layout(h).await;
    l.mosaics = vec![Mosaic {
        id: "m1".into(),
        name: "Work".into(),
        layout: "dwindle".into(),
        tiles: vec![MosaicTile {
            id: "t-chat".into(),
            session_id: Some(session.into()),
            board_id: None,
            note_id: None,
        }],
        geometry: None,
    }];
    let _: Value = call(h, Request::SaveMosaics { layout: l }).await;
}

/// Answers the first question asked on a board with `answer`, after
/// checking (and reporting) whether that board was on screen by then.
fn answer_asks(h: &Harness, answer: AskAnswer) -> Arc<Mutex<Vec<(Whiteboard, bool)>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut rx = h.engine.subscribe();
    let engine = h.engine.clone();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            let Event::BoardUpdated { board } = ev else {
                continue;
            };
            let Some(ask) = board.ask.clone() else {
                continue;
            };
            let l: MosaicLayout =
                serde_json::from_value(engine.handle(Request::GetMosaics, None).await.unwrap())
                    .unwrap();
            let shown = l.mosaics.iter().any(|m| {
                m.tiles
                    .iter()
                    .any(|t| t.board_id.as_deref() == Some(&board.id))
            });
            log.lock().push((board.clone(), shown));
            let _ = engine
                .handle(
                    Request::AnswerBoard {
                        id: board.id.clone(),
                        ask_id: ask.id,
                        answer: answer.clone(),
                    },
                    None,
                )
                .await;
        }
    });
    seen
}

#[tokio::test]
async fn whiteboards_and_notes_are_cards_until_asked_for() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    chat_on_screen(&h, &s.id).await;

    // The agent makes a whiteboard and a note on its own: cards, no tiles.
    h.mock.push(Reply::Tools(vec![
        (
            "whiteboard".into(),
            json!({"action": "show", "title": "Plan", "markdown": "# Steps"}),
        ),
        (
            "note".into(),
            json!({"action": "create", "title": "Router notes", "content": "- WAN up"}),
        ),
    ]));
    h.mock.push(Reply::Text("Done.".into()));
    h.send(&s.id, "look into my router").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let r = tool_results(&entries);
    let d = details(&entries);
    assert!(!r[0].1 && r[0].2.contains("card in the chat"), "{r:?}");
    assert_eq!(d[0]["board"]["title"], "Plan");
    let plan = d[0]["board"]["id"].as_str().unwrap().to_string();
    assert_eq!(d[1]["note"]["title"], "Router notes");
    assert_eq!(d[1]["note"]["action"], "create");
    let note = d[1]["note"]["id"].as_str().unwrap().to_string();
    assert_eq!(tiles(&h).await, (vec![], vec![]), "no tile popped up");

    // Updating them still puts nothing up.
    h.mock.push(Reply::Tools(vec![
        (
            "whiteboard".into(),
            json!({"action": "show", "id": plan, "markdown": "# Steps\n1. a"}),
        ),
        (
            "note".into(),
            json!({"action": "update", "id": note, "append": "- LAN up"}),
        ),
    ]));
    h.mock.push(Reply::Text("Updated.".into()));
    h.send(&s.id, "add more").await;
    h.settle(&s.id).await;
    let d = details(&h.entries(&s.id).await);
    assert_eq!(d[3]["note"]["action"], "update");
    assert_eq!(d[3]["note"]["version"], 2);
    assert_eq!(tiles(&h).await, (vec![], vec![]));

    // Asked to show them: the tiles go beside the chat, in its mosaic.
    h.mock.push(Reply::Tools(vec![
        (
            "whiteboard".into(),
            json!({"action": "show", "id": plan, "markdown": "# Steps", "open": true}),
        ),
        (
            "note".into(),
            json!({"action": "read", "id": note, "show": true}),
        ),
    ]));
    h.mock.push(Reply::Text("Here.".into()));
    h.send(&s.id, "show me both").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let r = tool_results(&entries);
    assert!(r[4].2.contains("in a tile beside this chat"), "{r:?}");
    assert!(r[5].2.contains("open beside this chat"), "{r:?}");
    assert_eq!(details(&entries)[5]["note"]["id"], note.as_str());
    let l = layout(&h).await;
    assert_eq!(l.mosaics.len(), 1, "no new mosaic");
    let ids: Vec<String> = l.mosaics[0].tiles.iter().map(|t| t.id.clone()).collect();
    assert_eq!(ids[0], "t-chat", "{ids:?}");
    assert_eq!(tiles(&h).await, (vec![plan.clone()], vec![note.clone()]));

    // The user closes the board's tile; the agent's next update leaves it closed.
    let mut l = layout(&h).await;
    l.mosaics[0]
        .tiles
        .retain(|t| t.board_id.as_deref() != Some(plan.as_str()));
    let _: Value = call(&h, Request::SaveMosaics { layout: l }).await;
    h.mock.push(tool(
        "whiteboard",
        json!({"action": "show", "id": plan, "markdown": "# Steps v3"}),
    ));
    h.mock.push(Reply::Text("Updated.".into()));
    h.send(&s.id, "tweak it").await;
    h.settle(&s.id).await;
    let r = tool_results(&h.entries(&s.id).await);
    assert!(r[6].2.contains("card in the chat"), "{r:?}");
    assert!(!tiles(&h).await.0.contains(&plan));
}

#[tokio::test]
async fn questions_come_to_the_chat_and_boards_they_are_about_go_on_screen() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    chat_on_screen(&h, &s.id).await;
    let asked = answer_asks(&h, AskAnswer::Confirm { yes: true });
    // A plain question: in the chat only.
    h.mock.push(tool(
        "whiteboard",
        json!({"action": "ask", "prompt": "Go ahead?", "input": "confirm"}),
    ));
    h.mock.push(Reply::Text("Going.".into()));
    h.send(&s.id, "deploy it").await;
    h.settle(&s.id).await;
    let r = tool_results(&h.entries(&s.id).await);
    assert!(r[0].2.contains("said yes"), "{r:?}");
    {
        let a = asked.lock();
        assert_eq!(a.len(), 1);
        assert!(!a[0].1, "a plain question put a tile up");
    }
    assert!(tiles(&h).await.0.is_empty());

    // A question about a board the user must look at: that board goes up.
    h.mock.push(tool(
        "whiteboard",
        json!({"action": "show", "title": "Designs", "markdown": "A or B"}),
    ));
    h.mock.push(Reply::Text("Made it.".into()));
    h.send(&s.id, "draw two designs").await;
    h.settle(&s.id).await;
    let board = boards(&h, &s.id)
        .await
        .into_iter()
        .find(|b| b.title == "Designs")
        .unwrap();
    assert!(tiles(&h).await.0.is_empty());
    h.mock.push(tool(
        "whiteboard",
        json!({"action": "ask", "id": board.id, "prompt": "Which one?", "input": "confirm"}),
    ));
    h.mock.push(Reply::Text("Thanks.".into()));
    h.send(&s.id, "ask me which").await;
    h.settle(&s.id).await;
    let a = asked.lock();
    assert_eq!(a.len(), 2);
    assert!(
        a[1].1 && a[1].0.id == board.id,
        "the board asked on is on screen"
    );
}

#[tokio::test]
async fn what_the_user_opens_is_placed_as_before() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    chat_on_screen(&h, &s.id).await;
    // `/show` from the chat: the server puts it beside the chat.
    let b: Whiteboard = call(
        &h,
        Request::CreateBoard {
            session_id: s.id.clone(),
            title: "Mine".into(),
            content: BoardContent::Markdown {
                markdown: "hi".into(),
            },
            placed: false,
        },
    )
    .await;
    assert!(b.by_user);
    let l = layout(&h).await;
    let t = &l.mosaics[0].tiles;
    assert_eq!(t[0].session_id.as_deref(), Some(s.id.as_str()));
    assert_eq!(t[1].board_id.as_deref(), Some(b.id.as_str()));
    // A tile spawned on a device: the device placed it, the server doesn't.
    let w: Whiteboard = call(
        &h,
        Request::CreateBoard {
            session_id: s.id.clone(),
            title: "Spawned".into(),
            content: BoardContent::Url {
                url: "https://example.com/".into(),
            },
            placed: true,
        },
    )
    .await;
    assert!(!tiles(&h).await.0.contains(&w.id));
    // A note the user opens (save_note, then its tile from the app) is theirs.
    let n: Note = call(
        &h,
        Request::SaveNote {
            id: None,
            title: "Groceries".into(),
            content: "milk".into(),
            base_version: None,
            session_id: Some(s.id.clone()),
        },
    )
    .await;
    assert!(n.by_user);
    assert!(tiles(&h).await.1.is_empty());
}

// ---- the browser ---------------------------------------------------------

fn have_browser() -> bool {
    let ok = tessera_core::browser::installed(None);
    if !ok {
        eprintln!("no Chromium-family browser installed; skipping");
    }
    ok
}

const LOGIN: &str = r#"<!doctype html><html><head><title>Router</title></head><body>
<h1>Router admin</h1>
<form method="post" action="/login">
  <label>User <input name="user"></label>
  <label>Password <input type="password" name="pw"></label>
  <button>Log in</button>
</form>
<p>Settings: WAN, LAN, Wi-Fi</p>
</body></html>"#;

/// A pretend router admin page: http://127.0.0.1:<port>/
async fn router() -> (String, Arc<Mutex<Vec<String>>>) {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let p = posted.clone();
    let app = Router::new()
        .route("/", get(|| async { axum::response::Html(LOGIN) }))
        .route(
            "/login",
            post(move |body: String| {
                let p = p.clone();
                async move {
                    p.lock().push(body);
                    axum::response::Html(
                        "<!doctype html><title>Status</title><h1>Status</h1><p>WAN: up</p>",
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), posted)
}

/// Allows every approval.
fn allow_all(h: &Harness) {
    let mut rx = h.engine.subscribe();
    let engine = h.engine.clone();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Event::ApprovalRequested {
                session_id,
                request,
            } = ev
            {
                let _ = engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision: ApprovalDecision::Allow,
                        },
                        None,
                    )
                    .await;
            }
        }
    });
}

async fn secret(h: &Harness, name: &str, value: &str, hosts: &[&str]) {
    h.engine
        .handle(
            Request::SaveSecret {
                name: name.into(),
                kind: SecretKind::Token,
                description: String::new(),
                fields: [("value".to_string(), value.to_string())].into(),
                remove_fields: Vec::new(),
                ask: true,
                hosts: hosts.iter().map(|h| h.to_string()).collect(),
                bots: None,
                on_server: None,
                on_machines: None,
                new_name: None,
                machines_default: None,
            },
            None,
        )
        .await
        .unwrap();
}

async fn idle(h: &Harness, id: &str) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(90), h.engine.wait_idle(id))
        .await
        .expect("the turn didn't finish");
}

fn assert_shot(d: &Value) {
    let shot = &d["shot"];
    assert_eq!(shot["mime"], "image/jpeg", "{d}");
    assert!(shot["data"].as_str().is_some_and(|s| s.len() > 100), "{d}");
    assert!(
        shot["width"].as_u64().is_some_and(|w| w > 0 && w <= 240),
        "{d}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_browses_and_signs_in_in_the_background() {
    if !have_browser() {
        return;
    }
    let (site, posted) = router().await;
    let url = format!("{site}/");
    let h = harness(vec![], 100_000).await;
    allow_all(&h);
    secret(&h, "ROUTER_PW", "testpw2-router", &["127.0.0.1"]).await;
    let s = h.session().await;
    chat_on_screen(&h, &s.id).await;

    // Browsing, and an older instruction's `show: true`: background tabs.
    h.mock
        .push(tool("browser", json!({"action": "open", "url": url})));
    h.mock.push(tool(
        "browser",
        json!({"action": "open", "url": url, "show": true}),
    ));
    // Signing in with a stored login, in the background too.
    h.mock.push(tool(
        "browser",
        json!({"action": "type", "ref": "e1", "text": "admin"}),
    ));
    h.mock.push(tool(
        "browser",
        json!({"action": "type", "ref": "e2", "text": "$ROUTER_PW"}),
    ));
    h.mock.push(tool(
        "browser",
        json!({"action": "click", "text": "Log in"}),
    ));
    h.mock.push(Reply::Text("WAN is up.".into()));
    h.send(&s.id, "check my router's status").await;
    idle(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let r = tool_results(&entries);
    let d = details(&entries);
    assert_eq!(r.len(), 5, "{r:?}");
    for (i, (_, err, text)) in r.iter().enumerate() {
        assert!(!err, "step {i}: {text}");
    }
    assert!(r[0].2.contains("Opened background tab"), "{}", r[0].2);
    assert!(
        r[1].2.contains("not on screen") && r[1].2.contains("needs_user"),
        "{}",
        r[1].2
    );
    assert!(r[4].2.contains("WAN: up"), "{}", r[4].2);
    assert_eq!(posted.lock().as_slice(), ["user=admin&pw=testpw2-router"]);
    // No tile, no board: the chat has the steps, each with a picture...
    assert!(boards(&h, &s.id).await.is_empty());
    assert_eq!(tiles(&h).await, (vec![], vec![]));
    for i in [0, 1, 2, 4] {
        assert_shot(&d[i]);
        assert!(
            d[i]["tab"].as_str().is_some_and(|t| t.starts_with('t')),
            "{}",
            d[i]
        );
        assert!(d[i]["board_id"].is_null(), "{}", d[i]);
    }
    // ...but none of the field a stored secret was just typed into.
    assert!(d[3]["shot"].is_null(), "{}", d[3]);
    for (_, _, text) in &r {
        assert!(!text.contains("testpw2"), "a secret leaked: {text}");
    }

    // The user watches the tab the agent signed in on: it becomes their web
    // view as it is (their device places it), and the agent carries on in it
    // by its old name.
    let tab = d[4]["tab"].as_str().unwrap().to_string();
    let watched: Whiteboard = call(
        &h,
        Request::BrowserWatch {
            session_id: s.id.clone(),
            tab: tab.clone(),
        },
    )
    .await;
    assert!(watched.by_user, "{watched:?}");
    assert!(
        matches!(&watched.content, BoardContent::Url { url } if url.ends_with("/login")),
        "{watched:?}"
    );
    assert!(tiles(&h).await.0.is_empty(), "the device places it");
    let again: Whiteboard = call(
        &h,
        Request::BrowserWatch {
            session_id: s.id.clone(),
            tab: watched.id.clone(),
        },
    )
    .await;
    assert_eq!(again.id, watched.id);
    // Watching the step again by the tab's old name (its tile was closed).
    let by_old: Whiteboard = call(
        &h,
        Request::BrowserWatch {
            session_id: s.id.clone(),
            tab: tab.clone(),
        },
    )
    .await;
    assert_eq!(by_old.id, watched.id);
    h.mock
        .push(tool("browser", json!({"action": "snapshot", "tab": tab})));
    h.mock.push(tool("browser", json!({"action": "tabs"})));
    h.mock.push(Reply::Text("Still up.".into()));
    h.send(&s.id, "and now?").await;
    idle(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let r = tool_results(&entries);
    let d = details(&entries);
    assert!(!r[5].1 && r[5].2.contains("WAN: up"), "{:?}", r[5]);
    assert_eq!(d[5]["board_id"], watched.id.as_str());
    assert!(
        r[6].2.contains(&watched.id) && r[6].2.contains("web view tile"),
        "{}",
        r[6].2
    );
    // Someone else's chat can't watch it.
    let other = h.session().await;
    assert!(
        h.engine
            .handle(
                Request::BrowserWatch {
                    session_id: other.id.clone(),
                    tab: watched.id.clone(),
                },
                None,
            )
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_browser_goes_on_screen_only_when_shown_or_handed_over() {
    if !have_browser() {
        return;
    }
    let (site, _) = router().await;
    let url = format!("{site}/");
    let h = harness(vec![], 100_000).await;
    allow_all(&h);
    let asked = answer_asks(&h, AskAnswer::Confirm { yes: true });
    let s = h.session().await;
    chat_on_screen(&h, &s.id).await;

    // Asked to see it: `show` puts the tab beside the chat.
    h.mock
        .push(tool("browser", json!({"action": "open", "url": url})));
    h.mock.push(tool(
        "browser",
        json!({"action": "show", "reason": "You asked to see it"}),
    ));
    h.mock.push(Reply::Text("It's beside the chat.".into()));
    h.send(&s.id, "show me my router's page").await;
    idle(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let r = tool_results(&entries);
    assert!(
        !r[1].1 && r[1].2.contains("in a tile beside the chat"),
        "{r:?}"
    );
    let shown = details(&entries)[1]["board_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_shot(&details(&entries)[1]);
    let l = layout(&h).await;
    assert_eq!(l.mosaics.len(), 1, "no other mosaic");
    assert_eq!(
        l.mosaics[0].tiles[1].board_id.as_deref(),
        Some(shown.as_str()),
        "right after the chat"
    );
    assert!(!boards(&h, &s.id).await.iter().any(|b| b.by_user));

    // A sign-in only the user can do: handed over with the reason, on
    // screen, and the agent waits for Done.
    h.mock.push(tool(
        "browser",
        json!({"action": "needs_user", "url": url, "reason": "Sign in to the router (there is no stored login)"}),
    ));
    h.mock.push(Reply::Text("Thanks.".into()));
    h.send(&s.id, "log in to the router").await;
    idle(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    let r = tool_results(&entries);
    assert!(!r[2].1 && r[2].2.contains("The user is done"), "{:?}", r[2]);
    assert!(r[2].2.contains("Router admin"), "{}", r[2].2);
    let a = asked.lock().clone();
    assert_eq!(a.len(), 1);
    let (board, on_screen) = &a[0];
    assert!(*on_screen, "handed over but not on screen");
    let ask = board.ask.as_ref().unwrap();
    assert_eq!(
        ask.prompt,
        "Sign in to the router (there is no stored login)"
    );
    assert!(matches!(ask.input, AskInput::Confirm));
    assert_eq!(details(&entries)[2]["board_id"], board.id.as_str());
    let (on, _) = tiles(&h).await;
    assert!(
        on.contains(&shown)
            && on.contains(
                &details(&entries)[2]["board_id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            )
    );

    // No reason, no handover.
    h.mock
        .push(tool("browser", json!({"action": "needs_user"})));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "again").await;
    idle(&h, &s.id).await;
    let r = tool_results(&h.entries(&s.id).await);
    assert!(
        r[3].1 && r[3].2.contains("`reason` is required"),
        "{:?}",
        r[3]
    );
}

/// A small live check (`TESSERA_LIVE_MODEL=provider/model`, e.g.
/// `lmstudio/qwen3.8-27b`, at `TESSERA_LIVE_URL`): a browsing task leaves
/// nothing on screen; "show me" puts the page up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_model_browses_in_the_background_and_shows_when_asked() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        eprintln!("set TESSERA_LIVE_MODEL");
        return;
    };
    if !have_browser() {
        return;
    }
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let (site, _) = router().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"
        ),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    let h = Harness {
        engine,
        mock: Mock::start(vec![]).await,
        dir,
        work: std::env::temp_dir(),
        _work_dir: tempfile::tempdir().unwrap(),
    };
    allow_all(&h);
    let s = h.session().await;
    chat_on_screen(&h, &s.id).await;
    let turn = |text: String| {
        let h = &h;
        let id = s.id.clone();
        async move {
            h.send(&id, &text).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            tokio::time::timeout(Duration::from_secs(600), h.engine.wait_idle(&id))
                .await
                .unwrap();
            for (tool, err, out) in tool_results(&h.entries(&id).await) {
                eprintln!(
                    "[{tool}{}] {}",
                    if err { " ERROR" } else { "" },
                    tessera_core::util::truncate(&out, 160)
                );
            }
        }
    };
    turn(format!(
        "Use the browser to open {site}/ and tell me which settings it lists. Be brief."
    ))
    .await;
    let used = tool_results(&h.entries(&s.id).await)
        .iter()
        .any(|(t, err, _)| t == "browser" && !err);
    assert!(used, "the model didn't use the browser");
    assert_eq!(tiles(&h).await, (vec![], vec![]), "browsing put a tile up");
    turn("Show me that page.".into()).await;
    let (on, _) = tiles(&h).await;
    eprintln!("tiles after \"show me\": {on:?}");
    assert_eq!(on.len(), 1, "\"show me\" didn't put the page up");
}
