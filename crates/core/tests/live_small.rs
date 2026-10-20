//! Live check of model profiles and small model mode against the user's
//! local model (ignored by default; a handful of turns, one at a time on a
//! single-slot server, under the shared lock):
//!   flock /tmp/tessera-local-model.lock env TESSERA_LIVE_MODEL=local/qwen3.8-27b \
//!     TESSERA_LIVE_URL=http://192.168.1.50:1234/v1 \
//!     cargo test -p tessera-core --test live_small -- --ignored --nocapture --test-threads 1
//! `TESSERA_LIVE_DISCOVER=1` lists the server's models (as llama.cpp: the
//! card's top_k/min_p then go too); only when the model under test is the
//! only one loaded (discovery reads `/props` of loaded models).

use std::time::{Duration, Instant};

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

fn live() -> Option<(String, String)> {
    let model = std::env::var("TESSERA_LIVE_MODEL").ok()?;
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    Some((model, url))
}

async fn engine(dir: &std::path::Path, model: &str, url: &str, extra: &str) -> Engine {
    let (provider, id) = model.split_once('/').unwrap();
    let discover = std::env::var("TESSERA_LIVE_DISCOVER").is_ok();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\n[memory]\nenabled = false\n{extra}\n\
             [[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = {discover}\n\
             [[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"
        ),
    )
    .unwrap();
    Engine::open(Paths::at(dir), EngineOptions { scheduler: false })
        .await
        .unwrap()
}

async fn run(e: &Engine, cwd: &std::path::Path, text: &str) -> (SessionInfo, Vec<Entry>) {
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(cwd.display().to_string()),
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
    e.handle(
        Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text(text)],
            delivery: Delivery::Auto,
            attachments: vec![],
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(900), e.wait_idle(&s.id))
        .await
        .expect("turn took over 15 minutes");
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
    (snap.info, snap.entries)
}

fn dump(entries: &[Entry]) -> (usize, usize) {
    let (mut calls, mut errors) = (0, 0);
    for en in entries {
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
                        } => {
                            calls += 1;
                            eprintln!("[assistant] → {name} {arguments}")
                        }
                        AssistantPart::Thinking { .. } => {}
                    }
                }
            }
            EntryBody::ToolResult {
                tool,
                content,
                is_error,
                ..
            } => {
                errors += *is_error as usize;
                eprintln!(
                    "[{tool}{}] {}",
                    if *is_error { " error" } else { "" },
                    tessera_core::util::truncate(&plain_text(content), 200)
                )
            }
            EntryBody::Notice { text, .. } => eprintln!("[notice] {text}"),
            EntryBody::Compaction { .. } => eprintln!("[compaction]"),
        }
    }
    (calls, errors)
}

/// The tool check passes quietly, then a small task in small model mode:
/// a bug fixed with the flat edit form and checked by running it.
#[tokio::test]
#[ignore]
async fn live_small_mode_fixes_a_bug() {
    let Some((model, url)) = live() else { return };
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("calc.py"),
        "def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n",
    )
    .unwrap();
    let e = engine(dir.path(), &model, &url, "").await;
    let started = Instant::now();
    let (info, entries) = run(
        &e,
        work.path(),
        "calc.py has a bug in add(). Fix it, then check with: python3 -c 'import calc; print(calc.add(2, 3))'",
    )
    .await;
    let (calls, errors) = dump(&entries);
    eprintln!(
        "{:.0} s, {calls} calls, {errors} failed, {} steps, usage {:?}",
        started.elapsed().as_secs_f64(),
        info.steps,
        info.usage
    );
    assert!(
        !entries
            .iter()
            .any(|en| matches!(&en.body, EntryBody::Notice { text, .. } if text.contains("didn't use a tool"))),
        "the tool check failed"
    );
    let fixed = std::fs::read_to_string(work.path().join("calc.py")).unwrap();
    assert!(fixed.contains("return a + b"), "{fixed}");
    let providers: Vec<ProviderInfo> = serde_json::from_value(
        e.handle(Request::ListProviders { refresh: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let m = providers
        .iter()
        .flat_map(|p| &p.models)
        .find(|m| model.ends_with(&format!("/{}", m.id)))
        .unwrap();
    assert!(m.small, "small model mode on (Auto, own server)");
}

/// Small model mode off, for comparison (the same task, the full prompt).
#[tokio::test]
#[ignore]
async fn live_full_mode_fixes_a_bug() {
    let Some((model, url)) = live() else { return };
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("calc.py"),
        "def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n",
    )
    .unwrap();
    let e = engine(
        dir.path(),
        &model,
        &url,
        "[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n",
    )
    .await;
    let started = Instant::now();
    let (info, entries) = run(
        &e,
        work.path(),
        "calc.py has a bug in add(). Fix it, then check with: python3 -c 'import calc; print(calc.add(2, 3))'",
    )
    .await;
    let (calls, errors) = dump(&entries);
    eprintln!(
        "{:.0} s, {calls} calls, {errors} failed, {} steps, usage {:?}",
        started.elapsed().as_secs_f64(),
        info.steps,
        info.usage
    );
    let fixed = std::fs::read_to_string(work.path().join("calc.py")).unwrap();
    assert!(fixed.contains("return a + b"), "{fixed}");
}
