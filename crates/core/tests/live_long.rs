//! Live long-horizon check against a local model (ignored by default).
//!
//! A multi-file coding task under a deliberately tiny context cap, so the
//! session has to compact several times and still finish correctly and keep
//! the user's standing constraints:
//!
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_long -- --ignored --nocapture

use std::time::{Duration, Instant};

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

const TASK: &str = "\
Build a small Python package in this directory, step by step:
1. `inventory/models.py`: an `Item` dataclass (name: str, qty: int, price: float) that rejects negative qty/price and empty names.
2. `inventory/store.py`: an `Inventory` class with add(item), remove(name, qty), total_value(), low_stock(threshold) and to_json()/from_json(text).
3. `inventory/report.py`: `format_report(inv)` returning an aligned plain-text table with a total line.
4. `inventory/cli.py`: argparse commands add/remove/list/report that load and save `inventory.json`.
5. Tests for every module in `tests/` using the standard library `unittest` (pytest is not installed). Run `python3 -m unittest discover -s tests -v` and fix failures until everything passes.
6. Finally write `NOTES.md` listing every public function and method with a one-line description.

Standing rules for the whole task: the very first line of EVERY Python file must be exactly `# owner: kinder`. Use type hints everywhere. No third-party dependencies. Work through the steps in order and keep going until all six are done; don't stop to ask me anything.";

async fn engine(dir: &std::path::Path, cap: u64) -> Option<Engine> {
    let model = std::env::var("TESSERA_LIVE_MODEL").ok()?;
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/')?;
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\n[context]\nmax_tokens = {cap}\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"
        ),
    )
    .ok()?;
    Engine::open(Paths::at(dir), EngineOptions::default())
        .await
        .ok()
}

async fn snapshot(e: &Engine, sid: &str) -> SessionSnapshot {
    serde_json::from_value(
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
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn long_task_survives_repeated_compaction() {
    let home = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().to_path_buf();
    let cap: u64 = std::env::var("TESSERA_LIVE_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(14_000);
    let Some(e) = engine(home.path(), cap).await else {
        eprintln!("set TESSERA_LIVE_MODEL to run");
        return;
    };
    let info: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.display().to_string()),
                model: None,
                title: Some("long horizon".into()),
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
        Request::UpdateSession {
            session_id: info.id.clone(),
            title: None,
            model: None,
            thinking: Some(ThinkingLevel::Low),
            permission: Some(PermissionMode::Auto),
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

    let started = Instant::now();
    e.handle(
        Request::Send {
            session_id: info.id.clone(),
            content: vec![ContentPart::text(TASK)],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Nudge once if the model stops early (as a user would), then judge.
    for round in 0..3 {
        tokio::time::timeout(Duration::from_secs(40 * 60), e.wait_idle(&info.id))
            .await
            .expect("timed out");
        let done = work.join("NOTES.md").exists() && tests_pass(&work);
        if done || round == 2 {
            break;
        }
        eprintln!("-- nudge {round}");
        e.handle(
            Request::Send {
                session_id: info.id.clone(),
                content: vec![ContentPart::text(
                    "Please continue until all six steps are done and the tests pass.",
                )],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let snap = snapshot(&e, &info.id).await;
    let compactions: Vec<(u64, u64, usize)> = snap
        .entries
        .iter()
        .filter_map(|x| match &x.body {
            EntryBody::Compaction {
                tokens_before,
                tokens_after,
                summary,
                ..
            } => Some((*tokens_before, *tokens_after, summary.len())),
            _ => None,
        })
        .collect();
    let tools = snap
        .entries
        .iter()
        .filter(|x| matches!(x.body, EntryBody::ToolResult { .. }))
        .count();
    let py: Vec<_> = walk(&work)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "py"))
        .collect();
    let unowned: Vec<_> = py
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|t| t.lines().next() != Some("# owner: kinder"))
                .unwrap_or(true)
        })
        .collect();
    let notes = std::fs::read_to_string(work.join("NOTES.md")).unwrap_or_default();
    let passing = tests_pass(&work);

    eprintln!("\n=== long-horizon report ===");
    eprintln!(
        "time: {:.1} min · tool calls: {tools} · entries: {}",
        started.elapsed().as_secs_f64() / 60.0,
        snap.entries.len()
    );
    eprintln!(
        "context cap: {cap} · compactions: {} {:?}",
        compactions.len(),
        compactions
    );
    eprintln!(
        "python files: {} · missing owner line: {:?}",
        py.len(),
        unowned
    );
    eprintln!("tests pass: {passing} · NOTES.md: {} bytes", notes.len());
    eprintln!(
        "final context: {} / {} tokens",
        snap.info.context.tokens, snap.info.context.window
    );

    assert!(
        compactions.len() >= 2,
        "the cap should force several compactions"
    );
    assert!(
        compactions.iter().all(|(before, after, _)| after < before),
        "compaction must shrink the context"
    );
    assert!(passing, "the task should end with passing tests");
    assert!(
        unowned.is_empty(),
        "the standing rule survived every compaction"
    );
    for f in ["total_value", "low_stock", "format_report"] {
        assert!(notes.contains(f), "NOTES.md covers {f}");
    }
}

fn tests_pass(work: &std::path::Path) -> bool {
    std::process::Command::new("python3")
        .args(["-m", "unittest", "discover", "-s", "tests"])
        .current_dir(work)
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stderr).contains("OK"))
        .unwrap_or(false)
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n != "__pycache__" && n != ".git")
            {
                out.extend(walk(&p));
            }
        } else {
            out.push(p);
        }
    }
    out
}
