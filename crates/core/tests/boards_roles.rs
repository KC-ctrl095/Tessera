//! User-opened whiteboards the agent can read, model roles (the memory
//! review's model), fast mode, and the background review as a helper.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

async fn call<T: serde::de::DeserializeOwned>(h: &Harness, req: Request) -> T {
    serde_json::from_value(h.engine.handle(req, None).await.unwrap()).unwrap()
}

fn create_board(session_id: &str, title: &str, content: BoardContent) -> Request {
    Request::CreateBoard {
        session_id: session_id.into(),
        title: title.into(),
        placed: false,
        content,
    }
}

/// One mosaic per chat, named as given.
async fn put_in_mosaics(h: &Harness, chats: &[(&str, &str)]) {
    let mosaics = chats
        .iter()
        .map(|(name, chat)| Mosaic {
            id: format!("m-{name}"),
            name: name.to_string(),
            layout: "dwindle".into(),
            tiles: vec![chat_tile(chat)],
            geometry: None,
        })
        .collect();
    let _: MosaicLayout = call(
        h,
        Request::SaveMosaics {
            layout: MosaicLayout {
                version: 0,
                mosaics,
            },
        },
    )
    .await;
}

fn chat_tile(id: &str) -> MosaicTile {
    MosaicTile {
        id: format!("t-{id}"),
        session_id: Some(id.into()),
        board_id: None,
        note_id: None,
    }
}

async fn dev_server() -> u16 {
    use axum::routing::get;
    let app = axum::Router::new().route(
        "/status",
        get(|| async {
            axum::response::Html(
                "<html><head><title>Build status</title><script>var tracker = 1;</script></head>\
                 <body><h1>Pipeline</h1><p>All 12 checks green</p></body></html>",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

/// A web view spawned in a tile of its own: the server adds no tile (and,
/// with no mosaics yet, no "Main" mosaic holding the chat).
#[tokio::test]
async fn a_board_the_device_placed_adds_no_tiles() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let b: Whiteboard = call(
        &h,
        Request::CreateBoard {
            session_id: s.id.clone(),
            title: "Notes".into(),
            content: BoardContent::Markdown {
                markdown: "- milk".into(),
            },
            placed: true,
        },
    )
    .await;
    assert!(b.by_user);
    let layout: MosaicLayout = call(&h, Request::GetMosaics).await;
    assert!(layout.mosaics.is_empty(), "{layout:?}");

    // Without `placed` (the terminal's /board) the server places it.
    let _: Whiteboard = call(
        &h,
        create_board(
            &s.id,
            "More",
            BoardContent::Markdown {
                markdown: "- eggs".into(),
            },
        ),
    )
    .await;
    let layout: MosaicLayout = call(&h, Request::GetMosaics).await;
    assert_eq!(layout.mosaics.len(), 1);
    assert_eq!(layout.mosaics[0].tiles.len(), 2);
}

#[tokio::test]
async fn the_user_opens_boards_that_the_agent_can_list_and_read() {
    let port = dev_server().await;
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let other = h.session().await;
    put_in_mosaics(&h, &[("Work", &s.id), ("Home", &other.id)]).await;

    // The user shows a few things beside the chat; checked like the agent's.
    let notes: Whiteboard = call(
        &h,
        create_board(
            &s.id,
            "Trip notes",
            BoardContent::Markdown {
                markdown: "# Packing\n- passport\n- charger".into(),
            },
        ),
    )
    .await;
    assert!(notes.by_user && notes.session_id == s.id);
    let tiles = &h.engine.handle(Request::GetMosaics, None).await.unwrap()["mosaics"][0]["tiles"];
    assert_eq!(tiles[1]["board_id"], notes.id.as_str(), "{tiles}");
    let page: Whiteboard = call(
        &h,
        create_board(
            &s.id,
            "Chart",
            BoardContent::Html {
                html:
                    "<html><body><h2>Sales</h2><p>Q3 up 4%</p><script>draw()</script></body></html>"
                        .into(),
            },
        ),
    )
    .await;
    let app: Whiteboard = call(
        &h,
        create_board(
            &s.id,
            "Dev server",
            BoardContent::Local {
                port,
                path: "/status".into(),
            },
        ),
    )
    .await;
    let bad = h
        .engine
        .handle(
            create_board(
                &s.id,
                "x",
                BoardContent::Url {
                    url: "javascript:alert(1)".into(),
                },
            ),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::BadRequest);
    let missing = h
        .engine
        .handle(
            create_board(
                "nope",
                "x",
                BoardContent::Markdown {
                    markdown: "x".into(),
                },
            ),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    // Another chat's board stays out of view until the user puts it here.
    let elsewhere: Whiteboard = call(
        &h,
        create_board(
            &other.id,
            "Elsewhere",
            BoardContent::Markdown {
                markdown: "other chat".into(),
            },
        ),
    )
    .await;

    h.mock.push(Reply::Tools(vec![(
        "whiteboard".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![
        (
            "whiteboard".into(),
            json!({"action": "read", "title": "trip"}),
        ),
        (
            "whiteboard".into(),
            json!({"action": "read", "id": page.id}),
        ),
        ("whiteboard".into(), json!({"action": "read", "id": app.id})),
        (
            "whiteboard".into(),
            json!({"action": "read", "id": elsewhere.id}),
        ),
    ]));
    h.mock.push(Reply::Text("Read them.".into()));
    h.send(&s.id, "what's on my boards?").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert_eq!(results.len(), 5, "{results:?}");
    let list = &results[0].2;
    assert!(
        list.contains(&format!(
            "{} · \"Trip notes\" · markdown · opened by the user · on screen in \"Work\" beside this chat",
            notes.id
        )),
        "{list}"
    );
    assert!(!list.contains("Elsewhere"), "{list}");
    let (read_notes, read_page, read_app) = (&results[1].2, &results[2].2, &results[3].2);
    assert!(read_notes.contains("- passport"), "{read_notes}");
    assert!(
        read_page.contains("Q3 up 4%")
            && read_page.contains("Source (HTML")
            && read_page.contains("draw()"),
        "{read_page}"
    );
    // The local app is read through the server, as its tile loads it.
    assert!(
        read_app.contains("All 12 checks green") && read_app.contains("# Build status"),
        "{read_app}"
    );
    assert!(!read_app.contains("tracker"), "{read_app}");
    assert!(results[4].1, "{:?}", results[4]);

    // Once the user moves it beside this chat, the agent sees it too.
    let mut layout: MosaicLayout = call(&h, Request::GetMosaics).await;
    let tile = layout.mosaics[1].tiles.pop().unwrap();
    assert_eq!(tile.board_id.as_deref(), Some(elsewhere.id.as_str()));
    layout.mosaics[0].tiles.push(tile);
    let _: MosaicLayout = call(&h, Request::SaveMosaics { layout }).await;
    h.mock.push(Reply::Tools(vec![
        ("whiteboard".into(), json!({"action": "list"})),
        (
            "whiteboard".into(),
            json!({"action": "read", "id": elsewhere.id}),
        ),
        // It can't change another chat's board, and its own update of the
        // user's board makes the board its own.
        (
            "whiteboard".into(),
            json!({"action": "show", "id": elsewhere.id, "markdown": "x"}),
        ),
        (
            "whiteboard".into(),
            json!({"action": "show", "id": notes.id, "markdown": "# Packed"}),
        ),
    ]));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "and now?").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let last: Vec<_> = results[5..].to_vec();
    assert!(
        last[0].2.contains(
            "\"Elsewhere\" · markdown · opened by the user · from the chat \"New chat\" · on screen in \"Work\""
        ),
        "{last:?}"
    );
    assert!(!last[1].1 && last[1].2.contains("other chat"), "{last:?}");
    assert!(last[2].1, "{last:?}");
    let updated: Whiteboard = call(
        &h,
        Request::GetBoard {
            id: notes.id.clone(),
        },
    )
    .await;
    assert!(!updated.by_user);
}

#[tokio::test]
async fn reading_a_web_page_board_on_the_lan_needs_approval_like_web_fetch() {
    let port = dev_server().await;
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let b: Whiteboard = call(
        &h,
        create_board(
            &s.id,
            "Router",
            BoardContent::Url {
                url: format!("http://127.0.0.1:{port}/status"),
            },
        ),
    )
    .await;
    h.mock.push(Reply::Tools(vec![(
        "whiteboard".into(),
        json!({"action": "read"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "what does this page say?").await;
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("approval requested");
    assert!(
        request.reason.contains("local or private network"),
        "{request:?}"
    );
    let _: Value = call(
        &h,
        Request::RespondApproval {
            session_id: s.id.clone(),
            request_id: request.id,
            decision: ApprovalDecision::Allow,
        },
    )
    .await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(!results[0].1, "{results:?}");
    assert!(
        results[0].2.contains(&b.id) && results[0].2.contains("All 12 checks green"),
        "{results:?}"
    );
}

#[tokio::test]
async fn image_boards_reach_models_that_see_images_and_are_described_otherwise() {
    use base64::Engine as _;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(640u32.to_be_bytes());
    png.extend(480u32.to_be_bytes());
    png.extend([0u8; 32]);
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let _: Whiteboard = call(
        &h,
        create_board(
            &s.id,
            "Screenshot",
            BoardContent::Image {
                mime: "image/png".into(),
                data: data.clone(),
            },
        ),
    )
    .await;
    let read = || {
        Reply::Tools(vec![(
            "whiteboard".into(),
            json!({"action": "read", "title": "Screenshot"}),
        )])
    };
    // `mock/m` has no vision: a description.
    h.mock.push(read());
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "look").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].2.contains("PNG image, 640×480") && results[0].2.contains("can't see images"),
        "{results:?}"
    );
    // A vision model gets the image itself.
    let _: SessionInfo = call(
        &h,
        Request::UpdateSession {
            session_id: s.id.clone(),
            title: None,
            model: Some(ModelRef {
                provider: "mock".into(),
                model: "m-vision".into(),
            }),
            thinking: None,
            permission: None,
            pinned: None,
            archived: None,
            cwd: None,
            members: None,
            fast: None,
            subagents: None,
        },
    )
    .await;
    h.mock.push(read());
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "look again").await;
    h.settle(&s.id).await;
    let image = h
        .entries(&s.id)
        .await
        .into_iter()
        .rev()
        .find_map(|e| match e.body {
            EntryBody::ToolResult { content, .. } => content.into_iter().find_map(|p| match p {
                ContentPart::Image { data, .. } => Some(data),
                _ => None,
            }),
            _ => None,
        });
    assert_eq!(image.as_deref(), Some(data.as_str()));
}

#[tokio::test]
async fn fast_mode_is_a_session_setting_offered_by_openai_models() {
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    assert!(!s.fast);
    let on: SessionInfo = call(
        &h,
        Request::UpdateSession {
            session_id: s.id.clone(),
            title: None,
            model: None,
            thinking: None,
            permission: None,
            pinned: None,
            archived: None,
            cwd: None,
            members: None,
            fast: Some(true),
            subagents: None,
        },
    )
    .await;
    assert!(on.fast && h.info(&s.id).await.fast);
    // A model without a fast tier ignores it.
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests().pop().unwrap();
    assert!(req.get("service_tier").is_none(), "{req}");
    let providers: Vec<ProviderInfo> = call(&h, Request::ListProviders { refresh: false }).await;
    let fast = |id: &str| {
        providers
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .models
            .iter()
            .all(|m| m.fast)
    };
    assert!(fast("openai-codex"), "{providers:?}");
    assert!(!fast("mock"));
}

#[tokio::test]
async fn model_roles_are_listed_set_and_cleared() {
    let h = harness(vec![], 100_000).await;
    let roles: ModelRoles = call(&h, Request::GetModelRoles).await;
    let names: Vec<&str> = roles.roles.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["default", "utility", "memory"]);
    assert!(roles.roles.iter().all(|r| r.builtin));
    assert_eq!(
        roles.roles[0].resolved.as_ref().unwrap().to_string(),
        "mock/m"
    );
    assert!(roles.roles[2].value.is_none() && roles.roles[2].resolved.is_none());

    let set = |role: &str, model: Option<&str>| Request::SetModelRole {
        role: role.into(),
        model: model.map(str::to_string),
    };
    let roles: ModelRoles = call(&h, set("memory", Some("mock/careful"))).await;
    let memory = &roles.roles[2];
    assert_eq!(memory.value.as_deref(), Some("mock/careful"));
    assert_eq!(
        memory.resolved.as_ref().unwrap().to_string(),
        "mock/careful"
    );
    let saved = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(saved.contains("memory = \"mock/careful\""), "{saved}");
    // A role of the user's own, after Tessera's.
    let roles: ModelRoles = call(&h, set("research", Some("@memory"))).await;
    let research = roles.roles.last().unwrap();
    assert!(!research.builtin && research.name == "research");
    assert_eq!(
        research.resolved.as_ref().unwrap().to_string(),
        "mock/careful"
    );
    // Refused: unknown providers and cycles.
    for bad in [
        set("memory", Some("nowhere/x")),
        set("memory", Some("@research")),
    ] {
        let e = h.engine.handle(bad, None).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest, "{e:?}");
    }
    // Cleared, memory follows @utility, then nothing (the chat's own model).
    let _: ModelRoles = call(&h, set("fast", Some("mock/small"))).await;
    let roles: ModelRoles = call(&h, set("memory", None)).await;
    assert!(roles.roles[2].value.is_none());
    assert_eq!(roles.roles[1].value.as_deref(), Some("mock/small"));
    assert_eq!(
        roles.roles[2].resolved.as_ref().unwrap().to_string(),
        "mock/small"
    );
    let roles: ModelRoles = call(&h, set("utility", Some(""))).await;
    assert!(roles.roles[2].resolved.is_none());
}

/// A harness whose chats are reviewed after every turn.
async fn reviewing_harness(extra: &str) -> Harness {
    let mock = Mock::start(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        "default_model = \"mock/m\"\n{extra}\n[agent]\nreview_interval = 1\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\n",
        mock.url
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Harness {
        engine,
        mock,
        dir,
        work,
        _work_dir: work_dir,
    }
}

#[tokio::test]
async fn the_memory_review_is_a_hidden_helper_on_the_memory_model() {
    let h = reviewing_harness("[models]\nmemory = \"mock/careful\"").await;
    // An existing skill the review should know about.
    let skill = h.dir.path().join("skills/canvas-api");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: canvas-api\ndescription: List courses and assignments through the Canvas API.\n---\nSteps.\n",
    )
    .unwrap();
    const KEY: &str = "You are reviewing a recent conversation";
    h.mock.route(
        KEY,
        Reply::Tools(vec![(
            "memory".into(),
            json!({"action": "add", "target": "user", "content": "Prefers answers in bullet points"}),
        )]),
    );
    h.mock.route(KEY, Reply::Text("Saved.".into()));
    // Some real work first, so the review is offered the skill tools too.
    for n in 1..=4 {
        h.mock.push(Reply::Tools(vec![(
            "bash".into(),
            json!({"command": format!("echo step {n}")}),
        )]));
    }
    h.mock.push(Reply::Text("Sure.".into()));
    let s = h.session().await;
    let _: SessionInfo = call(
        &h,
        Request::UpdateSession {
            session_id: s.id.clone(),
            title: None,
            model: None,
            thinking: None,
            permission: None,
            pinned: None,
            archived: None,
            cwd: None,
            members: None,
            fast: Some(true),
            subagents: None,
        },
    )
    .await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "please use bullet points from now on").await;
    // The parent chat is told what was learned, as before.
    let notice = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = h
                .entries(&s.id)
                .await
                .into_iter()
                .find_map(|e| match e.body {
                    EntryBody::Notice { text, learned, .. } if !learned.is_empty() => Some(text),
                    _ => None,
                });
            if let Some(t) = found {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a learned notice");
    assert_eq!(
        notice,
        "Learned from this conversation — about you: Prefers answers in bullet points"
    );
    // Every client update about the review marks it as a helper.
    let mut review: Option<SessionInfo> = None;
    while let Ok(e) = events.try_recv() {
        if let Event::SessionUpdated { session } = e
            && session.title == "Memory review"
        {
            assert!(session.internal && !session.fast, "{session:?}");
            assert_eq!(session.parent_id.as_deref(), Some(s.id.as_str()));
            review = Some(session);
        }
    }
    let review = review.expect("review session updates");
    let listed: Vec<SessionInfo> = call(&h, Request::ListSessions { archived: false }).await;
    assert!(listed.iter().all(|x| x.id != review.id));
    // Its copy of the chat doesn't show up in search.
    let hits: Vec<SearchHit> = call(
        &h,
        Request::Search {
            query: "reviewing".into(),
            limit: None,
        },
    )
    .await;
    assert!(hits.is_empty(), "{hits:?}");
    // It ran on the memory model, with the criteria and the current skills.
    let requests = h.mock.state.requests.lock().clone();
    let review_req = requests
        .iter()
        .find(|r| r.to_string().contains(KEY))
        .expect("review request");
    assert_eq!(review_req["model"], "careful");
    let prompt = review_req["messages"].to_string();
    for want in [
        "trial and error",
        "is not a fact until it was confirmed",
        "- canvas-api (user): List courses and assignments",
        "Nothing to save",
    ] {
        assert!(prompt.contains(want), "{want} missing");
    }
    // The main chat still ran on its own model.
    assert!(requests.iter().any(|r| r["model"] == "m"));
    // The memory was saved.
    let memory: MemoryState = call(&h, Request::GetMemory).await;
    assert!(memory.user.contains("bullet points"));
}
