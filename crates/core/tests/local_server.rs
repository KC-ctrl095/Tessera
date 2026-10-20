//! A model server on this computer or network that answers one request at a
//! time (llama.cpp as one user runs it): Tessera's own work for a chat waits
//! for the chat's turn, checkpoints are written on the spot (in place, from
//! the chat's cached prompt, where the server allows), subagents take turns,
//! and a turn that may call no tool keeps its tools listed.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// The mock as a local server: `provider` lines go into its `[[provider]]`
/// table (`parallel = 4`, `discover = true`…), `model` into its model's.
async fn local(
    script: Vec<Reply>,
    window: u64,
    provider: &str,
    model: &str,
    agent: &str,
) -> Harness {
    let mock = Mock::start(script).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        "default_model = \"mock/m\"\n[agent]\nreview_interval = 0\n{agent}\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nname = \"Mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\n{provider}\n[[provider.models]]\nid = \"m\"\ncontext_window = {window}\nmax_output = 2000\n{model}\n",
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

fn system(req: &Value) -> &str {
    req["messages"][0]["content"].as_str().unwrap_or("")
}

fn first_user(req: &Value) -> String {
    req["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "user"))
        .map(|m| m["content"].to_string())
        .unwrap_or_default()
}

fn titles_asked(h: &Harness) -> usize {
    h.mock
        .state
        .requests
        .lock()
        .iter()
        .filter(|r| system(r).starts_with("You name conversations"))
        .count()
}

async fn settle_long(h: &Harness, id: &str) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(90), h.engine.wait_idle(id))
        .await
        .expect("session did not become idle");
}

#[tokio::test]
async fn the_title_waits_for_the_turn_on_a_server_that_takes_one_request() {
    for parallel in [false, true] {
        let h = local(
            vec![Reply::Slow(Duration::from_millis(1500), "Done.".into())],
            100_000,
            if parallel { "parallel = 4" } else { "" },
            "",
            "",
        )
        .await;
        let s = h.session().await;
        h.send(&s.id, "Plan the move of the NAS backups").await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        let info = h.info(&s.id).await;
        assert_eq!(info.status, SessionStatus::Running);
        if parallel {
            // Several at once: named by the model alongside the turn.
            assert_eq!(titles_asked(&h), 1);
            continue;
        }
        // Named from the message at once; the model isn't asked mid-turn.
        assert_eq!(info.title, "Plan the move of the NAS backups");
        assert_eq!(titles_asked(&h), 0);
        h.settle(&s.id).await;
        for _ in 0..100 {
            if h.info(&s.id).await.title == "Test Title" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(h.info(&s.id).await.title, "Test Title");
        let order: Vec<bool> = h
            .mock
            .state
            .requests
            .lock()
            .iter()
            .map(|r| system(r).starts_with("You name conversations"))
            .collect();
        assert_eq!(order, [false, true], "the title after the turn");
    }
}

#[tokio::test]
async fn a_user_rename_during_the_turn_is_kept() {
    let h = local(
        vec![Reply::Slow(Duration::from_millis(1200), "Done.".into())],
        100_000,
        "",
        "",
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "Sort the photos").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
                title: Some("My photos".into()),
                model: None,
                thinking: None,
                permission: None,
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
    h.settle(&s.id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.info(&s.id).await.title, "My photos");
}

/// A step that grows the context, and takes a little while.
fn step(i: usize, pad: usize) -> Reply {
    Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("sleep 0.05; echo step{i} # {}", "x".repeat(pad))}),
    )])
}

#[tokio::test]
async fn checkpoints_are_written_in_place_between_steps() {
    let mut script: Vec<Reply> = (0..30).map(|i| step(i, 4_000)).collect();
    script.push(Reply::Text("finished".into()));
    // llama.cpp (as the server lists itself), a model that doesn't reason.
    let h = local(script, 40_000, "discover = true", "reasoning = false", "").await;
    h.mock
        .state
        .summary_delay_ms
        .store(300, std::sync::atomic::Ordering::Relaxed);
    let s = h.session().await;
    h.send(&s.id, "build it; remember marker-QQ").await;
    settle_long(&h, &s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.body, EntryBody::Compaction { .. })),
        "a checkpoint"
    );
    // Never alongside the agent's steps.
    let timeline = h.mock.state.timeline.lock().clone();
    for (i, t) in timeline.iter().enumerate() {
        if *t == "summary" {
            assert_eq!(timeline.get(i + 1), Some(&"summary done"), "{timeline:?}");
        }
    }
    // Asked as the chat's next reply: its own request so far (the server
    // has it cached), tools listed but not to be called.
    let requests = h.mock.state.requests.lock().clone();
    let mut in_place = 0;
    for (i, r) in requests.iter().enumerate() {
        assert!(
            !system(r).starts_with("You write context checkpoints"),
            "no transcript for the summariser"
        );
        if !in_place_checkpoint(r) {
            continue;
        }
        in_place += 1;
        assert_eq!(r["tool_choice"], "none");
        let before = requests[..i]
            .iter()
            .rfind(|p| !in_place_checkpoint(p) && !system(p).starts_with("You name"))
            .unwrap();
        assert_eq!(r["tools"], before["tools"]);
        let mine = r["messages"].as_array().unwrap();
        let theirs = before["messages"].as_array().unwrap();
        let n = mine.len() - 1;
        assert!(n > 2 && n <= theirs.len());
        assert_eq!(mine[..n], theirs[..n], "a prefix of the chat's own request");
    }
    assert!(in_place >= 1);
    let last = h.mock.agent_requests().pop().unwrap();
    let text = first_user(&last);
    assert!(text.contains("[Context checkpoint]"), "{text}");
    assert!(text.contains("marker-QQ"));
}

#[tokio::test]
async fn subagents_take_turns_on_a_server_that_takes_one_request() {
    for parallel in [true, false] {
        let h = local(
            vec![
                Reply::Tools(vec![(
                    "agent".into(),
                    json!({"wait": true, "tasks": [
                        {"name": "a", "prompt": "look here <TA1>"},
                        {"name": "b", "prompt": "look there <TB1>"}
                    ]}),
                )]),
                Reply::Text("both done".into()),
            ],
            100_000,
            if parallel { "parallel = 4" } else { "" },
            "",
            "",
        )
        .await;
        h.mock.route(
            "<TA1>",
            Reply::Slow(Duration::from_millis(800), "report A".into()),
        );
        h.mock.route(
            "<TB1>",
            Reply::Slow(Duration::from_millis(800), "report B".into()),
        );
        let s = h.session().await;
        h.send(&s.id, "go").await;
        let children = || {
            h.mock
                .agent_requests()
                .iter()
                .filter(|r| {
                    let u = first_user(r);
                    u.contains("<TA1>") || u.contains("<TB1>")
                })
                .count()
        };
        for _ in 0..200 {
            if children() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            children(),
            if parallel { 2 } else { 1 },
            "parallel = {parallel}"
        );
        h.settle(&s.id).await;
        let out = &tool_results(&h.entries(&s.id).await)[0].2;
        assert!(
            out.contains("report A") && out.contains("report B"),
            "{out}"
        );
    }
}

#[tokio::test]
async fn a_subagent_out_of_budget_keeps_its_tools_listed() {
    // On llama.cpp: `tool_choice: none`, the same tools (the prompt stays
    // cached); a server not known to honour it gets no tools, as before.
    for llama in [true, false] {
        let h = local(
            vec![
                Reply::Tools(vec![(
                    "agent".into(),
                    json!({"wait": true, "tasks": [{"name": "a", "prompt": "count <BUD1>"}]}),
                )]),
                Reply::Text("done".into()),
            ],
            100_000,
            if llama { "discover = true" } else { "" },
            "",
            "subagent_steps = 1",
        )
        .await;
        h.mock.route(
            "<BUD1>",
            Reply::Tools(vec![("bash".into(), json!({"command": "echo one"}))]),
        );
        h.mock.route("<BUD1>", Reply::Text("the report".into()));
        let s = h.session().await;
        h.send(&s.id, "go").await;
        h.settle(&s.id).await;
        let mine: Vec<Value> = h
            .mock
            .agent_requests()
            .into_iter()
            .filter(|r| first_user(r).contains("<BUD1>"))
            .collect();
        assert_eq!(mine.len(), 2, "llama = {llama}");
        assert!(mine[1].to_string().contains("[Budget reached]"));
        if llama {
            assert_eq!(mine[1]["tool_choice"], "none");
            assert_eq!(mine[1]["tools"], mine[0]["tools"]);
        } else {
            assert!(mine[1].get("tool_choice").is_none());
            assert!(mine[1]["tools"].as_array().is_none_or(|t| t.is_empty()));
        }
        let out = &tool_results(&h.entries(&s.id).await)[0].2;
        assert!(out.contains("the report"), "{out}");
    }
}

/// No hidden extract calls on the chat's own single-slot server, and no
/// second head/tail cut that loses page text and its continuation offset.
#[tokio::test]
async fn page_extraction_on_our_own_slot_returns_a_bounded_page_without_extra_calls() {
    let page = "A page line with café and facts.\n".repeat(2_000);
    let app = axum::Router::new().route(
        "/page",
        axum::routing::get(move || {
            let page = page.clone();
            async move { page }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/page", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let h = local(
        vec![
            Reply::Tools(vec![(
                "web_fetch".into(),
                json!({
                    "url": url, "prompt": "What facts are here?"
                }),
            )]),
            Reply::Text("Read the page.".into()),
        ],
        100_000,
        "",
        "",
        "permission = \"auto\"",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "read the facts").await;
    h.settle(&s.id).await;
    let result = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(!result.1, "{}", result.2);
    assert!(result.2.contains("Extraction skipped"), "{}", result.2);
    assert!(result.2.contains("Continue with offset="));
    assert!(!result.2.contains("Output truncated") && !result.2.contains("Full output"));
    assert!(result.2.len() < 14_400, "{} bytes", result.2.len());
    let requests = h.mock.agent_requests();
    assert_eq!(
        requests.len(),
        2,
        "tool step + answer, zero extraction requests"
    );
    assert_eq!(
        serde_json::to_vec(&requests[0]["messages"]).unwrap(),
        serde_json::to_vec(
            &requests[1]["messages"].as_array().unwrap()
                [..requests[0]["messages"].as_array().unwrap().len()]
        )
        .unwrap()
    );
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
    println!(
        "own-slot web: requests=2, extract_requests=0, result_bytes={}, prefix=identical",
        result.2.len()
    );
    server.abort();
}

#[tokio::test]
async fn a_separate_utility_provider_can_still_extract_during_the_turn() {
    let util = Mock::start(vec![Reply::Text("The release codename is Tea.".into())]).await;
    let page = "Release notes and details. ".repeat(300);
    let app = axum::Router::new().route(
        "/page",
        axum::routing::get(move || {
            let page = page.clone();
            async move { page }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/page", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut h = local(
        vec![
            Reply::Tools(vec![(
                "web_fetch".into(),
                json!({
                    "url": url, "prompt": "What is the codename?"
                }),
            )]),
            Reply::Text("Tea.".into()),
        ],
        100_000,
        "",
        "",
        "permission = \"auto\"",
    )
    .await;
    let config_path = h.dir.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(&config_path, format!("utility_model = \"util/m\"\n{config}\n[[provider]]\nid = \"util\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n", util.url)).unwrap();
    h.engine = Engine::open(Paths::at(h.dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let s = h.session().await;
    h.send(&s.id, "read the release notes").await;
    h.settle(&s.id).await;
    let result = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        !result.1 && result.2.contains("written by a helper model"),
        "{}",
        result.2
    );
    assert_eq!(h.mock.agent_requests().len(), 2);
    assert_eq!(
        util.state
            .requests
            .lock()
            .iter()
            .filter(|r| system(r).starts_with("You extract"))
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn switching_models_updates_facts_once_without_rewriting_history() {
    let h = local(
        vec![
            Reply::Text("one".into()),
            Reply::Text("two".into()),
            Reply::Text("three".into()),
        ],
        100_000,
        "",
        "reasoning = false",
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "first").await;
    h.settle(&s.id).await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
                model: Some("mock/second".parse().unwrap()),
                title: None,
                thinking: None,
                permission: None,
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
    for message in ["second", "third"] {
        h.send(&s.id, message).await;
        h.settle(&s.id).await;
    }
    let requests = h.mock.agent_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["model"], "second");
    for pair in requests.windows(2) {
        assert_eq!(system(&pair[0]).as_bytes(), system(&pair[1]).as_bytes());
        assert_eq!(
            serde_json::to_vec(&pair[0]["tools"]).unwrap(),
            serde_json::to_vec(&pair[1]["tools"]).unwrap()
        );
        let previous = pair[0]["messages"].as_array().unwrap();
        let next = pair[1]["messages"].as_array().unwrap();
        assert_eq!(
            serde_json::to_vec(previous).unwrap(),
            serde_json::to_vec(&next[..previous.len()]).unwrap()
        );
    }
    let notes: Vec<String> = h
        .entries(&s.id)
        .await
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User { content, author } if author.as_deref() == Some("harness") => {
                Some(plain_text(content))
            }
            _ => None,
        })
        .filter(|t| t.starts_with("[Environment update]"))
        .collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("- Model: mock/second"));
    println!(
        "switch: requests=3, environment_notes=1, note_est_tokens={}, prefix=identical",
        tessera_core::util::estimate_tokens(&notes[0])
    );
}

#[tokio::test]
async fn explicit_compaction_reuses_the_cached_chat_prefix_too() {
    let mut script: Vec<Reply> = (0..15).map(|i| step(i, 4_000)).collect();
    script.push(Reply::Text("finished".into()));
    let h = local(script, 100_000, "discover = true", "reasoning = false", "").await;
    let s = h.session().await;
    h.send(&s.id, "build it").await;
    settle_long(&h, &s.id).await;
    assert_eq!(h.mock.summary_requests(), 0, "no automatic compaction");
    h.engine
        .handle(
            Request::Compact {
                session_id: s.id.clone(),
                instructions: Some("Keep the build commands".into()),
            },
            None,
        )
        .await
        .unwrap();
    settle_long(&h, &s.id).await;
    let requests = h.mock.state.requests.lock().clone();
    let summaries: Vec<&Value> = requests.iter().filter(|r| in_place_checkpoint(r)).collect();
    assert_eq!(
        summaries.len(),
        1,
        "one in-place checkpoint, no transcript retries"
    );
    assert_eq!(h.mock.summary_requests(), 1);
    let previous = h.mock.agent_requests().pop().unwrap();
    let summary = summaries[0];
    assert_eq!(summary["tools"], previous["tools"]);
    assert_eq!(summary["tool_choice"], "none");
    assert!(summary.to_string().contains("Keep the build commands"));
    let a = previous["messages"].as_array().unwrap();
    let b = summary["messages"].as_array().unwrap();
    assert_eq!(
        serde_json::to_vec(&a[..b.len() - 1]).unwrap(),
        serde_json::to_vec(&b[..b.len() - 1]).unwrap()
    );
    assert!(b.len() > 2);
    println!(
        "explicit checkpoint: requests=1, cached_prefix_messages={}, prefix=identical",
        b.len() - 1
    );
}

#[tokio::test]
async fn task_pruning_and_size_caps_archive_exact_text_for_recall() {
    let long = "Readable normal prose with spaces and café. ".repeat(13);
    let items: Vec<String> = (0..30)
        .map(|i| format!("Item {i}: {long}").trim_end().to_string())
        .collect();
    let h = local(
        vec![
            Reply::Tools(vec![(
                "task".into(),
                json!({"decisions": items, "artifacts": ["old-a", "old-b"], "notes": ["old note"]}),
            )]),
            Reply::Tools(vec![(
                "task".into(),
                json!({"remove": {"decisions": [1], "artifacts": [1]}, "notes": ["current note"]}),
            )]),
            Reply::Text("kept current state".into()),
        ],
        100_000,
        "",
        "",
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "keep a ledger").await;
    h.settle(&s.id).await;
    let l = h.info(&s.id).await.task.unwrap();
    assert_eq!(l.decisions.len(), 2);
    assert_eq!(l.artifacts, ["old-b"]);
    assert_eq!(l.notes, ["current note"]);
    assert_eq!(l.archived["decisions"].count, 28);
    assert_eq!(l.archived["artifacts"].count, 1);
    assert_eq!(l.archived["notes"].count, 1);
    let (label, bytes) = h
        .engine
        .store()
        .artifact(&l.archived["decisions"].artifact)
        .unwrap()
        .unwrap();
    assert!(label.contains("ledger archive"));
    let latest: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(latest["items"][0].as_str().unwrap().starts_with("Item 27:"));
    let previous = latest["previous_artifact"].as_str().unwrap();
    let (_, bytes) = h.engine.store().artifact(previous).unwrap().unwrap();
    let previous_bundle: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        previous_bundle["items"][0], items[0],
        "not truncated or squeezed"
    );
    assert_eq!(previous_bundle["items"].as_array().unwrap().len(), 27);
    let rendered = tessera_core::tools::meta::render_ledger(&l);
    assert!(rendered.contains("28 older items archived; use recall artifact="));
    assert!(rendered.len() < 2_048);
    println!(
        "ledger: initial_decision_bytes={}, live_ledger_bytes={}, archived_decisions=28",
        items.iter().map(String::len).sum::<usize>(),
        rendered.len()
    );
    h.mock.state.script.lock().extend([
        Reply::Tools(vec![(
            "task".into(),
            json!({"decisions": [], "artifacts": [], "notes": []}),
        )]),
        Reply::Tools(vec![("recall".into(), json!({"artifact": previous}))]),
        Reply::Text("archived and recalled".into()),
    ]);
    h.send(
        &s.id,
        "clear the live lists, then retrieve the old evidence",
    )
    .await;
    h.settle(&s.id).await;
    let l = h.info(&s.id).await.task.unwrap();
    assert!(l.decisions.is_empty() && l.artifacts.is_empty() && l.notes.is_empty());
    assert_eq!(l.archived["decisions"].count, 30);
    let outputs = tool_results(&h.entries(&s.id).await);
    assert!(
        outputs
            .iter()
            .any(|r| r.0 == "recall" && !r.1 && r.2.contains(&items[0]))
    );
    // Artifacts remain session owned and the existing recall policy denies
    // private/bot-separated contexts; there is no global archive channel.
    assert_eq!(
        h.engine.store().artifact_session(previous).unwrap(),
        Some(s.id)
    );
}

#[tokio::test]
async fn repeated_native_checkpoints_do_not_repeat_the_unchanged_ledger_in_the_nudge() {
    let mut script: Vec<Reply> = (0..15).map(|i| step(i, 4_000)).collect();
    script.push(Reply::Text("first done".into()));
    let h = local(script, 100_000, "discover = true", "reasoning = false", "").await;
    let s = h.session().await;
    h.engine
        .handle(
            Request::SetGoal {
                session_id: s.id.clone(),
                goal: "Unique durable ledger fact in normal readable prose. ".repeat(20),
            },
            None,
        )
        .await
        .unwrap();
    h.send(&s.id, "first build").await;
    settle_long(&h, &s.id).await;
    h.engine
        .handle(
            Request::Compact {
                session_id: s.id.clone(),
                instructions: None,
            },
            None,
        )
        .await
        .unwrap();
    settle_long(&h, &s.id).await;
    h.mock.state.script.lock().extend(
        (15..30)
            .map(|i| step(i, 4_000))
            .chain([Reply::Text("second done".into())]),
    );
    h.send(&s.id, "second build").await;
    settle_long(&h, &s.id).await;
    h.engine
        .handle(
            Request::Compact {
                session_id: s.id.clone(),
                instructions: None,
            },
            None,
        )
        .await
        .unwrap();
    settle_long(&h, &s.id).await;
    let requests = h.mock.state.requests.lock().clone();
    let checkpoints: Vec<_> = requests.iter().filter(|r| in_place_checkpoint(r)).collect();
    assert_eq!(checkpoints.len(), 2);
    let first = checkpoints[0]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .as_str()
        .unwrap();
    let second = checkpoints[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(first.contains("Unique durable ledger fact"));
    assert!(second.contains("work state unchanged (version "));
    assert!(!second.contains("Unique durable ledger fact"));
    assert!(
        checkpoints[1]["messages"].as_array().unwrap()[1]["content"]
            .as_str()
            .unwrap()
            .contains("Unique durable ledger fact"),
        "the cached earlier checkpoint still holds the ledger"
    );
    let ledger = h.info(&s.id).await.task.unwrap();
    let full_nudge = tessera_core::context::checkpoint_request(true, Some(&ledger), None, 4_000);
    assert!(
        second.len() < full_nudge.len(),
        "unchanged nudge should be smaller than resending the same ledger"
    );
    println!(
        "native ledger checkpoints: nudge_before_bytes={}, unchanged_nudge_bytes={}, full_ledger_copies=1",
        full_nudge.len(),
        second.len()
    );
}
