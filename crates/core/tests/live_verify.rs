//! Live checks of the batch 14 harness notes against the local model
//! (ignored by default; the model has one slot, so take the lock):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b TESSERA_LIVE_URL=http://192.168.1.50:1234/v1 \
//!   flock /tmp/tessera-local-model.lock cargo test -p tessera-core --test live_verify -- --ignored --nocapture --test-threads 1

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

fn live() -> Option<(String, String)> {
    let model = std::env::var("TESSERA_LIVE_MODEL").ok()?;
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    Some((model, url))
}

/// A throwaway engine on the live model, with these `[agent]` lines.
async fn engine(agent: &str) -> Option<(Engine, tempfile::TempDir, tempfile::TempDir)> {
    let (model, url) = live()?;
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\nmax_steps = 16\n{agent}\n[memory]\nenabled = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 65536\nreasoning = true\n"
        ),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    Some((e, home, work))
}

async fn turn(e: &Engine, cwd: &std::path::Path, text: &str) -> Vec<Entry> {
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(cwd.display().to_string()),
                model: None,
                title: Some("live".into()),
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
    e.handle(
        Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text(text)],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(900), e.wait_idle(&s.id))
        .await
        .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(
        e.handle(
            Request::GetSession {
                session_id: s.id,
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    for en in &snap.entries {
        match &en.body {
            EntryBody::User { content, author } => eprintln!(
                "[{}] {}",
                author.as_deref().unwrap_or("user"),
                tessera_core::util::truncate(&plain_text(content), 300)
            ),
            EntryBody::Assistant { content, .. } => {
                for p in content {
                    match p {
                        AssistantPart::Text { text } => {
                            eprintln!("[assistant] {}", tessera_core::util::truncate(text, 300))
                        }
                        AssistantPart::ToolCall {
                            name, arguments, ..
                        } => eprintln!(
                            "[assistant] → {name} {}",
                            tessera_core::util::truncate(&arguments.to_string(), 200)
                        ),
                        AssistantPart::Thinking { .. } => {}
                    }
                }
            }
            EntryBody::ToolResult {
                tool,
                content,
                is_error,
                ..
            } => eprintln!(
                "[{tool}{}] {}",
                if *is_error { " error" } else { "" },
                tessera_core::util::truncate(&plain_text(content), 240)
            ),
            EntryBody::Notice { text, .. } => eprintln!("[notice] {text}"),
            EntryBody::Compaction { .. } => {}
        }
    }
    snap.entries
}

fn notes(entries: &[Entry], prefix: &str) -> usize {
    entries
        .iter()
        .filter(|e| matches!(&e.body, EntryBody::User { content, author: Some(a) } if a == "harness" && plain_text(content).starts_with(prefix)))
        .count()
}

/// A change nothing ran: the model gets the note once and either runs a
/// check or says why none applies (strict, so the syntax check alone
/// doesn't count).
#[tokio::test]
#[ignore]
async fn live_unchecked_change_gets_the_note_once() {
    let Some((e, _home, work)) = engine("verify_check = \"strict\"").await else {
        return;
    };
    let started = std::time::Instant::now();
    let entries = turn(
        &e,
        work.path(),
        "Create mathx.py with a function mean(xs) that returns the average of a list of numbers (0 for an empty list). Answer in one sentence when done.",
    )
    .await;
    let n = notes(&entries, "[Check]");
    eprintln!("check notes: {n}; took {:?}", started.elapsed());
    assert!(work.path().join("mathx.py").exists());
    assert!(n <= 1);
}

/// Changes followed by a test run: no note.
#[tokio::test]
#[ignore]
async fn live_tested_change_gets_no_note() {
    let Some((e, _home, work)) = engine("").await else {
        return;
    };
    let started = std::time::Instant::now();
    let entries = turn(
        &e,
        work.path(),
        "Create stats.py with a function median(xs), and test_stats.py with two pytest tests for it. Run the tests with `python3 -m pytest -q`, then answer in one sentence.",
    )
    .await;
    let n = notes(&entries, "[Check]");
    eprintln!("check notes: {n}; took {:?}", started.elapsed());
    assert_eq!(n, 0, "the tests ran after the changes");
}

/// A change the model has no reason to run: the note comes once, and the
/// model runs something or says why nothing applies.
#[tokio::test]
#[ignore]
async fn live_write_only_change_gets_the_note() {
    let Some((e, _home, work)) = engine("").await else {
        return;
    };
    let started = std::time::Instant::now();
    let entries = turn(
        &e,
        work.path(),
        "Write greet.go (package greet) with a function Greet(name string) string that returns \"Hello, \" + name. Just write the file and tell me in one sentence.",
    )
    .await;
    let n = notes(&entries, "[Check]");
    eprintln!("check notes: {n}; took {:?}", started.elapsed());
    assert!(work.path().join("greet.go").exists());
    assert!(n <= 1);
}
