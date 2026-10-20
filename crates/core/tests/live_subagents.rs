//! Live check of background subagents (ignored by default):
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_subagents -- --ignored --nocapture

use std::time::{Duration, Instant};

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

fn texts(entries: &[Entry]) -> Vec<(u64, String, String)> {
    entries
        .iter()
        .filter_map(|en| match &en.body {
            EntryBody::User { content, author } => Some((
                en.seq,
                format!(
                    "user{}",
                    author
                        .as_deref()
                        .map(|a| format!("[{}]", &a[..a.len().min(20)]))
                        .unwrap_or_default()
                ),
                plain_text(content),
            )),
            EntryBody::Assistant { content, .. } => {
                let t: String = content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.clone()),
                        AssistantPart::ToolCall {
                            name, arguments, ..
                        } => Some(format!("\n→ {name} {arguments}")),
                        _ => None,
                    })
                    .collect();
                let who = if t.starts_with("\n→") {
                    "calls"
                } else {
                    "assistant"
                };
                (!t.is_empty()).then(|| (en.seq, who.into(), t))
            }
            EntryBody::ToolResult { tool, content, .. } => {
                Some((en.seq, format!("tool:{tool}"), plain_text(content)))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[ignore]
async fn background_subagents_keep_the_chat_responsive() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("notes.txt"),
        "alpha: 3 apples\nbeta: 7 pears\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 0\nsubagent_steps = 6\nsubagent_minutes = 4\npermission = \"auto\"\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
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
    let t0 = Instant::now();
    e.handle(Request::Send { session_id: s.id.clone(), content: vec![ContentPart::text("Kick off two background subagents (don't wait for them, I want to keep chatting): one counts the words in notes.txt, the other reports how many fruits in total notes.txt lists. When their reports come in, give me both answers.")], delivery: Delivery::Auto, attachments: Vec::new() }, None).await.unwrap();
    // Wait for the parent's first turn to end (it should not block on the subagents).
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::time::timeout(Duration::from_secs(300), e.wait_idle(&s.id))
        .await
        .unwrap();
    let first_idle = t0.elapsed();
    // Ask something unrelated while they work: answered right away.
    let t1 = Instant::now();
    e.handle(
        Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text(
                "While they work: what's the capital of Norway? One word.",
            )],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::time::timeout(Duration::from_secs(300), e.wait_idle(&s.id))
        .await
        .unwrap();
    let side = t1.elapsed();
    // Then everything lands and the parent wraps up by itself.
    let mut done = false;
    for _ in 0..240 {
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
        let t = texts(&snap.entries);
        let reports = t
            .iter()
            .filter(|(_, who, _)| who.starts_with("user[subagent"))
            .count();
        let last_is_answer = t.last().is_some_and(|(_, who, _)| who == "assistant")
            && snap.info.status == SessionStatus::Idle;
        if reports >= 2 && last_is_answer {
            done = true;
            for (seq, who, text) in &t {
                eprintln!(
                    "#{seq} {who}: {}",
                    text.replace('\n', " ")
                        .chars()
                        .take(220)
                        .collect::<String>()
                );
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !done {
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
        eprintln!("status {:?}", snap.info.status);
        for (seq, who, text) in texts(&snap.entries) {
            eprintln!(
                "#{seq} {who}: {}",
                text.replace('\n', " ")
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        let list: Vec<SessionInfo> = serde_json::from_value(
            e.handle(Request::ListSessions { archived: false }, None)
                .await
                .unwrap(),
        )
        .unwrap();
        let _ = list;
    }
    eprintln!(
        "\nfirst turn ended after {:.1}s · side question answered in {:.1}s · total {:.1}s",
        first_idle.as_secs_f64(),
        side.as_secs_f64(),
        t0.elapsed().as_secs_f64()
    );
    assert!(done, "reports arrived and the parent answered");
}

/// A swarm of 8 on a local model: more than can run at once, so some queue.
///   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_subagents swarm -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn a_swarm_on_a_local_model_reports_and_wraps_up() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut total = 0;
    for i in 1..=8 {
        let n = i * 3 + 1;
        total += n;
        std::fs::write(
            work.path().join(format!("box{i}.txt")),
            format!("This box holds {n} marbles.\n"),
        )
        .unwrap();
    }
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 0\nsubagent_steps = 4\nsubagent_minutes = 4\nmax_subagents = 4\npermission = \"auto\"\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
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
    let t0 = Instant::now();
    e.handle(Request::Send { session_id: s.id.clone(), content: vec![ContentPart::text("There are 8 files, box1.txt to box8.txt. Start 8 background subagents in one `agent` call, one per file (readonly), each reporting how many marbles its box holds. When all reports are in, tell me the total number of marbles.")], delivery: Delivery::Auto, attachments: Vec::new() }, None).await.unwrap();
    let mut done = None;
    let mut last_dump = Instant::now();
    for _ in 0..1200 {
        tokio::time::sleep(Duration::from_millis(500)).await;
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
        let t = texts(&snap.entries);
        let reports = t
            .iter()
            .filter(|(_, who, _)| who.starts_with("user[subagent"))
            .count();
        if reports >= 8
            && snap.info.status == SessionStatus::Idle
            && t.last().is_some_and(|(_, who, _)| who == "assistant")
        {
            done = Some(t);
            break;
        }
        // It ran them in the foreground instead (`wait: true`): say how, and stop.
        if snap.info.status == SessionStatus::Idle
            && t.iter()
                .any(|(_, who, text)| who == "tool:agent" && text.starts_with("## "))
        {
            for (seq, who, text) in &t {
                eprintln!(
                    "#{seq} {who}: {}",
                    text.chars().take(1_500).collect::<String>()
                );
            }
            break;
        }
        if last_dump.elapsed() > Duration::from_secs(60) {
            last_dump = Instant::now();
            let list: Vec<SessionInfo> = serde_json::from_value(
                e.handle(Request::ListSessions { archived: false }, None)
                    .await
                    .unwrap(),
            )
            .unwrap();
            eprintln!(
                "--- {:.0}s: parent {:?}, {reports} reports",
                t0.elapsed().as_secs_f64(),
                snap.info.status
            );
            for c in list
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(s.id.as_str()))
            {
                eprintln!("  {} {:?} steps={}", c.title, c.status, c.steps);
            }
            for (seq, who, text) in t.iter().rev().take(6).rev() {
                eprintln!(
                    "  #{seq} {who}: {}",
                    text.replace('\n', " ")
                        .chars()
                        .take(200)
                        .collect::<String>()
                );
            }
        }
    }
    let t = done.expect("all 8 reports arrived and the parent wrapped up");
    for (seq, who, text) in &t {
        eprintln!(
            "#{seq} {who}: {}",
            text.replace('\n', " ")
                .chars()
                .take(160)
                .collect::<String>()
        );
    }
    let answer = &t.last().unwrap().2;
    eprintln!(
        "\n{} reports, total {total}, took {:.0}s",
        t.iter()
            .filter(|(_, w, _)| w.starts_with("user[subagent"))
            .count(),
        t0.elapsed().as_secs_f64()
    );
    assert!(
        answer.contains(&total.to_string()),
        "expected {total} in: {answer}"
    );
}

/// A worktree-isolated subagent edits a file in its private checkout; the
/// parent applies the change once the report arrives.
///   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_subagents worktree -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn a_worktree_subagent_edits_in_isolation_and_the_parent_applies() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(
        repo.join("greet.py"),
        "def greet(name):\n    return \"Hello \" + name\n\n\nprint(greet(\"world\"))\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(
        home.path().join("config.toml"),
        format!("default_model = \"{model}\"\n[agent]\nreview_interval = 0\nsubagent_steps = 8\nsubagent_minutes = 6\npermission = \"auto\"\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"),
    )
    .unwrap();
    let e = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        e.handle(
            Request::CreateSession {
                cwd: Some(repo.display().to_string()),
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
    let t0 = Instant::now();
    e.handle(Request::Send { session_id: s.id.clone(), content: vec![ContentPart::text("Use the `agent` tool to start ONE subagent named \"greeter\" with `isolation: \"worktree\"` and this task: \"In greet.py, change the greeting string from \\\"Hello \\\" to \\\"Hi \\\". Change nothing else.\" Don't edit greet.py yourself. When its report arrives, apply its changes with the `agents` tool (action `apply`, name `greeter`), then read greet.py and tell me its return line.")], delivery: Delivery::Auto, attachments: Vec::new() }, None).await.unwrap();
    let mut done = None;
    for _ in 0..1200 {
        tokio::time::sleep(Duration::from_millis(500)).await;
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
        let t = texts(&snap.entries);
        let applied = t
            .iter()
            .any(|(_, who, text)| who == "tool:agents" && text.contains("Applied"));
        let idle = snap.info.status == SessionStatus::Idle
            && t.last().is_some_and(|(_, who, _)| who == "assistant");
        if applied && idle {
            done = Some(t);
            break;
        }
        // It answered without applying anything: no point waiting longer.
        if idle && t.iter().any(|(_, who, _)| who == "tool:agents") {
            break;
        }
    }
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
    for (seq, who, text) in texts(&snap.entries) {
        let max = if who == "calls" { 2_000 } else { 300 };
        eprintln!(
            "#{seq} {who}: {}",
            text.replace('\n', " ")
                .chars()
                .take(max)
                .collect::<String>()
        );
    }
    let greet = std::fs::read_to_string(repo.join("greet.py")).unwrap();
    eprintln!(
        "\ngreet.py now:\n{greet}\ntook {:.0}s",
        t0.elapsed().as_secs_f64()
    );
    let worktrees = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list"])
        .output()
        .unwrap();
    let worktrees = String::from_utf8_lossy(&worktrees.stdout).to_string();
    assert!(done.is_some(), "the parent applied the change and answered");
    assert!(greet.contains("\"Hi \""), "{greet}");
    assert!(!greet.contains("\"Hello \""), "{greet}");
    assert_eq!(worktrees.lines().count(), 1, "{worktrees}");
}
