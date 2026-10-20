//! Live check of the background review's judgement (ignored by default). The
//! chat itself is scripted; only the review runs on the real model (the
//! `memory` role):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_review -- --ignored --nocapture

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

fn bash(output: &str) -> Reply {
    Reply::Tools(vec![(
        "bash".into(),
        json!({ "command": format!("cat <<'EOF'\n{output}\nEOF") }),
    )])
}

#[tokio::test]
#[ignore]
async fn the_review_keeps_verified_procedures_and_drops_unconfirmed_diagnoses() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let mock = Mock::start(vec![]).await;
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"mock/m\"\n[models]\nmemory = \"{model}\"\n[agent]\nreview_interval = 2\n\
             [tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n\
             [[provider.models]]\nid = \"m\"\ncontext_window = 100000\n\
             [[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n\
             [[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n",
            mock.url
        ),
    )
    .unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let h = Harness {
        engine,
        mock,
        work: work.path().to_path_buf(),
        dir: home,
        _work_dir: work,
    };
    // A weaker agent guesses wrong, is corrected, then works the procedure out.
    for r in [
        bash("GET https://canvas.example.edu/api/v1/assignments\nHTTP 404 {\"errors\":[{\"message\":\"The specified resource does not exist.\"}]}"),
        bash("GET https://canvas.example.edu/api/v1/users/self/assignments\nHTTP 404 {\"errors\":[{\"message\":\"The specified resource does not exist.\"}]}"),
        Reply::Text("Both endpoints return 404, so API access seems to be disabled for your Canvas account.".into()),
        bash("GET https://canvas.example.edu/api/v1/courses?enrollment_state=active&per_page=100\nHTTP 200\nLink: <https://canvas.example.edu/api/v1/courses?enrollment_state=active&page=2&per_page=100>; rel=\"next\"\n[{\"id\":101,\"name\":\"Biology\"},{\"id\":102,\"name\":\"History\"}]"),
        bash("GET https://canvas.example.edu/api/v1/courses?enrollment_state=active&page=2&per_page=100\nHTTP 200\n[{\"id\":103,\"name\":\"Statistics\"}]"),
        bash("GET https://canvas.example.edu/api/v1/courses/101/assignments?bucket=upcoming&per_page=100\nHTTP 200\n[{\"name\":\"Lab report\",\"due_at\":\"2026-10-02T04:59:00Z\"}]\nGET https://canvas.example.edu/api/v1/courses/103/assignments?bucket=upcoming&per_page=100\nHTTP 200\n[{\"name\":\"Problem set 4\",\"due_at\":\"2026-10-03T04:59:00Z\"}]"),
        Reply::Text("You were right, the API works. Due this week: Lab report (Biology), Oct 1 at 11:59 PM, and Problem set 4 (Statistics), Oct 2 at 11:59 PM, Central time. How it works: list active courses with GET /api/v1/courses?enrollment_state=active&per_page=100 and follow the Link header's rel=\"next\" (there were 2 pages), then GET /api/v1/courses/<id>/assignments?bucket=upcoming for each course. due_at is UTC, so convert to America/Chicago.".into()),
    ] {
        h.mock.push(r);
    }
    let s = h.session().await;
    h.send(
        &s.id,
        "Use my Canvas token to list my assignments due this week.",
    )
    .await;
    h.settle(&s.id).await;
    h.send(
        &s.id,
        "API access is not disabled, my other scripts use it. Assignments are per course: list my courses first. I'm in Chicago.",
    )
    .await;
    h.settle(&s.id).await;

    // Wait for the review to finish (its notice, or its session going idle).
    let mut notice = None;
    for _ in 0..600 {
        notice = h
            .entries(&s.id)
            .await
            .into_iter()
            .find_map(|e| match e.body {
                EntryBody::Notice { text, learned, .. } if !learned.is_empty() => Some(text),
                _ => None,
            });
        if notice.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let memory: MemoryState =
        serde_json::from_value(h.engine.handle(Request::GetMemory, None).await.unwrap()).unwrap();
    let skills = std::fs::read_dir(h.dir.path().join("skills"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| std::fs::read_to_string(e.path().join("SKILL.md")).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    eprintln!(
        "\n=== notice ===\n{notice:?}\n=== USER.md ===\n{}\n=== MEMORY.md ===\n{}\n=== skills ===\n{}\n",
        memory.user,
        memory.memory,
        skills.join("\n---\n")
    );
    let saved = format!("{}\n{}\n{}", memory.user, memory.memory, skills.join("\n")).to_lowercase();
    // Warning against it ("don't conclude the API is disabled") is fine;
    // claiming it isn't.
    for line in saved.lines().filter(|l| l.contains("disabled")) {
        assert!(
            ["not ", "n't", "no "].iter().any(|n| line.contains(n)),
            "saved the unconfirmed diagnosis: {line}"
        );
    }
    assert!(
        !skills.is_empty(),
        "the worked-out procedure became a skill"
    );
    assert!(
        saved.contains("courses") && saved.contains("next"),
        "kept the working procedure (with pagination)"
    );
}
