//! Live check of whiteboards with a real model (ignored by default). The
//! model draws a board; the test serves it and waits so a browser can look:
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b TESSERA_LIVE_HOLD=90 \
//!     cargo test -p tessera-server --test live_board -- --ignored --nocapture

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

#[tokio::test]
#[ignore]
async fn a_model_draws_a_whiteboard_that_renders() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let prompt = std::env::var("TESSERA_LIVE_PROMPT").unwrap_or_else(|_| {
        "On a whiteboard, show me an interactive 3D topology of my home network: an internet cloud, a router, a \
         Proxmox server with two VMs, a NAS, a desktop PC and a phone on Wi-Fi. Label everything and let me rotate it."
            .into()
    });
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 0\npermission = \"auto\"\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
    )
    .unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let router = tessera_server::router(engine.clone(), None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(tessera_server::serve_on(listener, router));
    let s: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(home.path().display().to_string()),
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
    let t0 = std::time::Instant::now();
    engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text(prompt)],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::time::timeout(Duration::from_secs(900), engine.wait_idle(&s.id))
        .await
        .unwrap();
    let boards: Vec<Whiteboard> = serde_json::from_value(
        engine
            .handle(Request::ListBoards { session_id: None }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
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
    for e in &snap.entries {
        if let EntryBody::ToolResult {
            tool,
            content,
            is_error,
            ..
        } = &e.body
        {
            eprintln!(
                "[{tool}{}] {}",
                if *is_error { " ERROR" } else { "" },
                plain_text(content).chars().take(300).collect::<String>()
            );
        }
    }
    eprintln!(
        "took {:.0}s, {} board(s)",
        t0.elapsed().as_secs_f64(),
        boards.len()
    );
    let b = boards.last().expect("the model showed a board");
    if let BoardContent::Html { html } = &b.content {
        std::fs::write(home.path().join("board.html"), html).unwrap();
        let link = engine
            .handle(Request::BoardLink { id: b.id.clone() }, None)
            .await
            .unwrap();
        let page = format!("http://{addr}{}", link["path"].as_str().unwrap());
        eprintln!("BOARD_URL {page}");
        if let Ok(out) = std::env::var("TESSERA_LIVE_URL_FILE") {
            std::fs::write(out, &page).unwrap();
        }
    }
    let hold: u64 = std::env::var("TESSERA_LIVE_HOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    tokio::time::sleep(Duration::from_secs(hold)).await;
}
