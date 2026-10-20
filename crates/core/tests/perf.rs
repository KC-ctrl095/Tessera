//! Overhead numbers for the harness itself (ignored by default; the model is
//! a local mock, so this measures Tessera, not a model):
//!   cargo test --release -p tessera-core --test perf -- --ignored --nocapture

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::*;
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// Bytes this process has written (`wchar` in /proc/self/io).
fn written() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("wchar: ")
                    .and_then(|v| v.trim().parse().ok())
            })
        })
        .unwrap_or(0)
}

/// Turns through the engine: time per turn, events, and storage written.
/// `per_row_index` puts back the old full-text trigger (one index segment
/// per entry) for comparison.
async fn turns(per_row_index: bool) {
    let h = harness(vec![], 400_000).await;
    if per_row_index {
        let c = rusqlite::Connection::open(h.dir.path().join("tessera.db")).unwrap();
        c.execute_batch(
            "CREATE TRIGGER t AFTER INSERT ON entries BEGIN INSERT INTO entries_fts(rowid, text) VALUES (new.id, new.text); END;
             UPDATE meta SET value = '999999999' WHERE key = 'fts_upto';",
        )
        .unwrap();
    }
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    let turns = 200;
    let reply = "A reasonably sized answer with some detail. ".repeat(20);
    for _ in 0..turns {
        h.mock.push(Reply::Text(reply.clone()));
    }
    let before = written();
    let mut times = Vec::new();
    let mut by_type: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for i in 0..turns {
        let t = Instant::now();
        h.send(&s.id, &format!("message {i}: {}", "context ".repeat(40)))
            .await;
        tokio::time::timeout(Duration::from_secs(20), h.engine.wait_idle(&s.id))
            .await
            .unwrap();
        times.push(t.elapsed());
        while let Ok(e) = events.try_recv() {
            let json = serde_json::to_value(&e).unwrap();
            let ty = json["type"].as_str().unwrap_or("?").to_string();
            let slot = by_type.entry(ty).or_default();
            slot.0 += 1;
            slot.1 += json.to_string().len() as u64;
        }
    }
    // Minus what went over HTTP to the mock (requests and its replies).
    let http: u64 = h
        .mock
        .state
        .requests
        .lock()
        .iter()
        .map(|r| r.to_string().len() as u64 + 2_000)
        .sum();
    let grown = written().saturating_sub(before).saturating_sub(http);
    let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
    let avg = |t: &[Duration]| t.iter().map(ms).sum::<f64>() / t.len() as f64;
    eprintln!(
        "\n[{}] per turn: first 20 avg {:.2} ms, last 20 avg {:.2} ms (mock model incl. its HTTP); storage writes {:.1} KB",
        if per_row_index {
            "per-row index (before)"
        } else {
            "batched index"
        },
        avg(&times[..20]),
        avg(&times[turns - 20..]),
        grown as f64 / 1024.0 / turns as f64
    );
    if !per_row_index {
        let (n, bytes) = by_type
            .values()
            .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
        eprintln!(
            "events per turn: {:.1} ({:.1} KB); by type (count, KB):",
            n as f64 / turns as f64,
            bytes as f64 / 1024.0 / turns as f64
        );
        for (t, (c, b)) in &by_type {
            eprintln!("  {t:<18} {:>6} {:>8.1}", c, *b as f64 / 1024.0);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn harness_overhead() {
    turns(true).await;
    turns(false).await;
    // Startup on a data directory with history.
    let h = harness(vec![], 200_000).await;
    for i in 0..100 {
        let s = h.session().await;
        if i % 10 == 0 {
            h.mock.push(Reply::Text("ok".into()));
            h.send(&s.id, "hello").await;
            h.engine.wait_idle(&s.id).await;
        }
    }
    let home = h.dir.path().to_path_buf();
    drop(h.engine);
    let t = Instant::now();
    let e = Engine::open(Paths::at(&home), EngineOptions { scheduler: true })
        .await
        .unwrap();
    eprintln!(
        "startup with 100 chats: {:.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    drop(e);
}

#[test]
#[ignore]
fn storage_writes_by_operation() {
    use tessera_core::store::{SessionInternal, Store};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db")).unwrap();
    let mut info: SessionInfo = serde_json::from_value(serde_json::json!({
        "id": "s", "title": "t", "kind": "chat", "cwd": "/", "model": {"provider": "p", "model": "m"},
        "thinking": "auto", "permission": "default", "status": "idle", "created_at": 1, "updated_at": 1,
        "last_seq": 0, "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
        "context": {"tokens": 0, "window": 1, "compactions": 0}, "pinned": false, "archived": false,
        "preview": ""
    }))
    .unwrap();
    let internal = SessionInternal {
        system_prompt: Some("x".repeat(6_000)),
        ..Default::default()
    };
    store.put_session(&info, &internal).unwrap();
    let n = 200;
    let measure = |label: &str, f: &mut dyn FnMut(usize)| {
        let before = written();
        for i in 0..n {
            f(i);
        }
        eprintln!(
            "{label:<28} {:>6.1} KB each",
            written().saturating_sub(before) as f64 / 1024.0 / n as f64
        );
    };
    let text = "word ".repeat(260);
    measure("append (1.3 KB entry)", &mut |i| {
        store
            .append(
                "s",
                EntryBody::User {
                    content: vec![ContentPart::text(format!("{i} {text}"))],
                    author: None,
                },
            )
            .unwrap();
    });
    measure("put_session (6 KB state)", &mut |i| {
        info.updated_at = i as u64;
        store.put_session(&info, &internal).unwrap();
    });
    measure("put_session_info", &mut |i| {
        info.updated_at = i as u64;
        store.put_session_info(&info).unwrap();
    });
    let m = ModelRef {
        provider: "p".into(),
        model: "m".into(),
    };
    let u = Usage {
        input: 10,
        ..Default::default()
    };
    measure("record_usage", &mut |_| {
        store
            .record_usage(&tessera_core::store::UsageRecord {
                session_id: Some("s"),
                model: &m,
                kind: "chat",
                usage: &u,
                api_cost: None,
            })
            .unwrap();
    });
    measure("tool_started+finished", &mut |i| {
        let id = format!("c{i}");
        store.tool_started("s", &id, "bash").unwrap();
        store.tool_finished("s", &id).unwrap();
    });
}

/// What reading a chat's history costs the agent loop (each step reads it):
/// a long tool loop of `steps` (a call with some reasoning, then its output).
#[test]
#[ignore]
fn history_reads() {
    use tessera_core::store::{SessionInternal, Store};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db")).unwrap();
    let output = "line of command output with a path /src/module.rs:42 and some words\n".repeat(60);
    for steps in [100usize, 500, 2000] {
        let id = format!("s{steps}");
        let info: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": id, "title": "t", "kind": "chat", "cwd": "/", "model": {"provider": "p", "model": "m"},
            "thinking": "auto", "permission": "default", "status": "idle", "created_at": 1, "updated_at": 1,
            "last_seq": 0, "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
            "context": {"tokens": 0, "window": 1, "compactions": 0}, "pinned": false, "archived": false,
            "preview": ""
        }))
        .unwrap();
        store
            .put_session(&info, &SessionInternal::default())
            .unwrap();
        for i in 0..steps {
            store
                .append(
                    &id,
                    EntryBody::Assistant {
                        content: vec![
                            AssistantPart::Thinking {
                                text: "I should look at the next file and check it. ".repeat(20),
                                opaque: None,
                            },
                            AssistantPart::ToolCall {
                                id: format!("c{i}"),
                                name: "bash".into(),
                                arguments: serde_json::json!({"command": format!("grep -rn thing src/{i}")}),
                            },
                        ],
                        model: ModelRef {
                            provider: "p".into(),
                            model: "m".into(),
                        },
                        usage: Usage::default(),
                        stop: StopReason::ToolUse,
                        error: None,
                        author: None,
                    },
                )
                .unwrap();
            store
                .append(
                    &id,
                    EntryBody::ToolResult {
                        call_id: format!("c{i}"),
                        tool: "bash".into(),
                        content: vec![ContentPart::text(output.clone())],
                        is_error: false,
                        artifact: None,
                        details: None,
                    },
                )
                .unwrap();
        }
        let reads = 30;
        let t = Instant::now();
        let mut n = 0;
        for _ in 0..reads {
            n += store.entries(&id, 0).unwrap().len();
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / reads as f64;
        // One step: the read, then the step's own appends.
        let t = Instant::now();
        for i in 0..reads {
            n += store.entries(&id, 0).unwrap().len();
            store
                .append(
                    &id,
                    EntryBody::Notice {
                        level: NoticeLevel::Info,
                        text: format!("step {i}"),
                        learned: Vec::new(),
                    },
                )
                .unwrap();
        }
        let per_step = t.elapsed().as_secs_f64() * 1000.0 / reads as f64;
        eprintln!(
            "history of {steps} steps ({} entries): full read {per:.2} ms; read + append {per_step:.2} ms ({n} read)",
            steps * 2
        );
    }
}
