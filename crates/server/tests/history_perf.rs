//! Reproducible, opt-in huge-history benchmark. Never uses a real provider or home.
//! HISTORY_PAGE_LIMIT=120 selects the opt-in paged request after it is implemented.
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tessera_core::{Engine, config::Paths};
use tessera_proto::*;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

#[derive(Default)]
struct Size(usize);
impl std::io::Write for Size {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn connect(url: &str, public: &[u8], token: &str) -> (Socket, tessera_secure::Channel) {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("{url}/api/secure"))
        .await
        .unwrap();
    let (init, hello) = tessera_secure::Initiator::start(public).unwrap();
    socket.send(Message::Binary(hello.into())).await.unwrap();
    let reply = socket.next().await.unwrap().unwrap().into_data();
    let mut channel = init.finish(&reply).unwrap();
    for f in channel
        .sealer
        .seal(&serde_json::to_vec(&json!({"token":token})).unwrap())
        .unwrap()
    {
        socket.send(Message::Binary(f.into())).await.unwrap();
    }
    loop {
        if let Message::Binary(f) = socket.next().await.unwrap().unwrap()
            && channel.opener.open(&f).unwrap().is_some()
        {
            break;
        }
    }
    (socket, channel)
}

async fn send(socket: &mut Socket, channel: &mut tessera_secure::Channel, value: Value) {
    for f in channel
        .sealer
        .seal(&serde_json::to_vec(&value).unwrap())
        .unwrap()
    {
        socket.send(Message::Binary(f.into())).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual huge-history measurement, ports 18720 and 18721"]
async fn huge_history_data_path() {
    let dir = tempfile::tempdir().unwrap();
    let model = tokio::net::TcpListener::bind("127.0.0.1:18721")
        .await
        .unwrap();
    let model_task = tokio::spawn(async move {
        let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(|| async {
            let stream = futures::stream::unfold(0, |n| async move {
                if n == 1002 { return None; }
                if n == 1001 { return Some((Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data("[DONE]")), n + 1)); }
                tokio::time::sleep(Duration::from_millis(10)).await;
                let chunk = if n == 1000 {
                    json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
                } else {
                    json!({"choices":[{"delta":{"content":format!("{} ", now())},"finish_reason":null}]})
                };
                Some((Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(chunk.to_string())), n + 1))
            });
            axum::response::Sse::new(stream)
        }));
        axum::serve(model, app).await.unwrap();
    });
    std::fs::write(dir.path().join("config.toml"), "default_model = \"fixture/m\"\n[[provider]]\nid = \"fixture\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:18721/v1\"\n[[provider.models]]\nid = \"m\"\ncontext_window = 262144\n").unwrap();
    let engine = Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let create = |title: &str| {
        serde_json::from_value::<Request>(
            json!({"method":"create_session","params":{"title":title,"cwd":dir.path().to_str()}}),
        )
        .unwrap()
    };
    let info: SessionInfo =
        serde_json::from_value(engine.handle(create("Huge fixture"), None).await.unwrap()).unwrap();
    let live: SessionInfo =
        serde_json::from_value(engine.handle(create("Live fixture"), None).await.unwrap()).unwrap();
    let seed = Instant::now();
    for n in 0..5000 {
        let body = match n % 5 {
            0 => EntryBody::User { content: if n % 100 == 0 { vec![ContentPart::text(format!("Question {n}")), ContentPart::Image { mime:"image/png".into(), data:"A".repeat(65536) }] } else { vec![ContentPart::text(format!("Question {n} {}", "word ".repeat(50)))] }, author:None },
            1 => serde_json::from_value(json!({"kind":"assistant","content":[{"type":"text","text":format!("Answer {n} {}", "word ".repeat(50))}],"model":{"provider":"fixture","model":"m"},"usage":{"input":0,"output":0,"cache_read":0,"cache_write":0},"stop":"stop"})).unwrap(),
            2 => serde_json::from_value(json!({"kind":"assistant","content":[{"type":"tool_call","id":format!("call-{n}"),"name":"read","arguments":{"path":"fixture.txt"}}],"model":{"provider":"fixture","model":"m"},"usage":{"input":0,"output":0,"cache_read":0,"cache_write":0},"stop":"stop"})).unwrap(),
            3 => EntryBody::ToolResult { call_id:format!("call-{}",n-1), tool:"read".into(), content:vec![ContentPart::text("fixture output line\n".repeat(1800))], is_error:false, artifact:None, details:None },
            _ if n == 4999 => EntryBody::Compaction { external:false, summary:"Fixture checkpoint".into(), first_kept_seq:4901, tokens_before:100000, tokens_after:1000 },
            _ => EntryBody::Notice { level:NoticeLevel::Info, text:format!("Fixture note {n}"), learned:vec![] },
        };
        engine.store().append(&info.id, body).unwrap();
    }
    eprintln!(
        "HISTORY seed_ms={:.1} entries=5000 cache_budget={}",
        ms(seed),
        tessera_core::store::history_cache_budget()
    );
    let page: Option<u32> = std::env::var("HISTORY_PAGE_LIMIT")
        .ok()
        .map(|v| v.parse().unwrap());
    let req = || {
        if page.is_some() {
            json!({"method":"get_session_page","params":{"session_id":info.id,"limit":page}})
        } else {
            json!({"method":"get_session","params":{"session_id":info.id}})
        }
    };
    for run in 0..3 {
        let t = Instant::now();
        let entries = if let Some(limit) = page {
            engine
                .store()
                .entries_page(&info.id, None, limit)
                .unwrap()
                .entries
        } else {
            engine.store().entries(&info.id, 0).unwrap()
        };
        let sqlite = ms(t);
        let t = Instant::now();
        let display = SessionSnapshot {
            history_revision: 0,
            history_before: None,
            info: info.clone(),
            entries,
            streaming: None,
            approvals: vec![],
            queued: vec![],
            children: vec![],
        };
        let build = ms(t);
        let t = Instant::now();
        let mut size = Size::default();
        serde_json::to_writer(&mut size, &display).unwrap();
        let bound = ms(t);
        let t = Instant::now();
        let value = serde_json::to_value(display).unwrap();
        let to_value = ms(t);
        drop(value);
        eprintln!(
            "HISTORY snapshot_components run={run} build_ms={build:.3} size_check_ms={bound:.3} to_value_ms={to_value:.3} bytes={}",
            size.0
        );
        let t = Instant::now();
        let value = engine
            .handle(serde_json::from_value(req()).unwrap(), None)
            .await
            .unwrap();
        let snapshot = ms(t);
        let count = value["entries"].as_array().unwrap().len();
        let t = Instant::now();
        let encoded = serde_json::to_vec(&ServerMessage::Response {
            id: 42,
            result: value,
        })
        .unwrap();
        let encode = ms(t);
        let identity = tessera_secure::Identity::generate().unwrap();
        let (init, hello) = tessera_secure::Initiator::start(identity.public()).unwrap();
        let (mut server, reply) = tessera_secure::accept(&identity, &hello).unwrap();
        let mut client = init.finish(&reply).unwrap();
        let t = Instant::now();
        let frames = server.sealer.seal(&encoded).unwrap();
        let encrypt = ms(t);
        let t = Instant::now();
        let mut plain = None;
        for frame in &frames {
            plain = client.opener.open(frame).unwrap();
        }
        let decrypt = ms(t);
        let t = Instant::now();
        let decoded: ServerMessage = serde_json::from_slice(&plain.unwrap()).unwrap();
        let decode = ms(t);
        let ServerMessage::Response { result, .. } = decoded else {
            panic!()
        };
        let t = Instant::now();
        let _: SessionSnapshot = serde_json::from_value(result).unwrap();
        let typed = ms(t);
        eprintln!(
            "HISTORY stages run={run} sqlite_read_clone_ms={sqlite:.3} snapshot_bound_value_ms={snapshot:.3} encode_ms={encode:.3} encrypt_ms={encrypt:.3} decrypt_reassemble_ms={decrypt:.3} decode_value_ms={decode:.3} decode_typed_ms={typed:.3} bytes={} frames={} entries={count}",
            encoded.len(),
            frames.len()
        );
    }
    for run in 0..5 {
        let t = Instant::now();
        let history = engine.store().context_history(&info.id).unwrap();
        let history_ms = ms(t);
        let t = Instant::now();
        let view = tessera_core::context::build_view(&history, &Default::default(), None);
        eprintln!(
            "HISTORY step run={run} history_ms={history_ms:.3} view_ms={:.3} entries={} messages={} cached={:?}",
            ms(t),
            history.len(),
            view.messages.len(),
            engine.store().history_cache_stats()
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18720")
        .await
        .unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    let server_task =
        tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    let identity = tessera_server::load_identity(engine.paths()).unwrap();
    let (_, token) = engine.store().add_device("fixture").unwrap();
    for run in 0..3 {
        let (mut socket, mut channel) =
            connect("ws://127.0.0.1:18720", identity.public(), &token).await;
        let (mut other, mut other_channel) =
            connect("ws://127.0.0.1:18720", identity.public(), &token).await;
        let observer = tokio::spawn(async move {
            let mut latency = Vec::new();
            loop {
                let frame = other.next().await.unwrap().unwrap().into_data();
                if let Some(message) = other_channel.opener.open(&frame).unwrap() {
                    let v: Value = serde_json::from_slice(&message).unwrap();
                    if v["event"]["type"] == "stream_delta" {
                        if let Some(text) = v["event"]["delta"]["text"].as_str() {
                            for stamp in text
                                .split_whitespace()
                                .filter_map(|s| s.parse::<u64>().ok())
                            {
                                latency.push(now().saturating_sub(stamp) as f64);
                            }
                        }
                    } else if v["event"]["type"] == "stream_ended" {
                        break;
                    }
                }
            }
            other.close(None).await.unwrap();
            latency.sort_by(f64::total_cmp);
            latency
        });
        send(&mut socket, &mut channel, json!({"id":41,"method":"send","params":{"session_id":live.id,"content":[{"type":"text","text":"Count fixture samples"}]}})).await;
        // Measure with the other chat demonstrably streaming, not while its
        // first cold model preparation is still starting up.
        let warm = Instant::now();
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(30), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_data();
            if let Some(message) = channel.opener.open(&frame).unwrap() {
                let value: Value = serde_json::from_slice(&message).unwrap();
                if value["event"]["type"] == "stream_delta" {
                    break;
                }
            }
        }
        eprintln!("HISTORY stream_start_wait run={run} ms={:.3}", ms(warm));
        let t = Instant::now();
        let mut request = req();
        request["id"] = json!(42);
        send(&mut socket, &mut channel, request).await;
        let mut first = None;
        let mut received = None;
        let mut ended = false;
        let mut frame_decode = 0.0;
        let mut open_ms = 0.0;
        let mut transfer_wait = 0.0;
        let mut gaps = Vec::new();
        let mut latency = Vec::new();
        let mut previous = None::<Instant>;
        loop {
            let waiting = Instant::now();
            let was_partial = first.is_some();
            let msg = tokio::time::timeout(Duration::from_secs(120), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if was_partial {
                transfer_wait += ms(waiting);
            }
            if let Message::Binary(frame) = msg {
                first.get_or_insert_with(|| ms(t));
                let p = Instant::now();
                let message = channel.opener.open(&frame).unwrap();
                let decoding = ms(p);
                open_ms += decoding;
                frame_decode += decoding;
                if let Some(message) = message {
                    let p = Instant::now();
                    let mut v: Value = serde_json::from_slice(&message).unwrap();
                    let parse = ms(p);
                    if v["id"] == 42 {
                        received = Some(ms(t));
                        let p = Instant::now();
                        let _: SessionSnapshot =
                            serde_json::from_value(v["result"].take()).unwrap();
                        eprintln!(
                            "HISTORY wire run={run} request_to_first_frame_ms={:.3} request_to_parsed_ms={:.3} frame_window_ms={:.3} decrypt_all_ms={open_ms:.3} decrypt_response_ms={frame_decode:.3} socket_wait_after_first_ms={transfer_wait:.3} snapshot_parse_value_ms={parse:.3} typed_ms={:.3}",
                            first.unwrap(),
                            received.unwrap(),
                            received.unwrap() - first.unwrap(),
                            ms(p)
                        );
                    } else if v["event"]["type"] == "stream_delta" {
                        if let Some(text) = v["event"]["delta"]["text"].as_str() {
                            for stamp in text
                                .split_whitespace()
                                .filter_map(|s| s.parse::<u64>().ok())
                            {
                                latency.push(now().saturating_sub(stamp) as f64);
                            }
                        }
                        if let Some(previous) = previous {
                            gaps.push(ms(previous));
                        }
                        previous = Some(Instant::now());
                    } else if v["event"]["type"] == "stream_ended" {
                        ended = true;
                    }
                    if ended && received.is_some() {
                        break;
                    }
                    first = None;
                    frame_decode = 0.0;
                    transfer_wait = 0.0;
                }
            }
        }
        gaps.sort_by(f64::total_cmp);
        latency.sort_by(f64::total_cmp);
        eprintln!(
            "HISTORY live run={run} samples={} delta_latency_p50_ms={:.1} p95_ms={:.1} max_ms={:.1} max_gap_ms={:.1}",
            latency.len(),
            latency[latency.len() / 2],
            latency[latency.len() * 95 / 100],
            latency.last().unwrap(),
            gaps.last().unwrap()
        );
        let other = observer.await.unwrap();
        eprintln!(
            "HISTORY other_connection run={run} samples={} delta_latency_p50_ms={:.1} p95_ms={:.1} max_ms={:.1}",
            other.len(),
            other[other.len() / 2],
            other[other.len() * 95 / 100],
            other.last().unwrap()
        );
        socket.close(None).await.unwrap();
        // Let the agent finish persisting its idle status before another run.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    server_task.abort();
    model_task.abort();
}
