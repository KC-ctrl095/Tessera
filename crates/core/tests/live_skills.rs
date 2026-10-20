//! Live evaluation of skill creation (ignored by default): scripted chats, the
//! background review on a real model. Trivial chats must not end in a skill;
//! chats that worked out a reusable procedure should.
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_skills -- --ignored --nocapture
//! `SKILL_EVAL_CASES=a,b` runs only those cases; `SKILL_EVAL_REPORT_ONLY=1`
//! prints the table without asserting (to compare prompt versions).

mod common;

use std::time::Duration;

use common::skill_cases::{Case, cases};
use common::*;
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

struct Outcome {
    attempts: usize,
    created: Vec<String>,
    refused: usize,
    memory_writes: usize,
    review_ran: bool,
}

async fn run_case(case: &Case, model: &str, url: &str) -> Outcome {
    let (provider, id) = model.split_once('/').unwrap();
    let mock = Mock::start(vec![]).await;
    let home = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    for (f, text) in case.files {
        let p = work.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"mock/m\"\n[models]\nmemory = \"{model}\"\n[agent]\nreview_interval = {}\npermission = \"auto\"\n\
             [tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n\
             [[provider.models]]\nid = \"m\"\ncontext_window = 100000\n\
             [[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n\
             [[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n",
            case.turns.len(),
            mock.url
        ),
    )
    .unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let h = Harness {
        engine,
        mock,
        work,
        dir: home,
        _work_dir: work_dir,
    };
    let mut events = h.engine.subscribe();
    let s = h.session().await;
    for (user, steps) in &case.turns {
        for step in steps {
            h.mock.push(step.reply());
        }
        h.send(&s.id, user).await;
        h.settle(&s.id).await;
    }
    // The review starts right after the last turn.
    let mut review: Option<String> = None;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(8) && review.is_none() {
        while let Ok(e) = events.try_recv() {
            if let Event::SessionUpdated { session } = e
                && session.title == "Memory review"
            {
                review = Some(session.id);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut out = Outcome {
        attempts: 0,
        created: Vec::new(),
        refused: 0,
        memory_writes: 0,
        review_ran: review.is_some(),
    };
    if let Some(rid) = review {
        tokio::time::timeout(Duration::from_secs(900), h.engine.wait_idle(&rid))
            .await
            .expect("the review did not finish");
        let entries = h.entries(&rid).await;
        let mut creates = std::collections::HashMap::new();
        for e in &entries {
            if let EntryBody::Assistant { content, .. } = &e.body {
                for p in content {
                    if let AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                    } = p
                    {
                        if name == "skill_manage" && arguments["action"] == "create" {
                            out.attempts += 1;
                            creates.insert(id.clone(), arguments["name"].to_string());
                        }
                        if name == "memory" && arguments["action"] != "remove" {
                            out.memory_writes += 1;
                        }
                    }
                }
            }
        }
        for e in &entries {
            if let EntryBody::ToolResult {
                call_id, is_error, ..
            } = &e.body
                && let Some(name) = creates.get(call_id)
            {
                if *is_error {
                    out.refused += 1;
                } else {
                    out.created.push(name.trim_matches('"').to_string());
                }
            }
        }
    }
    out
}

#[tokio::test]
#[ignore]
async fn trivial_chats_make_no_skills_and_worked_out_procedures_do() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let only: Vec<String> = std::env::var("SKILL_EVAL_CASES")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let mut wrong = Vec::new();
    eprintln!(
        "\n{:<16} {:<9} {:>8} {:>8} {:>8} {:>7}  created",
        "case", "expected", "attempts", "refused", "created", "memory"
    );
    for case in cases()
        .into_iter()
        .filter(|c| only.is_empty() || only.iter().any(|o| o == c.name))
    {
        let started = std::time::Instant::now();
        let o = run_case(&case, &model, &url).await;
        eprintln!(
            "{:<16} {:<9} {:>8} {:>8} {:>8} {:>7}  {:?} ({}s{})",
            case.name,
            if case.reusable { "skill" } else { "none" },
            o.attempts,
            o.refused,
            o.created.len(),
            o.memory_writes,
            o.created,
            started.elapsed().as_secs(),
            if o.review_ran { "" } else { ", no review" }
        );
        if case.reusable != !o.created.is_empty() {
            wrong.push(case.name);
        }
    }
    if std::env::var_os("SKILL_EVAL_REPORT_ONLY").is_some() {
        return;
    }
    assert!(wrong.is_empty(), "wrong outcome for {wrong:?}");
}
