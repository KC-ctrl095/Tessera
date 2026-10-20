//! Live checks of the completion check, model fallback, restart recovery and
//! web_fetch extraction against a local model (ignored by default):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_harness -- --ignored --nocapture --test-threads 1

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

fn provider_block(model: &str, url: &str) -> String {
    let (provider, id) = model.split_once('/').unwrap();
    format!(
        "[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"
    )
}

async fn snapshot(e: &Engine, id: &str) -> SessionSnapshot {
    serde_json::from_value(
        e.handle(
            Request::GetSession {
                session_id: id.into(),
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

fn dump(entries: &[Entry]) {
    for e in entries {
        match &e.body {
            EntryBody::User { content, author } => eprintln!(
                "[{}] {}",
                author.as_deref().unwrap_or("user"),
                tessera_core::util::truncate(&plain_text(content), 500)
            ),
            EntryBody::Assistant { content, model, .. } => {
                for p in content {
                    match p {
                        AssistantPart::Text { text } => {
                            eprintln!(
                                "[assistant {model}] {}",
                                tessera_core::util::truncate(text, 400)
                            )
                        }
                        AssistantPart::ToolCall {
                            name, arguments, ..
                        } => {
                            eprintln!("[assistant {model}] → {name} {arguments}")
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
            } => eprintln!(
                "[{tool}{}] {}",
                if *is_error { " error" } else { "" },
                tessera_core::util::truncate(&plain_text(content), 300)
            ),
            EntryBody::Notice { text, .. } => eprintln!("[notice] {text}"),
            EntryBody::Compaction { .. } => eprintln!("[compaction]"),
        }
    }
}

async fn send(e: &Engine, id: &str, text: &str) {
    e.handle(
        Request::Send {
            session_id: id.into(),
            content: vec![ContentPart::text(text)],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(900), e.wait_idle(id))
        .await
        .unwrap();
}

/// The configured model is down (closed port): the local model takes over
/// with a notice, and the completion check makes it verify its criterion.
#[tokio::test]
#[ignore]
async fn fallback_then_completion_check_with_evidence() {
    let Some((model, url)) = live() else { return };
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"down/m\"\n[agent]\nreview_interval = 0\n[models]\nspare = \"{model}\"\n[models.fallback]\ndefault = [\"@spare\"]\n[[provider]]\nid = \"down\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\n{}",
            provider_block(&model, &url)
        ),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(work.path().display().to_string()),
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
    let started = std::time::Instant::now();
    send(
        &e,
        &s.id,
        "Create hello.txt containing exactly `hi`. First record the task with the `task` tool, with done_criteria [\"hello.txt contains hi\"]. Keep it short.",
    )
    .await;
    let snap = snapshot(&e, &s.id).await;
    dump(&snap.entries);
    eprintln!("took {:?}", started.elapsed());
    let notices: Vec<&str> = snap
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("continuing with") && n.contains(&model)),
        "{notices:?}"
    );
    assert!(snap.entries.iter().all(
        |e| !matches!(&e.body, EntryBody::Assistant { model: m, .. } if m.to_string() == "down/m")
    ));
    assert_eq!(
        std::fs::read_to_string(work.path().join("hello.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    let checks = snap
        .entries
        .iter()
        .filter(|e| matches!(&e.body, EntryBody::User { author: Some(a), .. } if a == "harness"))
        .count();
    let task = snap.info.task.clone().unwrap_or_default();
    eprintln!("completion checks: {checks}; criteria: {:?}", task.criteria);
    assert!(checks <= 1);
    assert_eq!(
        snap.info.model.to_string(),
        "down/m",
        "the session keeps its own model"
    );
}

/// A job interrupted mid-command resumes after a restart and is told the
/// command's outcome is uncertain.
#[tokio::test]
#[ignore]
async fn interrupted_job_resumes_with_a_recovery_trace() {
    let Some((model, url)) = live() else { return };
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\n{}",
            provider_block(&model, &url)
        ),
    )
    .unwrap();
    let (job_session, job_id);
    {
        let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
            .await
            .unwrap();
        let job: Job = serde_json::from_value(
            e.handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Marker".into(),
                        schedule: "0 3 * * *".into(),
                        prompt: "Run exactly this with bash (foreground, no changes): `sleep 900 && echo done > marker.txt`. Then report whether marker.txt exists.".into(),
                        cwd: work.path().display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: None,
                        last_run: None,
                        next_run: None,
                        last_session: None,
                        last_status: None,
                        ..Default::default()
                    },
                },
                None,
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let info: SessionInfo = serde_json::from_value(
            e.handle(Request::RunJob { id: job.id.clone() }, None)
                .await
                .unwrap(),
        )
        .unwrap();
        // Wait until the command is running, then "crash".
        let mut running = false;
        for _ in 0..600 {
            let snap = snapshot(&e, &info.id).await;
            if snap
                .streaming
                .as_ref()
                .is_some_and(|s| !s.running_tools.is_empty())
            {
                running = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(
            running,
            "the job never started its command: {:#?}",
            snapshot(&e, &info.id).await.entries
        );
        job_session = info.id;
        job_id = job.id;
    }
    let store = tessera_core::store::Store::open(&home.path().join("tessera.db")).unwrap();
    let (mut info, internal) = store.get_session(&job_session).unwrap().unwrap();
    info.status = SessionStatus::Running;
    store.put_session(&info, &internal).unwrap();
    drop(store);
    let e = Engine::open(Paths::at(home.path()), EngineOptions { scheduler: true })
        .await
        .unwrap();
    let started = std::time::Instant::now();
    // Watch the resumed turn until the agent's first action after the
    // recovery note, then stop it (re-running a 15-minute sleep proves nothing).
    let mut first_action = None;
    for _ in 0..1200 {
        let snap = snapshot(&e, &job_session).await;
        let after_note = snap
            .entries
            .iter()
            .skip_while(
                |e| !matches!(&e.body, EntryBody::User { author: Some(a), .. } if a == "harness"),
            )
            .find_map(|e| match &e.body {
                EntryBody::Assistant { content, .. } => Some(content.clone()),
                _ => None,
            });
        if let Some(parts) = after_note {
            first_action = Some(parts);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    e.handle(
        Request::Abort {
            session_id: job_session.clone(),
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(60), e.wait_idle(&job_session))
        .await
        .unwrap();
    let snap = snapshot(&e, &job_session).await;
    dump(&snap.entries);
    eprintln!("resumed in {:?}", started.elapsed());
    let trace = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a == "harness" => Some(plain_text(content)),
            _ => None,
        })
        .expect("recovery trace");
    assert!(
        trace.contains("UNCERTAIN") && trace.contains("sleep 900"),
        "{trace}"
    );
    // Its first move is not a blind repeat of the uncertain command.
    let first = first_action.expect("the job resumed");
    let repeated = first.iter().any(|p| matches!(p, AssistantPart::ToolCall { arguments, .. } if arguments.to_string().contains("sleep 900")));
    assert!(!repeated, "blindly re-ran the uncertain command: {first:?}");
    let jobs: Vec<Job> =
        serde_json::from_value(e.handle(Request::ListJobs, None).await.unwrap()).unwrap();
    eprintln!(
        "job status: {:?}",
        jobs.iter().find(|j| j.id == job_id).unwrap().last_status
    );
}

/// `web_fetch` with a prompt: the local model answers from a long page and
/// only the extract enters the agent's context.
#[tokio::test]
#[ignore]
async fn web_fetch_prompt_extracts_from_a_long_page() {
    let Some((model, url)) = live() else { return };
    let filler = "The committee discussed budgets, schedules and catering at length. ".repeat(250);
    let page = format!(
        "<html><head><title>Release notes</title></head><body><main><p>{filler}</p><h2>Codename</h2><p>The 4.2 release is codenamed Marigold Falcon and ships on 12 March.</p><p>{filler}</p></main></body></html>"
    );
    let app = axum::Router::new().route(
        "/notes",
        axum::routing::get(move || {
            let p = page.clone();
            async move { axum::response::Html(p) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\nutility_model = \"{model}\"\n[agent]\nreview_interval = 0\npermission = \"auto\"\n{}", provider_block(&model, &url)),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: None,
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
    let started = std::time::Instant::now();
    send(
        &e,
        &s.id,
        &format!("Call web_fetch once on http://{addr}/notes with prompt \"What is the 4.2 release codename?\" and tell me the answer in one line."),
    )
    .await;
    let snap = snapshot(&e, &s.id).await;
    dump(&snap.entries);
    eprintln!("took {:?}", started.elapsed());
    let fetched = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult { tool, content, .. } if tool == "web_fetch" => {
                Some(plain_text(content))
            }
            _ => None,
        })
        .expect("web_fetch was called");
    assert!(fetched.starts_with("Extract from"), "{fetched}");
    assert!(fetched.contains("Marigold Falcon"), "{fetched}");
    assert!(fetched.len() < 2_000, "{} chars", fetched.len());
}
