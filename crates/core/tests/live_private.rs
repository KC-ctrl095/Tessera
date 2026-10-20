//! Live check of private chats against a local model (ignored by default):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_private -- --ignored --nocapture

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

#[tokio::test]
#[ignore]
async fn private_chat_searches_the_web_and_leaves_nothing_behind() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 0\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    if let Ok(url) = std::env::var("TESSERA_LIVE_SEARXNG_URL") {
        let headers = std::env::var("TESSERA_LIVE_SEARXNG_HEADERS").unwrap_or_default();
        let headers = headers
            .split(';')
            .filter_map(|h| h.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        e.handle(
            Request::SaveWebSearch {
                search: "searxng".into(),
                searxng_url: Some(url),
                headers: Some(headers),
            },
            None,
        )
        .await
        .unwrap();
    }
    e.handle(
        Request::SetMemory {
            target: MemoryTarget::User,
            content: "The user's name is Quillon.".into(),
        },
        None,
    )
    .await
    .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
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
        .unwrap(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    e.handle(Request::Send { session_id: s.id.clone(), content: vec![ContentPart::text("What's the latest stable Rust release and when did it come out? Search the web and cite your source. Also: what's my name?")], delivery: Delivery::Auto, attachments: Vec::new() }, None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(600), e.wait_idle(&s.id))
        .await
        .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
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
    let mut searched = false;
    for en in &snap.entries {
        match &en.body {
            EntryBody::ToolResult {
                tool,
                is_error,
                content,
                ..
            } => {
                let t = plain_text(content);
                eprintln!(
                    "[{tool}{}] {}",
                    if *is_error { " ERROR" } else { "" },
                    t[..t.len().min(300)].replace('\n', " ")
                );
                searched |= tool == "web_search" && !is_error;
            }
            EntryBody::Assistant { content, .. } => {
                for p in content {
                    if let AssistantPart::Text { text } = p {
                        eprintln!("\n=== answer ===\n{text}");
                    }
                }
            }
            _ => {}
        }
    }
    eprintln!(
        "title: {} · {:.1}s",
        snap.info.title,
        started.elapsed().as_secs_f64()
    );
    let answer = snap
        .entries
        .iter()
        .rev()
        .find_map(|en| match &en.body {
            EntryBody::Assistant { content, .. } => content.iter().find_map(|p| {
                if let AssistantPart::Text { text } = p {
                    Some(text.clone())
                } else {
                    None
                }
            }),
            _ => None,
        })
        .unwrap_or_default();
    assert!(searched, "web_search worked");
    assert!(answer.contains("http"), "cites a source");
    assert!(
        !answer.contains("Quillon"),
        "private chats know nothing from memory"
    );
    e.handle(
        Request::DeleteSession {
            session_id: s.id.clone(),
        },
        None,
    )
    .await
    .unwrap();
    let db = std::fs::read(home.path().join("tessera.db")).unwrap();
    let wal = std::fs::read(home.path().join("tessera.db-wal")).unwrap_or_default();
    assert!(
        !db.windows(4).chain(wal.windows(4)).any(|w| w == b"Rust"),
        "nothing on disk"
    );
}
