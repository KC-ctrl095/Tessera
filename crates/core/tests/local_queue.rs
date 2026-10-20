//! Two chats on the user's own single-slot model server, end to end with
//! paused time. The server is shaped like a local model mux in front of
//! llama.cpp: one request at a time, and a request that waits 30 seconds for
//! its turn is refused with "503 Local model is busy; retry shortly". The
//! chats take turns, one model request at a time in the order they asked,
//! so the server never sees two at once and nobody is told it is busy.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// The server: one slot, a 30 s wait before "busy", `hold` per answer.
struct Mux {
    slot: tokio::sync::Mutex<()>,
    hold: Duration,
    busy: AtomicUsize,
    active: AtomicUsize,
    most_at_once: AtomicUsize,
    /// The last user message of each request answered, in order.
    served: Mutex<Vec<String>>,
}

async fn chat(State(m): State<Arc<Mux>>, axum::Json(req): axum::Json<Value>) -> Response {
    let Ok(_slot) = tokio::time::timeout(Duration::from_secs(30), m.slot.lock()).await else {
        m.busy.fetch_add(1, Ordering::SeqCst);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": {"message": "Local model is busy; retry shortly"}})),
        )
            .into_response();
    };
    let now = m.active.fetch_add(1, Ordering::SeqCst) + 1;
    m.most_at_once.fetch_max(now, Ordering::SeqCst);
    let who = req["messages"]
        .as_array()
        .and_then(|ms| ms.iter().rev().find(|x| x["role"] == "user"))
        .map(|x| x["content"].to_string())
        .unwrap_or_default();
    tokio::time::sleep(m.hold).await;
    m.served.lock().push(who.clone());
    m.active.fetch_sub(1, Ordering::SeqCst);
    let chunks = [
        json!({"choices": [{"index": 0, "delta": {"content": format!("answered {who}")}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 5}}),
    ];
    let mut body = String::new();
    for c in chunks {
        body.push_str(&format!("data: {c}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], body).into_response()
}

struct Rig {
    engine: Engine,
    mux: Arc<Mux>,
    _dir: tempfile::TempDir,
    work: tempfile::TempDir,
}

async fn rig(hold: Duration) -> Rig {
    let mux = Arc::new(Mux {
        slot: tokio::sync::Mutex::new(()),
        hold,
        busy: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        most_at_once: AtomicUsize::new(0),
        served: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(mux.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    // A server on this computer that doesn't say how many slots it has: one
    // request at a time, as llama.cpp runs for one user.
    let config = format!(
        "default_model = \"local/m\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"local\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\nmax_output = 2000\n"
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Rig {
        engine,
        mux,
        _dir: dir,
        work: tempfile::tempdir().unwrap(),
    }
}

impl Rig {
    async fn start(&self, text: &str) -> String {
        let v = self
            .engine
            .handle(
                Request::CreateSession {
                    cwd: Some(self.work.path().display().to_string()),
                    model: None,
                    // Titled up front: no title request in the story.
                    title: Some(text.into()),
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

    /// Until the server is working on an answer.
    async fn answering(&self) {
        tokio::time::timeout(Duration::from_secs(60), async {
            while self.mux.active.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the first chat never reached the server");
    }

    async fn finish(&self, id: &str) {
        tokio::time::timeout(Duration::from_secs(4 * 3600), self.engine.wait_idle(id))
            .await
            .expect("the turn never ended");
    }

    async fn snapshot(&self, id: &str) -> SessionSnapshot {
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
        serde_json::from_value(v).unwrap()
    }
}

fn notices(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn said(entries: &[Entry]) -> String {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// Both chats send at once; each answer takes `hold`. Returns what each chat
/// ended with.
async fn two_chats(hold: Duration) -> (Rig, [(SessionStatus, Vec<String>, String); 2]) {
    let r = rig(hold).await;
    let a = r.start("first").await;
    r.answering().await;
    let b = r.start("second").await;
    r.finish(&a).await;
    r.finish(&b).await;
    let mut out = Vec::new();
    for id in [&a, &b] {
        let s = r.snapshot(id).await;
        out.push((s.info.status, notices(&s.entries), said(&s.entries)));
    }
    (r, out.try_into().unwrap())
}

#[tokio::test(start_paused = true)]
async fn two_chats_take_turns_instead_of_being_told_the_model_is_busy() {
    // 90 s answers: the second chat's first try would wait 30 s and be
    // refused, and its retry again, before the first chat is done.
    let (r, chats) = two_chats(Duration::from_secs(90)).await;
    for (status, notices, said) in &chats {
        assert_eq!(*status, SessionStatus::Idle, "{notices:?}");
        assert!(
            !notices
                .iter()
                .any(|n| n.contains("busy") || n.contains("retrying")),
            "{notices:?}"
        );
        assert!(said.starts_with("answered"), "{said:?}");
    }
    assert_eq!(
        r.mux.busy.load(Ordering::SeqCst),
        0,
        "the server refused a request"
    );
    assert_eq!(r.mux.most_at_once.load(Ordering::SeqCst), 1);
    // In the order they asked.
    let served = r.mux.served.lock().clone();
    assert_eq!(served.len(), 2, "{served:?}");
    assert!(
        served[0].contains("first") && served[1].contains("second"),
        "{served:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_long_answer_keeps_the_other_chat_waiting_not_failing() {
    // Longer than the whole retry schedule of a watched chat (ten tries over
    // about six minutes, each waiting 30 s at the server): the second chat
    // used to end with "Local model is busy". Now it just waits its turn.
    let (r, chats) = two_chats(Duration::from_secs(20 * 60)).await;
    let (status, notices, said) = &chats[1];
    assert_eq!(*status, SessionStatus::Idle, "{notices:?}");
    assert!(said.contains("second"), "{said:?}");
    assert_eq!(r.mux.busy.load(Ordering::SeqCst), 0);
    assert_eq!(r.mux.most_at_once.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn stop_ends_a_chat_waiting_for_its_turn_at_once() {
    let r = rig(Duration::from_secs(600)).await;
    let a = r.start("first").await;
    r.answering().await;
    let b = r.start("second").await;
    // The second chat is waiting for the first one's answer.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let t0 = tokio::time::Instant::now();
    r.engine
        .handle(
            Request::Abort {
                session_id: b.clone(),
            },
            None,
        )
        .await
        .unwrap();
    r.finish(&b).await;
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    let s = r.snapshot(&b).await;
    assert_ne!(
        s.info.status,
        SessionStatus::Error,
        "{:?}",
        notices(&s.entries)
    );
    // The first chat carries on and gets its answer; the stopped one never
    // reached the server.
    r.finish(&a).await;
    let served = r.mux.served.lock().clone();
    assert_eq!(served.len(), 1, "{served:?}");
    assert!(served[0].contains("first"), "{served:?}");
    assert_eq!(r.mux.busy.load(Ordering::SeqCst), 0);
}
