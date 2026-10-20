//! Live checks against a local OpenAI-compatible model (ignored by default):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_bots -- --ignored --nocapture

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

async fn engine(dir: &std::path::Path) -> Option<Engine> {
    let model = std::env::var("TESSERA_LIVE_MODEL").ok()?;
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/')?;
    std::fs::write(
        dir.join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
    )
    .ok()?;
    Engine::open(Paths::at(dir), EngineOptions::default())
        .await
        .ok()
}

async fn bot(
    e: &Engine,
    name: &str,
    role: &str,
    tools: ToolPreset,
    can: &[&str],
    perm: PermissionMode,
) -> Bot {
    let b = Bot {
        id: String::new(),
        name: name.into(),
        handle: String::new(),
        avatar: String::new(),
        color: String::new(),
        role: role.into(),
        personality: "Friendly and brief.".into(),
        instructions: String::new(),
        model: None,
        thinking: Some(ThinkingLevel::Low),
        permission: perm,
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
    };
    serde_json::from_value(e.handle(Request::SaveBot { bot: b }, None).await.unwrap()).unwrap()
}

async fn run(e: &Engine, sid: &str, text: &str) -> Vec<Entry> {
    e.handle(
        Request::Send {
            session_id: sid.into(),
            content: vec![ContentPart::text(text)],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(600), e.wait_idle(sid))
        .await
        .expect("timed out");
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
            Request::GetSession {
                session_id: sid.into(),
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    snap.entries
}

fn transcript(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| match &e.body {
            EntryBody::User { content, author } => format!(
                "USER{}: {}",
                author
                    .as_deref()
                    .map(|a| format!("({a})"))
                    .unwrap_or_default(),
                plain_text(content)
            ),
            EntryBody::Assistant {
                content, author, ..
            } => content
                .iter()
                .map(|p| match p {
                    AssistantPart::Text { text } => format!(
                        "{}: {}",
                        author.as_deref().unwrap_or("assistant"),
                        text.trim()
                    ),
                    AssistantPart::ToolCall {
                        name, arguments, ..
                    } => format!(
                        "  → {name} {}",
                        tessera_core::util::truncate(&arguments.to_string(), 160)
                    ),
                    _ => String::new(),
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            EntryBody::ToolResult {
                tool,
                content,
                is_error,
                ..
            } => format!(
                "  ← {tool}{} {}",
                if *is_error { " ERR" } else { "" },
                tessera_core::util::one_line(&plain_text(content), 160)
            ),
            EntryBody::Notice { text, .. } => format!("  [notice] {text}"),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
#[ignore]
async fn live_delegation_and_channel() {
    let dir = tempfile::tempdir().unwrap();
    let Some(e) = engine(dir.path()).await else {
        return;
    };
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let nova = bot(
        &e,
        "Nova",
        "personal assistant",
        ToolPreset::Assistant,
        &["forge"],
        PermissionMode::Default,
    )
    .await;
    let forge = bot(
        &e,
        "Forge",
        "developer bot that writes code and runs commands",
        ToolPreset::Coding,
        &[],
        PermissionMode::Auto,
    )
    .await;

    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.display().to_string()),
                model: None,
                title: None,
                bot_id: Some(nova.id.clone()),
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
    let entries = run(&e, &s.id, "Please have @forge create a file named hello.txt in the working directory containing exactly: hi from forge. Then tell me what Forge reported.").await;
    println!(
        "=== Nova chat ({:.1}s)\n{}",
        t0.elapsed().as_secs_f32(),
        transcript(&entries)
    );
    let file = std::fs::read_to_string(work.join("hello.txt")).unwrap_or_default();
    println!("=== hello.txt: {file:?}");
    assert!(
        file.contains("hi from forge"),
        "Forge should have written the file"
    );
    assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p, AssistantPart::ToolCall { name, .. } if name == "message_bot")))), "Nova should delegate");

    let room: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateChannel {
                title: "War room".into(),
                members: vec!["nova".into(), "forge".into()],
                cwd: Some(work.display().to_string()),
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    let entries = run(&e, &room.id, "@forge in one sentence, what is a good name for a CLI tool that tiles terminals? Then ask @nova whether she agrees.").await;
    println!(
        "=== War room ({:.1}s)\n{}",
        t0.elapsed().as_secs_f32(),
        transcript(&entries)
    );
    let speakers: Vec<String> = entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant {
                author: Some(a),
                content,
                ..
            } if content
                .iter()
                .any(|p| matches!(p, AssistantPart::Text { text } if !text.trim().is_empty())) =>
            {
                Some(a.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        speakers.first() == Some(&forge.id),
        "forge speaks first: {speakers:?}"
    );
    assert!(
        speakers.contains(&nova.id),
        "nova is handed the floor: {speakers:?}"
    );
}
