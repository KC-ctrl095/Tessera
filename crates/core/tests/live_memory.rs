//! Live check that memory learns and is used, without bloat (ignored by default):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_memory -- --ignored --nocapture

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

async fn say(e: &Engine, sid: &str, text: &str) -> String {
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(600), e.wait_idle(sid))
        .await
        .unwrap();
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
        .unwrap_or_default()
}

async fn memory(e: &Engine) -> MemoryState {
    serde_json::from_value(e.handle(Request::GetMemory, None).await.unwrap()).unwrap()
}

async fn wait_for_memory(e: &Engine, before: &MemoryState) -> MemoryState {
    for _ in 0..240 {
        let m = memory(e).await;
        if m.user != before.user || m.memory != before.memory {
            // Let the review finish its remaining edits.
            tokio::time::sleep(Duration::from_secs(8)).await;
            return memory(e).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    memory(e).await
}

#[tokio::test]
#[ignore]
async fn memory_learns_from_use_and_stays_lean() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 2\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let new_chat = || async {
        let s: SessionInfo = serde_json::from_value(
            e.handle(
                Request::CreateSession {
                    cwd: Some(work.path().display().to_string()),
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
        s.id
    };

    // Chat 1: the user reveals durable facts and a preference in passing.
    let a = new_chat().await;
    say(&e, &a, "Hey, I'm Alex. My homelab runs Proxmox on a box called 'build-box' and I keep my projects under /srv/Coding projects. Quick question: what's a good way to back up a Postgres database nightly?").await;
    let before = memory(&e).await;
    say(&e, &a, "Thanks. For the future: I like short answers with bullet points, and I use fish shell, not bash, so give me fish syntax.").await;
    let learned = wait_for_memory(&e, &before).await;
    eprintln!(
        "\n=== USER.md ===\n{}\n=== MEMORY.md ===\n{}\n",
        learned.user, learned.memory
    );
    let profile = learned.user.to_lowercase();
    assert!(profile.contains("alex"), "learned the name");
    assert!(profile.contains("fish"), "learned the shell");
    assert!(
        profile.contains("proxmox") || profile.contains("build-box"),
        "learned the homelab"
    );

    // Chat 2: a fresh session uses it without being told again.
    let b = new_chat().await;
    let answer = say(
        &e,
        &b,
        "Give me a one-liner to list the 5 biggest files in my projects folder.",
    )
    .await;
    eprintln!("=== new chat answer ===\n{answer}\n");
    let low = answer.to_lowercase();
    assert!(
        low.contains("/srv/coding projects") || low.contains("coding projects"),
        "used the projects path"
    );

    // Repeating the same facts must not duplicate them.
    let size_before = learned.user.len() + learned.memory.len();
    let c = new_chat().await;
    say(
        &e,
        &c,
        "Reminder: I'm Alex, I use fish, my Proxmox box is build-box. What's 2+2?",
    )
    .await;
    let again = say(&e, &c, "Cool, thanks.").await;
    let _ = again;
    tokio::time::sleep(Duration::from_secs(20)).await;
    let after = memory(&e).await;
    eprintln!(
        "=== after repeat ===\nUSER.md {} chars (was {}), MEMORY.md {} chars\n{}",
        after.user.len(),
        learned.user.len(),
        after.memory.len(),
        after.user
    );
    let entries: Vec<&str> = after.user.split("\n§\n").map(str::trim).collect();
    let mut unique = entries.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), entries.len(), "no duplicate entries");
    assert!(
        after.user.to_lowercase().matches("fish shell").count() <= 1,
        "facts are not repeated"
    );
    assert!(
        after.user.len() + after.memory.len() <= size_before + 400,
        "no bloat from repetition"
    );
    assert!(
        after.user.chars().count() as u32 <= after.user_limit
            && after.memory.chars().count() as u32 <= after.memory_limit
    );
}
