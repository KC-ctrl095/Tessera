//! A model server that fails for a while, end to end with paused time:
//! a chat on the screen gives up after its short schedule and says "retrying
//! in Ns"; work nobody watches keeps trying for half an hour and carries on when
//! the server is back; Stop cancels a wait at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;
use tokio::time::Instant;

/// An OpenAI-compatible endpoint that answers `status` for `down_for` after
/// its first request, then says "back again".
struct Flaky {
    status: u16,
    /// What the error says.
    message: &'static str,
    retry_after: Option<&'static str>,
    down_for: Duration,
    /// Replies (stream chunks) served before the outage starts, in order: a
    /// parent chat calling a tool before its helper meets the outage.
    script: Mutex<std::collections::VecDeque<Vec<Value>>>,
    first: Mutex<Option<Instant>>,
    hits: AtomicUsize,
    /// When each request came, from the first.
    times: Mutex<Vec<Duration>>,
    hold_first: std::sync::atomic::AtomicBool,
    first_started: tokio::sync::Notify,
    release_first: tokio::sync::Notify,
    plan_reset: std::sync::atomic::AtomicBool,
}

async fn chat(
    State(f): State<Arc<Flaky>>,
    axum::Json(req): axum::Json<Value>,
) -> impl IntoResponse {
    // Title requests are not part of the story.
    let system = req["messages"][0]["content"].as_str().unwrap_or("");
    if system.starts_with("You name conversations") {
        return sse("Title").into_response();
    }
    if let Some(chunks) = f.script.lock().pop_front() {
        return chunks_reply(chunks).into_response();
    }
    let now = Instant::now();
    let first = *f.first.lock().get_or_insert(now);
    let hit = f.hits.fetch_add(1, Ordering::SeqCst);
    f.times.lock().push(now - first);
    if hit == 0 && f.hold_first.load(Ordering::SeqCst) {
        f.first_started.notify_one();
        f.release_first.notified().await;
    }
    if now - first < f.down_for {
        let mut headers = axum::http::HeaderMap::new();
        if let Some(ra) = f.retry_after {
            headers.insert("retry-after", ra.parse().unwrap());
        }
        return (
            axum::http::StatusCode::from_u16(f.status).unwrap(),
            headers,
            if f.plan_reset.load(Ordering::SeqCst) {
                json!({"error": {"type": "usage_limit_reached", "message": f.message, "resets_in_seconds": 3600}}).to_string()
            } else {
                json!({"error": {"message": f.message}}).to_string()
            },
        )
            .into_response();
    }
    sse("back again").into_response()
}

fn sse(text: &str) -> impl IntoResponse {
    chunks_reply(vec![
        json!({"choices": [{"index": 0, "delta": {"content": text}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 5}}),
    ])
}

/// A reply that calls one tool.
fn tool_call(name: &str, args: Value) -> Vec<Value> {
    vec![
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": name, "arguments": args.to_string()}}]}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 100, "completion_tokens": 5}}),
    ]
}

fn chunks_reply(chunks: Vec<Value>) -> impl IntoResponse {
    let mut body = String::new();
    for c in chunks {
        body.push_str(&format!("data: {c}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], body)
}

struct Rig {
    engine: Engine,
    flaky: Arc<Flaky>,
    _dir: tempfile::TempDir,
    work: std::path::PathBuf,
    _work: tempfile::TempDir,
}

async fn rig(
    status: u16,
    retry_after: Option<&'static str>,
    down_for: Duration,
    agent: &str,
) -> Rig {
    rig_with(status, "Overloaded", retry_after, down_for, agent, vec![]).await
}

async fn rig_with(
    status: u16,
    message: &'static str,
    retry_after: Option<&'static str>,
    down_for: Duration,
    agent: &str,
    script: Vec<Vec<Value>>,
) -> Rig {
    let flaky = Arc::new(Flaky {
        status,
        message,
        retry_after,
        down_for,
        script: Mutex::new(script.into()),
        first: Mutex::new(None),
        hits: AtomicUsize::new(0),
        times: Mutex::new(Vec::new()),
        hold_first: std::sync::atomic::AtomicBool::new(false),
        first_started: tokio::sync::Notify::new(),
        release_first: tokio::sync::Notify::new(),
        plan_reset: std::sync::atomic::AtomicBool::new(false),
    });
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async { axum::Json(json!({"data": [{"id": "m"}]})) }),
        )
        .route("/v1/chat/completions", post(chat))
        .with_state(flaky.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    // A hosted-looking provider (the mock's address is local, so say it is
    // not: `parallel` only matters for single-slot servers).
    let config = format!(
        "default_model = \"mock/m\"\n[agent]\nreview_interval = 0\n{agent}\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\nparallel = 4\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\nmax_output = 2000\n"
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Rig {
        engine,
        flaky,
        _dir: dir,
        work,
        _work: work_dir,
    }
}

impl Rig {
    async fn start(&self, text: &str) -> String {
        let v = self
            .engine
            .handle(
                Request::CreateSession {
                    cwd: Some(self.work.display().to_string()),
                    model: None,
                    // Titled up front: no title request in the story.
                    title: Some("T".into()),
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap();
        let s: SessionInfo = serde_json::from_value(v).unwrap();
        self.engine
            .handle(
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
        s.id
    }

    /// Another message in a chat that is working.
    async fn say(&self, id: &str, text: &str) {
        self.engine
            .handle(
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
    }

    /// A room of two bots, with a first message from the user.
    async fn room(&self, text: &str) -> String {
        let mut members = Vec::new();
        for name in ["Tutor", "Quizzer"] {
            let bot: Bot = serde_json::from_value(json!({
                "id": "", "name": name, "handle": "", "avatar": "", "color": "", "role": "helps",
                "personality": "", "instructions": "", "permission": "default", "tools": "assistant"
            }))
            .unwrap();
            let v = self
                .engine
                .handle(Request::SaveBot { bot }, None)
                .await
                .unwrap();
            members.push(serde_json::from_value::<Bot>(v).unwrap().id);
        }
        let v = self
            .engine
            .handle(
                Request::CreateChannel {
                    title: "Study group".into(),
                    members,
                    cwd: Some(self.work.display().to_string()),
                },
                None,
            )
            .await
            .unwrap();
        let room: SessionInfo = serde_json::from_value(v).unwrap();
        self.say(&room.id, text).await;
        room.id
    }
    async fn sessions(&self) -> Vec<SessionInfo> {
        serde_json::from_value(
            self.engine
                .handle(Request::ListSessions { archived: false }, None)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// Notifications sent to the user's devices (titles).
    async fn notified(&self) -> Vec<String> {
        let v = self
            .engine
            .handle(Request::ListNotifications, None)
            .await
            .unwrap();
        serde_json::from_value::<Vec<UserNotification>>(v)
            .unwrap()
            .into_iter()
            .map(|n| n.title)
            .collect()
    }

    async fn wait_hits(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.flaky.hits.load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the queued message did not wake the request within 30 seconds");
    }
    /// Wait (in paused time) until the turn is over.
    async fn finish(&self, id: &str) {
        tokio::time::timeout(Duration::from_secs(4 * 3600), self.engine.wait_idle(id))
            .await
            .expect("the turn never ended");
    }

    async fn entries(&self, id: &str) -> Vec<Entry> {
        let v = self
            .engine
            .handle(
                Request::GetSession {
                    session_id: id.into(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap();
        serde_json::from_value::<SessionSnapshot>(v)
            .unwrap()
            .entries
    }
}

fn notices(entries: &[Entry]) -> Vec<(NoticeLevel, String)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { level, text, .. } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

fn tool_results(entries: &[Entry]) -> Vec<(String, bool, String)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                is_error,
                content,
                ..
            } => Some((tool.clone(), *is_error, plain_text(content))),
            _ => None,
        })
        .collect()
}

fn said(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_chat_on_the_screen_gives_up_after_its_short_schedule() {
    // The mock is a server on this computer, which gets ten tries (a model
    // loading, a PC waking up); a hosted API gets five (`llm/retry.rs`).
    let r = rig(529, None, Duration::from_secs(3600), "").await;
    let t0 = Instant::now();
    let id = r.start("hello").await;
    r.finish(&id).await;
    let took = t0.elapsed();
    // One try and ten more after 2, 4, 8, 16, 32 and five times 60 seconds.
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 11);
    assert!(
        took >= Duration::from_secs(362) && took < Duration::from_secs(375),
        "{took:?}"
    );
    let entries = r.entries(&id).await;
    let n = notices(&entries);
    let retries: Vec<&String> = n
        .iter()
        .map(|n| &n.1)
        .filter(|t| t.contains("retrying in"))
        .collect();
    assert_eq!(retries.len(), 10, "{n:?}");
    assert!(
        retries[0].ends_with("retrying in 2s (attempt 1/10)"),
        "{}",
        retries[0]
    );
    assert!(
        retries[9].ends_with("retrying in 60s (attempt 10/10)"),
        "{}",
        retries[9]
    );
    // Then the error, in plain words, without any "kept trying" claim.
    let last = n.last().unwrap();
    assert_eq!(last.0, NoticeLevel::Error, "{n:?}");
    assert!(
        last.1.contains("Overloaded") && !last.1.contains("kept trying"),
        "{}",
        last.1
    );
    assert_eq!(r.engine_status(&id).await, SessionStatus::Error);
}

#[tokio::test(start_paused = true)]
async fn work_nobody_watches_survives_ten_minutes_of_overload() {
    // YOLO mode counts as unattended.
    let r = rig(
        529,
        None,
        Duration::from_secs(10 * 60),
        "permission = \"auto\"",
    )
    .await;
    let id = r.start("do the night shift").await;
    r.finish(&id).await;
    let entries = r.entries(&id).await;
    // It carried on and answered once the server was back.
    assert_eq!(said(&entries), ["back again"], "{:?}", notices(&entries));
    let times = r.flaky.times.lock().clone();
    // Tries well past the five of a chat on the screen, none before the
    // incident ended succeeded, and the last came right after minute ten.
    assert!(times.len() >= 9, "{times:?}");
    let last = *times.last().unwrap();
    assert!(
        last >= Duration::from_secs(600) && last < Duration::from_secs(600 + 130),
        "{last:?}"
    );
    // Pauses are jittered (not all the same) and none is over two minutes.
    let gaps: Vec<u64> = times.windows(2).map(|w| (w[1] - w[0]).as_secs()).collect();
    assert!(gaps.iter().all(|g| *g <= 121), "{gaps:?}");
    assert!(
        gaps.iter().skip(2).any(|g| *g != gaps[2]) || gaps.len() < 4,
        "{gaps:?}"
    );
    let n = notices(&entries);
    // One notice when it began (not one per try), a second after ten
    // minutes at most, and one when the server answered again.
    let began: Vec<&String> = n
        .iter()
        .map(|n| &n.1)
        .filter(|t| t.contains("trying again in"))
        .collect();
    assert!(!began.is_empty() && began.len() <= 2, "{n:?}");
    assert!(
        began[0].contains("for up to 30 min") && began[0].contains("Stop to cancel"),
        "{}",
        began[0]
    );
    assert!(
        n.iter()
            .any(|n| n.0 == NoticeLevel::Info && n.1.contains("answered again after")),
        "{n:?}"
    );
    assert_eq!(r.engine_status(&id).await, SessionStatus::Idle);
}

#[tokio::test(start_paused = true)]
async fn unattended_work_gives_up_when_the_patience_ends_and_says_so() {
    let r = rig(
        503,
        None,
        Duration::from_secs(3 * 3600),
        "permission = \"auto\"",
    )
    .await;
    let t0 = Instant::now();
    let id = r.start("do the night shift").await;
    r.finish(&id).await;
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_secs(29 * 60) && took <= Duration::from_secs(31 * 60),
        "{took:?}"
    );
    let entries = r.entries(&id).await;
    let n = notices(&entries);
    let last = n.last().unwrap();
    assert_eq!(last.0, NoticeLevel::Error, "{n:?}");
    assert!(last.1.contains("kept trying for 30 min"), "{}", last.1);
    // A notice every ten minutes at most: the story is not 20 lines long.
    assert!(n.len() <= 6, "{n:?}");
}

#[tokio::test(start_paused = true)]
async fn a_servers_retry_after_is_honoured_and_stop_cancels_the_wait_at_once() {
    let r = rig(
        503,
        Some("90"),
        Duration::from_secs(3600),
        "permission = \"auto\"",
    )
    .await;
    let id = r.start("do the night shift").await;
    // Let it fail once and start waiting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_secs(80)).await;
    // The server said 90 s: no second try yet (jitter only adds).
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 2);
    let t = Instant::now();
    r.engine
        .handle(
            Request::Abort {
                session_id: id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    r.finish(&id).await;
    // Stop didn't wait out the pause (90 s or more).
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 2);
    let entries = r.entries(&id).await;
    assert!(
        notices(&entries).iter().any(|n| n.1 == "Stopped."),
        "{:?}",
        notices(&entries)
    );
}

#[tokio::test(start_paused = true)]
async fn a_room_the_user_types_in_is_watched_like_a_chat() {
    // Its members answer one after another: a failing one must not hold the
    // others up for half an hour. Each gets the short schedule (362 s on a
    // server of the user's own); nobody is told "this runs unattended".
    let r = rig(529, None, Duration::from_secs(3 * 3600), "").await;
    let t0 = Instant::now();
    let id = r.room("good morning").await;
    r.finish(&id).await;
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_secs(2 * 362) && took < Duration::from_secs(2 * 362 + 30),
        "{took:?}"
    );
    let n = notices(&r.entries(&id).await);
    assert!(!n.iter().any(|n| n.1.contains("runs unattended")), "{n:?}");
    assert_eq!(
        n.iter().filter(|n| n.1.contains("couldn't reply")).count(),
        2,
        "{n:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_message_from_the_user_ends_the_wait() {
    // Unattended, the server asked for 90 seconds; the user comes back after
    // five and writes: the model is asked again at once.
    let r = rig(
        503,
        Some("90"),
        Duration::from_secs(3600),
        "permission = \"auto\"",
    )
    .await;
    let id = r.start("do the night shift").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 1);
    r.say(&id, "are you there?").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        r.flaky.hits.load(Ordering::SeqCst),
        2,
        "asked again at once, not after the 90 seconds"
    );
    let at = r.flaky.times.lock().clone();
    assert!(at[1] < Duration::from_secs(10), "{at:?}");
    r.engine
        .handle(
            Request::Abort {
                session_id: id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    r.finish(&id).await;
}

#[tokio::test(start_paused = true)]
async fn a_long_outage_says_so_every_ten_minutes_and_tells_the_devices_once() {
    // 14 minutes of 503: a notice when it starts, one at the ten-minute
    // mark, a notification to the devices after five minutes, one line when
    // the server answers again.
    let r = rig(
        503,
        None,
        Duration::from_secs(14 * 60),
        "permission = \"auto\"",
    )
    .await;
    let id = r.start("do the night shift").await;
    // Boundaries, not just the eventual count: no device notification before
    // five minutes, one by seven, and no re-notice before minute ten.
    tokio::time::sleep(Duration::from_secs(4 * 60)).await;
    assert!(
        !r.notified()
            .await
            .iter()
            .any(|t| t == "Waiting for the model")
    );
    tokio::time::sleep(Duration::from_secs(3 * 60)).await;
    assert_eq!(
        r.notified()
            .await
            .iter()
            .filter(|t| *t == "Waiting for the model")
            .count(),
        1
    );
    assert!(
        !notices(&r.entries(&id).await)
            .iter()
            .any(|n| n.1.starts_with("Still no answer"))
    );
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    assert!(
        !notices(&r.entries(&id).await)
            .iter()
            .any(|n| n.1.starts_with("Still no answer"))
    );
    tokio::time::sleep(Duration::from_secs(3 * 60)).await;
    assert_eq!(
        notices(&r.entries(&id).await)
            .iter()
            .filter(|n| n.1.starts_with("Still no answer"))
            .count(),
        1
    );
    r.finish(&id).await;
    let entries = r.entries(&id).await;
    assert_eq!(said(&entries), ["back again"]);
    let n = notices(&entries);
    let began: Vec<&String> = n
        .iter()
        .map(|n| &n.1)
        .filter(|t| t.contains("trying again in"))
        .collect();
    assert_eq!(began.len(), 1, "{n:?}");
    let still: Vec<&String> = n
        .iter()
        .map(|n| &n.1)
        .filter(|t| t.starts_with("Still no answer from mock/m after 1"))
        .collect();
    assert_eq!(still.len(), 1, "{n:?}");
    assert!(
        still[0].contains("Trying again in") && still[0].ends_with("Stop to cancel."),
        "{}",
        still[0]
    );
    let sent = r.notified().await;
    assert_eq!(
        sent.iter()
            .filter(|t| *t == "Waiting for the model")
            .count(),
        1,
        "{sent:?}"
    );
    let times = r.flaky.times.lock().clone();
    assert!(times.len() >= 10, "{times:?}");
}

#[tokio::test(start_paused = true)]
async fn the_same_error_again_and_again_from_the_users_own_server_stops_the_wait() {
    // A model that fails to load answers 500 every time: unattended work stops
    // after five identical errors, not after half an hour.
    let r = rig_with(
        500,
        "failed to load model",
        None,
        Duration::from_secs(3 * 3600),
        "permission = \"auto\"",
        vec![],
    )
    .await;
    let t0 = Instant::now();
    let id = r.start("do the night shift").await;
    r.finish(&id).await;
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_secs(15) && took < Duration::from_secs(45),
        "{took:?}"
    );
    let hits = r.flaky.hits.load(Ordering::SeqCst);
    assert_eq!(hits, 5);
    let last = notices(&r.entries(&id).await).pop().unwrap();
    assert_eq!(last.0, NoticeLevel::Error);
    assert!(
        last.1.contains("failed to load model") && last.1.contains("same answer"),
        "{}",
        last.1
    );
    // The same outage as a 503 (a model still loading) is waited out.
    let r = rig_with(
        503,
        "model is loading",
        None,
        Duration::from_secs(10 * 60),
        "permission = \"auto\"",
        vec![],
    )
    .await;
    let id = r.start("do the night shift").await;
    r.finish(&id).await;
    assert_eq!(said(&r.entries(&id).await), ["back again"]);
}

#[tokio::test(start_paused = true)]
async fn a_job_is_unattended_even_without_yolo_permission() {
    let r = rig(503, None, Duration::from_secs(10 * 60), "").await;
    let job: Job = serde_json::from_value(
        r.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Night shift".into(),
                        schedule: "0 8 * * *".into(),
                        prompt: "do the night shift".into(),
                        cwd: r.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Default,
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
    let s: SessionInfo = serde_json::from_value(
        r.engine
            .handle(Request::RunJob { id: job.id }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(s.kind, SessionKind::Job);
    assert_eq!(s.permission, PermissionMode::Default);
    // RunJob queues its task before setting the session running. Wait for
    // the mock's first request so wait_idle cannot observe that initial gap.
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.flaky.hits.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the job never started");
    r.finish(&s.id).await;
    let entries = r.entries(&s.id).await;
    assert_eq!(said(&entries), ["back again"]);
    assert!(
        notices(&entries)
            .iter()
            .any(|n| n.1.contains("runs unattended"))
    );
    assert!(
        !r.notified()
            .await
            .iter()
            .any(|t| t == "Waiting for the model")
    );
}

#[tokio::test(start_paused = true)]
async fn a_bot_channel_waits_out_an_outage_without_yolo_permission() {
    let r = rig_with(
        503,
        "model is loading",
        None,
        Duration::from_secs(10 * 60),
        "",
        vec![tool_call(
            "message_bot",
            json!({"bot": "@scout", "message": "look around", "wait": true}),
        )],
    )
    .await;
    let bot: Bot = serde_json::from_value(json!({
        "id": "", "name": "Scout", "handle": "", "avatar": "", "color": "", "role": "helps",
        "personality": "", "instructions": "", "permission": "default", "tools": "assistant"
    }))
    .unwrap();
    r.engine
        .handle(Request::SaveBot { bot }, None)
        .await
        .unwrap();
    let id = r.start("ask the scout").await;
    r.finish(&id).await;
    let s = r
        .sessions()
        .await
        .into_iter()
        .find(|s| s.kind == SessionKind::Channel)
        .unwrap();
    assert!(s.bot_id.is_some());
    assert_eq!(s.permission, PermissionMode::Default);
    let entries = r.entries(&s.id).await;
    assert_eq!(said(&entries), ["back again"]);
    assert!(
        notices(&entries)
            .iter()
            .any(|n| n.1.contains("runs unattended"))
    );
    assert!(
        !r.notified()
            .await
            .iter()
            .any(|t| t == "Waiting for the model")
    );
    let results = tool_results(&r.entries(&id).await);
    let (tool, is_error, text) = results.first().unwrap();
    assert_eq!(tool, "message_bot");
    assert!(!is_error && text.contains("back again"), "{text}");
}
fn helper_script() -> Vec<Vec<Value>> {
    vec![tool_call(
        "agent",
        json!({"wait": true, "tasks": [{"name": "scout", "prompt": "look around"}]}),
    )]
}

#[tokio::test(start_paused = true)]
async fn a_helper_waits_out_an_outage_within_its_budget() {
    // The chat is watched; its helper is not: it keeps trying through a
    // 5-minute outage (its budget is 10 minutes) and reports when the server
    // is back.
    let r = rig_with(
        529,
        "Overloaded",
        None,
        Duration::from_secs(5 * 60),
        "subagent_minutes = 10",
        helper_script(),
    )
    .await;
    let t0 = Instant::now();
    let id = r.start("send a scout").await;
    r.finish(&id).await;
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_secs(5 * 60) && took < Duration::from_secs(7 * 60),
        "{took:?}"
    );
    // More tries than a chat on the screen would make (the helper is patient).
    assert!(r.flaky.hits.load(Ordering::SeqCst) >= 8);
    let results = tool_results(&r.entries(&id).await);
    let (tool, is_error, text) = results.last().unwrap();
    assert_eq!(tool, "agent");
    assert!(!is_error, "{text}");
    assert!(text.contains("back again"), "{text}");
    assert_eq!(r.engine_status(&id).await, SessionStatus::Idle);
}

#[tokio::test(start_paused = true)]
async fn a_helper_is_stopped_by_its_own_deadline_not_kept_waiting() {
    // Its budget is 1 minute (a hard stop 3 minutes after that), the outage
    // lasts 6: the helper's patience doesn't outlast its budget. It is stopped
    // at its deadline, the chat is told, and the chat's own turn carries on.
    let r = rig_with(
        529,
        "Overloaded",
        None,
        Duration::from_secs(6 * 60),
        "subagent_minutes = 1",
        helper_script(),
    )
    .await;
    let t0 = Instant::now();
    let id = r.start("send a scout").await;
    r.finish(&id).await;
    let took = t0.elapsed();
    let at = r.flaky.times.lock().clone();
    // The helper's last try came before its deadline (4 minutes); what comes
    // after is the chat's own (watched) retries.
    assert!(
        at.iter()
            .filter(|t| **t < Duration::from_secs(4 * 60))
            .count()
            >= 7,
        "{at:?}"
    );
    assert!(took < Duration::from_secs(10 * 60), "{took:?}");
    let results = tool_results(&r.entries(&id).await);
    let (tool, is_error, text) = results.first().unwrap();
    assert_eq!(tool, "agent");
    assert!(*is_error, "{text}");
    assert!(
        text.contains("This attempt timed out before it finished."),
        "{text}"
    );
    assert_eq!(r.engine_status(&id).await, SessionStatus::Idle);
}

impl Rig {
    async fn engine_status(&self, id: &str) -> SessionStatus {
        let v = self
            .engine
            .handle(
                Request::GetSession {
                    session_id: id.into(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap();
        serde_json::from_value::<SessionSnapshot>(v)
            .unwrap()
            .info
            .status
    }
}

#[tokio::test(start_paused = true)]
async fn ordinary_429_with_long_headers_uses_the_common_unattended_budget() {
    for retry_after in ["600", "3600"] {
        let r = rig(
            429,
            Some(retry_after),
            Duration::from_secs(3 * 60 * 60),
            "permission = \"auto\"",
        )
        .await;
        let id = r.start("work overnight").await;
        r.finish(&id).await;
        let took = r.flaky.first.lock().unwrap().elapsed();
        assert!(
            // A floor longer than the remaining patience ends the common
            // budget early: its600s+jitter pauses can stop just after20min.
            took >= Duration::from_secs(20 * 60) && took <= Duration::from_secs(30 * 60 + 10),
            "{retry_after}: {took:?}"
        );
        let entries = r.entries(&id).await;
        assert!(
            notices(&entries)
                .iter()
                .any(|n| n.1.contains("kept trying for")),
            "{entries:?}"
        );
        assert!(
            notices(&entries)
                .iter()
                .any(|n| n.1.contains("for up to 30 min"))
        );
        assert!(
            !notices(&entries)
                .iter()
                .any(|n| n.1.starts_with("Waiting for the usage limit")
                    || n.1.starts_with("Continuing when it resets"))
        );
        assert_eq!(
            r.notified()
                .await
                .iter()
                .filter(|t| *t == "Waiting for the model")
                .count(),
            1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_message_before_the_error_response_wakes_the_retry_exactly_once() {
    for status in [429, 503] {
        let r = rig(
            status,
            Some("3600"),
            Duration::from_secs(3 * 60 * 60),
            "permission = \"auto\"",
        )
        .await;
        r.flaky.hold_first.store(true, Ordering::SeqCst);
        let id = r.start("hello").await;
        r.flaky.first_started.notified().await;
        r.engine
            .handle(
                Request::Send {
                    session_id: id.clone(),
                    content: vec![ContentPart::text("also consider this")],
                    delivery: Delivery::Auto,
                    attachments: vec![],
                },
                None,
            )
            .await
            .unwrap();
        r.flaky.release_first.notify_one();
        r.wait_hits(2).await;
        assert_eq!(
            r.flaky.hits.load(Ordering::SeqCst),
            2,
            "{status}: message did not wake once"
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            r.flaky.hits.load(Ordering::SeqCst),
            2,
            "{status}: queued message made a busy loop"
        );
        r.engine
            .handle(
                Request::Abort {
                    session_id: id.clone(),
                },
                None,
            )
            .await
            .unwrap();
        r.finish(&id).await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_explicit_plan_reset_is_longer_but_still_message_and_stop_aware() {
    let r = rig(429, Some("3600"), Duration::from_secs(90 * 60), "").await;
    r.flaky.plan_reset.store(true, Ordering::SeqCst);
    r.flaky.hold_first.store(true, Ordering::SeqCst);
    let id = r.start("hello").await;
    r.flaky.first_started.notified().await;
    r.engine
        .handle(
            Request::Send {
                session_id: id.clone(),
                content: vec![ContentPart::text("try again")],
                delivery: Delivery::Auto,
                attachments: vec![],
            },
            None,
        )
        .await
        .unwrap();
    r.flaky.release_first.notify_one();
    r.wait_hits(2).await;
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 2);
    tokio::time::sleep(Duration::from_secs(31 * 60)).await;
    assert_eq!(r.engine_status(&id).await, SessionStatus::Running);
    assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 2);
    assert!(
        notices(&r.entries(&id).await)
            .iter()
            .any(|n| n.1.contains("usage limit"))
    );
    r.engine
        .handle(
            Request::Abort {
                session_id: id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    r.finish(&id).await;
}

#[tokio::test(start_paused = true)]
async fn loading_errors_end_after_five_but_loading_progress_recovers() {
    for message in [
        "error loading model: out of memory",
        "failed loading model",
        "loading model failed",
    ] {
        let r = rig_with(
            500,
            message,
            None,
            Duration::from_secs(3 * 60 * 60),
            "permission = \"auto\"",
            vec![],
        )
        .await;
        let id = r.start("work overnight").await;
        r.finish(&id).await;
        assert_eq!(r.flaky.hits.load(Ordering::SeqCst), 5, "{message}");
    }
    let r = rig_with(
        500,
        "model is loading",
        None,
        Duration::from_secs(3 * 60),
        "permission = \"auto\"",
        vec![],
    )
    .await;
    let id = r.start("work overnight").await;
    r.finish(&id).await;
    assert_eq!(said(&r.entries(&id).await), ["back again"]);
    assert!(r.flaky.hits.load(Ordering::SeqCst) > 5);
}
