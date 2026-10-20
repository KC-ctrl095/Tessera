//! Exercise the real byte writer and the real reconnect loop, without accounts.
use super::*;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::time::{Instant, Sleep};
use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

#[tokio::test(start_paused = true)]
async fn reads_zero_byte_writes_and_flushes_are_not_write_progress() {
    let (a, mut b) = tokio::io::duplex(16);
    let progress = WriteProgress::new();
    let mut io = ProgressIo::new(a, progress.clone());
    let start = Instant::now();
    tokio::time::advance(Duration::from_secs(30)).await;
    b.write_all(b"read only").await.unwrap();
    let mut buf = [0; 9];
    io.read_exact(&mut buf).await.unwrap();
    assert_eq!(io.write(&[]).await.unwrap(), 0);
    assert_eq!(
        io.write_vectored(&[io::IoSlice::new(&[])]).await.unwrap(),
        0
    );
    io.flush().await.unwrap();
    progress.stalled().await;
    assert_eq!(start.elapsed(), Duration::from_secs(40));
}

#[tokio::test(start_paused = true)]
async fn vectored_byte_writes_renew_the_deadline() {
    let (a, _b) = tokio::io::duplex(16);
    let progress = WriteProgress::new();
    let mut io = ProgressIo::new(a, progress.clone());
    let start = Instant::now();
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(
        io.write_vectored(&[io::IoSlice::new(b"wire bytes")])
            .await
            .unwrap()
            > 0
    );
    progress.stalled().await;
    assert_eq!(start.elapsed(), Duration::from_secs(70));
}

struct DripIo {
    delay: Pin<Box<Sleep>>,
    writes: usize,
    stall_after: usize,
    bytes: Arc<AtomicU64>,
}

impl DripIo {
    fn new(stall_after: usize) -> Self {
        Self {
            delay: Box::pin(tokio::time::sleep(Duration::from_secs(10))),
            writes: 0,
            stall_after,
            bytes: Arc::default(),
        }
    }
}

impl AsyncRead for DripIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

impl AsyncWrite for DripIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if me.writes >= me.stall_after {
            return Poll::Pending;
        }
        if std::future::Future::poll(me.delay.as_mut(), cx).is_pending() {
            return Poll::Pending;
        }
        let n = buf.len().min(8192);
        me.bytes.fetch_add(n as u64, Ordering::Relaxed);
        me.writes += 1;
        me.delay
            .as_mut()
            .reset(Instant::now() + Duration::from_secs(10));
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(start_paused = true)]
async fn real_byte_progress_keeps_a_large_frame_alive_past_forty_seconds() {
    let io = DripIo::new(usize::MAX);
    let bytes = io.bytes.clone();
    let progress = WriteProgress::new();
    let mut ws =
        WebSocketStream::from_raw_socket(ProgressIo::new(io, progress.clone()), Role::Client, None)
            .await;
    let start = Instant::now();
    assert!(
        send_frame(
            &mut ws,
            tungstenite::Message::Binary(vec![0; 65_536].into()),
            &tokio_util_lite::Stop::default(),
            &progress
        )
        .await
    );
    assert_eq!(start.elapsed(), Duration::from_secs(90));
    assert!(
        bytes.load(Ordering::Relaxed) > 65_536,
        "full frame plus WebSocket header written"
    );
}

#[tokio::test(start_paused = true)]
async fn a_partial_write_times_out_forty_seconds_after_its_last_bytes() {
    let io = DripIo::new(2);
    let bytes = io.bytes.clone();
    let progress = WriteProgress::new();
    let mut ws =
        WebSocketStream::from_raw_socket(ProgressIo::new(io, progress.clone()), Role::Client, None)
            .await;
    let start = Instant::now();
    assert!(
        !send_frame(
            &mut ws,
            tungstenite::Message::Binary(vec![0; 65_536].into()),
            &tokio_util_lite::Stop::default(),
            &progress
        )
        .await
    );
    assert_eq!(bytes.load(Ordering::Relaxed), 16_384);
    assert_eq!(start.elapsed(), Duration::from_secs(60));
}

#[tokio::test(start_paused = true)]
async fn close_interrupts_a_progressing_large_frame_without_waiting_for_idle() {
    let progress = WriteProgress::new();
    let io = DripIo::new(usize::MAX);
    let bytes = io.bytes.clone();
    let mut ws =
        WebSocketStream::from_raw_socket(ProgressIo::new(io, progress.clone()), Role::Client, None)
            .await;
    let stop = tokio_util_lite::Stop::default();
    let start = Instant::now();
    let cancel = async {
        tokio::time::sleep(Duration::from_secs(45)).await;
        stop.stop();
    };
    let (sent, ()) = tokio::join!(
        send_frame(
            &mut ws,
            tungstenite::Message::Binary(vec![0; 65_536].into()),
            &stop,
            &progress
        ),
        cancel
    );
    assert!(!sent);
    assert_eq!(start.elapsed(), Duration::from_secs(45));
    assert_eq!(bytes.load(Ordering::Relaxed), 32_768);
}

#[tokio::test(start_paused = true)]
async fn partial_send_reconnect_preserves_posts_completes_waiters_and_ends() {
    tokio::time::timeout(Duration::from_secs(120), reconnect_scenario())
        .await
        .unwrap();
}

async fn reconnect_scenario() {
    let (a, server_a) = tokio::io::duplex(64);
    let (b, server_b) = tokio::io::duplex(64);
    let progress_a = WriteProgress::new();
    let progress_b = WriteProgress::new();
    let sockets = Arc::new(Mutex::new(std::collections::VecDeque::from([
        (
            WebSocketStream::from_raw_socket(
                ProgressIo::new(a, progress_a.clone()),
                Role::Client,
                None,
            )
            .await,
            Link::Plain,
            progress_a,
        ),
        (
            WebSocketStream::from_raw_socket(
                ProgressIo::new(b, progress_b.clone()),
                Role::Client,
                None,
            )
            .await,
            Link::Plain,
            progress_b,
        ),
    ])));
    let (partial_tx, partial_rx) = oneshot::channel();
    let (drop_tx, drop_rx) = oneshot::channel();
    let first = tokio::spawn(async move {
        let mut ws = WebSocketStream::from_raw_socket(server_a, Role::Server, None).await;
        let hello: ClientMessage =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(hello.id, 0);
        // Read only part of the next frame; the 64-byte pipe then fills again.
        let mut bytes = [0; 16];
        assert!(ws.get_mut().read(&mut bytes).await.unwrap() > 0);
        partial_tx.send(()).unwrap();
        let _ = drop_rx.await;
    });
    let (ready_tx, ready_rx) = oneshot::channel();
    let second = tokio::spawn(async move {
        let mut ws = WebSocketStream::from_raw_socket(server_b, Role::Server, None).await;
        for id in [0, 2, 3] {
            let msg: ClientMessage =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(
                msg.id, id,
                "partially sent request must not replay; posts keep order"
            );
        }
        ready_tx.send(()).unwrap();
        let msg: ClientMessage =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(msg.id, 4);
        ws.send(tungstenite::Message::Text(
            serde_json::to_string(&ServerMessage::Response {
                id: 4,
                result: serde_json::json!("after reconnect"),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        assert!(
            ws.next().await.is_none_or(|r| r.is_err()),
            "Close drops the connection"
        );
    });
    let outbound = Arc::new(Outbox::default());
    let rx = outbound.clone();
    let waiters: Waiters = Arc::default();
    let (lost_tx, lost_rx) = oneshot::channel();
    waiters.lock().insert(1, lost_tx);
    let stop = Arc::new(tokio_util_lite::Stop::default());
    let (status, _) = watch::channel(ConnectionStatus::new(ConnState::Connecting, None, None));
    let handle = tokio::spawn(manager_with_connect(
        Arc::default(),
        rx,
        waiters.clone(),
        broadcast::channel(16).0,
        status,
        stop.clone(),
        Arc::default(),
        Arc::new(AtomicU64::new(0)),
        move |_| {
            let next = sockets
                .lock()
                .pop_front()
                .expect("only two connections needed");
            async move { Ok(("http://fixture".into(), next)) }
        },
    ));
    outbound
        .send(QueuedRequest {
            id: 1,
            request: Request::Send {
                session_id: "fixture".into(),
                content: vec![tessera_proto::ContentPart::text("x".repeat(65_536))],
                delivery: tessera_proto::Delivery::Auto,
                attachments: vec![],
            },
            expires: None,
            epoch: None,
        })
        .unwrap();
    partial_rx.await.unwrap();
    for id in [2, 3] {
        outbound
            .send(QueuedRequest {
                id,
                request: Request::Hello {
                    client: format!("post-{id}"),
                    version: "test".into(),
                },
                expires: None,
                epoch: None,
            })
            .unwrap();
    }
    let error = lost_rx.await.unwrap().unwrap_err();
    assert_eq!(error.message, "connection lost");
    ready_rx.await.unwrap();
    let (reply_tx, reply_rx) = oneshot::channel();
    waiters.lock().insert(4, reply_tx);
    outbound
        .send(QueuedRequest {
            id: 4,
            request: Request::ListJobs,
            expires: Some(Instant::now() + Duration::from_secs(5)),
            epoch: None,
        })
        .unwrap();
    assert_eq!(
        reply_rx.await.unwrap().unwrap(),
        serde_json::json!("after reconnect")
    );
    stop.stop();
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(waiters.lock().is_empty());
    drop_tx.send(()).unwrap();
    first.await.unwrap();
    second.await.unwrap();
}

// A real duplex byte stream whose large frames advance 8 KiB per ten seconds.
// Small control frames remain immediate once the large frame has flushed.
struct SlowDuplex {
    inner: tokio::io::DuplexStream,
    delay: Pin<Box<Sleep>>,
    slow: bool,
}

impl AsyncRead for SlowDuplex {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for SlowDuplex {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if !me.slow && buf.len() > 8192 {
            me.slow = true;
            me.delay
                .as_mut()
                .reset(Instant::now() + Duration::from_secs(10));
        }
        if me.slow && std::future::Future::poll(me.delay.as_mut(), cx).is_pending() {
            return Poll::Pending;
        }
        let limit = if me.slow {
            buf.len().min(8192)
        } else {
            buf.len()
        };
        let result = Pin::new(&mut me.inner).poll_write(cx, &buf[..limit]);
        if me.slow && matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            me.delay
                .as_mut()
                .reset(Instant::now() + Duration::from_secs(10));
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        let result = Pin::new(&mut me.inner).poll_flush(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            me.slow = false;
        }
        result
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[tokio::test(start_paused = true)]
async fn plain_upload_survives_real_heartbeat_and_off_cycle_nudge() {
    tokio::time::timeout(Duration::from_secs(360), healthy_slow_manager(false))
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn noise_upload_survives_real_heartbeat_and_off_cycle_nudge() {
    tokio::time::timeout(Duration::from_secs(360), healthy_slow_manager(true))
        .await
        .unwrap();
}

fn noise_links() -> (Link, Link) {
    let identity = tessera_secure::Identity::generate().unwrap();
    let (initiator, first) = tessera_secure::Initiator::start(identity.public()).unwrap();
    let (server, reply) = tessera_secure::accept(&identity, &first).unwrap();
    let client = initiator.finish(&reply).unwrap();
    (
        Link::Secure {
            sealer: client.sealer,
            opener: client.opener,
        },
        Link::Secure {
            sealer: server.sealer,
            opener: server.opener,
        },
    )
}

async fn healthy_slow_manager(secure: bool) {
    let (client_link, server_link) = if secure {
        noise_links()
    } else {
        (Link::Plain, Link::Plain)
    };
    let start = Instant::now();
    let (a, b) = tokio::io::duplex(131_072);
    let progress = WriteProgress::new();
    let io = SlowDuplex {
        inner: a,
        delay: Box::pin(tokio::time::sleep(Duration::from_secs(10))),
        slow: false,
    };
    let socket = Arc::new(Mutex::new(Some((
        WebSocketStream::from_raw_socket(ProgressIo::new(io, progress.clone()), Role::Client, None)
            .await,
        client_link,
        progress,
    ))));
    let (hello_tx, hello_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut ws = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
        let (mut server_writer, mut server_reader) = server_link.split();
        let hello = server_reader
            .receive(ws.next().await.unwrap().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&hello).unwrap().id, 0);
        hello_tx.send(()).unwrap();
        let mut pings = 0;
        let mut pongs = 0;
        // Independent peer heartbeats while the request frame is incomplete.
        // Use a separate writer half so neither peer's reader is starved.
        let (mut peer_writer, mut peer_reader) = ws.split();
        let request = {
            let request = async {
                loop {
                    let m = peer_reader.next().await.unwrap().unwrap();
                    match m {
                        tungstenite::Message::Ping(_) => {
                            pings += 1;
                        }
                        tungstenite::Message::Pong(_) => {
                            pongs += 1;
                        }
                        m => {
                            if let Some(text) = server_reader.receive(m).unwrap() {
                                break serde_json::from_str::<ClientMessage>(&text).unwrap();
                            }
                        }
                    }
                }
            };
            let heartbeats = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(25)).await;
                    peer_writer
                        .send(tungstenite::Message::Ping(Vec::new().into()))
                        .await
                        .unwrap();
                }
            };
            tokio::pin!(request, heartbeats);
            tokio::select! { request = &mut request => request, _ = &mut heartbeats => unreachable!() }
        };
        assert_eq!(request.id, 1);
        assert!(
            pings >= 1,
            "foreground/periodic Ping passed between upload frames"
        );
        assert!(
            pongs >= 1,
            "server Ping answered before the whole upload finished"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
        let reply = serde_json::to_string(&ServerMessage::Response {
            id: 1,
            result: serde_json::json!("healthy slow link"),
        })
        .unwrap();
        assert!(
            server_writer
                .send(
                    &mut peer_writer,
                    reply,
                    &tokio_util_lite::Stop::default(),
                    &WriteProgress::new(),
                    &Controls::new()
                )
                .await
        );
        while let Some(msg) = peer_reader.next().await {
            if msg.is_err() {
                break;
            }
        }
    });
    let outbound = Arc::new(Outbox::default());
    let rx = outbound.clone();
    let waiters: Waiters = Arc::default();
    let (reply_tx, reply_rx) = oneshot::channel();
    waiters.lock().insert(1, reply_tx);
    let stop = Arc::new(tokio_util_lite::Stop::default());
    let nudge = Arc::new(tokio::sync::Notify::new());
    let connect_count = Arc::new(AtomicU64::new(0));
    let counter = connect_count.clone();
    let (status, _) = watch::channel(ConnectionStatus::new(ConnState::Connecting, None, None));
    let handle = tokio::spawn(manager_with_connect(
        Arc::default(),
        rx,
        waiters.clone(),
        broadcast::channel(16).0,
        status,
        stop.clone(),
        nudge.clone(),
        Arc::new(AtomicU64::new(0)),
        move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            let next = socket
                .lock()
                .take()
                .expect("healthy connection must not reconnect");
            async move { Ok(("http://fixture".into(), next)) }
        },
    ));
    hello_rx.await.unwrap();
    outbound
        .send(QueuedRequest {
            id: 1,
            request: Request::Send {
                session_id: "fixture".into(),
                content: vec![tessera_proto::ContentPart::text("x".repeat(3 * 65_518))],
                delivery: tessera_proto::Delivery::Auto,
                attachments: vec![],
            },
            expires: Some(Instant::now() + Duration::from_secs(350)),
            epoch: None,
        })
        .unwrap();
    let nudger = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        nudge.notify_one();
    });
    assert_eq!(
        reply_rx.await.unwrap().unwrap(),
        serde_json::json!("healthy slow link")
    );
    assert!(start.elapsed() >= Duration::from_secs(241));
    assert_eq!(connect_count.load(Ordering::Relaxed), 1);
    stop.stop();
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(waiters.lock().is_empty());
    nudger.await.unwrap();
    server.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_silent_progressing_write_reconnects_at_the_fixed_read_deadline() {
    reconnect_deadline_control(true, false, false, Duration::from_millis(40_500)).await;
}

#[tokio::test(start_paused = true)]
async fn repeated_successful_sends_do_not_extend_the_read_deadline() {
    reconnect_deadline_control(false, true, false, Duration::from_millis(40_500)).await;
}

#[tokio::test(start_paused = true)]
async fn peer_that_stops_responding_reconnects_forty_seconds_after_last_read() {
    reconnect_deadline_control(true, false, true, Duration::from_millis(60_500)).await;
}

#[tokio::test(start_paused = true)]
async fn repeated_nudges_and_writes_do_not_extend_the_probe_deadline() {
    reconnect_deadline_control(false, false, false, Duration::from_millis(7500)).await;
}

async fn reconnect_deadline_control(
    slow: bool,
    repeated: bool,
    responds_first: bool,
    expected: Duration,
) {
    tokio::time::timeout(expected + Duration::from_secs(2), async {
        let start = Instant::now();
        let (a, b) = tokio::io::duplex(131_072);
        let progress = WriteProgress::new();
        let io = SlowDuplex {
            inner: a,
            delay: Box::pin(tokio::time::sleep(Duration::from_secs(10))),
            slow: false,
        };
        let first = Arc::new(Mutex::new(Some((
            WebSocketStream::from_raw_socket(
                ProgressIo::new(io, progress.clone()),
                Role::Client,
                None,
            )
            .await,
            Link::Plain,
            progress,
        ))));
        let (reconnect_tx, reconnect_rx) = oneshot::channel();
        let reconnect_tx = Arc::new(Mutex::new(Some(reconnect_tx)));
        let peer = tokio::spawn(async move {
            // Drain bytes without WebSocket's automatic Pong responses: this
            // peer must remain silent despite accepting outgoing requests.
            let (mut stream, mut sink) = tokio::io::split(b);
            let read = async {
                let mut buf = [0; 8192];
                while stream.read(&mut buf).await.unwrap_or(0) != 0 {}
            };
            let pulse = async {
                if responds_first {
                    for _ in 0..2 {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        // Unmasked empty server Pong frame.
                        sink.write_all(&[0x8a, 0]).await.unwrap();
                    }
                }
                std::future::pending::<()>().await;
            };
            tokio::pin!(read, pulse);
            tokio::select! { _ = &mut read => {}, _ = &mut pulse => {} }
        });
        let outbound = Arc::new(Outbox::default());
        let stop = Arc::new(tokio_util_lite::Stop::default());
        let nudge = Arc::new(tokio::sync::Notify::new());
        let (status, _) = watch::channel(ConnectionStatus::new(ConnState::Connecting, None, None));
        let handle = tokio::spawn(manager_with_connect(
            Arc::default(),
            outbound.clone(),
            Arc::default(),
            broadcast::channel(16).0,
            status,
            stop.clone(),
            nudge.clone(),
            Arc::new(AtomicU64::new(0)),
            move |_| {
                let socket = first.lock().take();
                if socket.is_none() {
                    reconnect_tx
                        .lock()
                        .take()
                        .unwrap()
                        .send(Instant::now())
                        .unwrap();
                }
                async move {
                    match socket {
                        Some(socket) => Ok(("http://fixture".into(), socket)),
                        None => std::future::pending().await,
                    }
                }
            },
        ));
        let post = if slow {
            Request::Send {
                session_id: "fixture".into(),
                content: vec![tessera_proto::ContentPart::text("x".repeat(65_536))],
                delivery: tessera_proto::Delivery::Auto,
                attachments: vec![],
            }
        } else {
            Request::ListJobs
        };
        outbound.post(1, post).unwrap();
        let posts = outbound.clone();
        let producer = tokio::spawn(async move {
            if repeated {
                for id in 2..6 {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    posts.post(id, Request::ListJobs).unwrap();
                }
            }
            if expected == Duration::from_millis(7500) {
                tokio::time::sleep(Duration::from_secs(5)).await;
                nudge.notify_one();
                tokio::time::sleep(Duration::from_secs(1)).await;
                nudge.notify_one();
            }
        });
        let reconnect = reconnect_rx.await.unwrap();
        assert_eq!(
            reconnect.duration_since(start),
            expected,
            "writes never renew inbound/probe deadlines"
        );
        stop.stop();
        handle.await.unwrap();
        producer.await.unwrap();
        peer.await.unwrap();
        assert_eq!(outbound.used(), (0, 0, 0));
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn short_probe_begins_only_after_the_actual_ping_flush() {
    let controls = Controls::new();
    let nudge = tokio::sync::Notify::new();
    let stop = tokio_util_lite::Stop::default();
    let waiters = Waiters::default();
    let events = broadcast::channel(16).0;
    let start = Instant::now();
    let mut reader = ReadLink::Plain;
    let mut input = futures::stream::pending();
    let read = connection_reader(
        &mut reader,
        &mut input,
        &waiters,
        &events,
        &stop,
        &nudge,
        &controls,
    );
    let writer = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        nudge.notify_one();
        tokio::time::sleep(Duration::from_secs(7)).await;
        assert!(
            controls.subscribe().borrow().is_none(),
            "queued ping has no probe deadline"
        );
        let (a, _b) = tokio::io::duplex(64);
        let progress = WriteProgress::new();
        let mut ws = WebSocketStream::from_raw_socket(
            ProgressIo::new(a, progress.clone()),
            Role::Client,
            None,
        )
        .await;
        assert!(controls.flush(&mut ws, &stop, &progress).await);
        assert_eq!(
            controls
                .subscribe()
                .borrow()
                .unwrap()
                .0
                .duration_since(start),
            Duration::from_secs(12)
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        nudge.notify_one();
    };
    let (reason, ()) = tokio::join!(read, writer);
    assert_eq!(
        reason.as_deref(),
        Some("connection lost while in the background")
    );
    assert_eq!(start.elapsed(), Duration::from_millis(14_500));
}

#[tokio::test(start_paused = true)]
async fn same_tick_probe_reply_cannot_be_rearmed_by_a_pending_flush_notice() {
    let controls = Controls::new();
    let nudge = tokio::sync::Notify::new();
    let stop = tokio_util_lite::Stop::default();
    let waiters = Waiters::default();
    let events = broadcast::channel(16).0;
    let mut reader = ReadLink::Plain;
    let (tx, mut input) = futures::channel::mpsc::unbounded();
    let read = connection_reader(
        &mut reader,
        &mut input,
        &waiters,
        &events,
        &stop,
        &nudge,
        &controls,
    );
    let peer = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        nudge.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (a, _b) = tokio::io::duplex(64);
        let progress = WriteProgress::new();
        let mut ws = WebSocketStream::from_raw_socket(
            ProgressIo::new(a, progress.clone()),
            Role::Client,
            None,
        )
        .await;
        assert!(controls.flush(&mut ws, &stop, &progress).await);
        tx.unbounded_send(Ok(tungstenite::Message::Pong(Vec::new().into())))
            .unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        stop.stop();
    };
    let (reason, ()) = tokio::join!(read, peer);
    assert!(
        reason.is_none(),
        "Pong already seen after flush, even at identical clock time"
    );
}

struct PendingFlushIo {
    inner: tokio::io::DuplexStream,
    delay: Pin<Box<Sleep>>,
    flushing: Arc<tokio::sync::Notify>,
}
impl AsyncRead for PendingFlushIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for PendingFlushIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if std::future::Future::poll(me.delay.as_mut(), cx).is_pending() {
            me.flushing.notify_one();
            return Poll::Pending;
        }
        Pin::new(&mut me.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[tokio::test(start_paused = true)]
async fn pong_during_pending_poll_flush_satisfies_the_probe() {
    let controls = Controls::new();
    let nudge = tokio::sync::Notify::new();
    let stop = tokio_util_lite::Stop::default();
    let waiters = Waiters::default();
    let events = broadcast::channel(16).0;
    let flushing = Arc::new(tokio::sync::Notify::new());
    let mut reader = ReadLink::Plain;
    let (tx, mut input) = futures::channel::mpsc::unbounded();
    let read = connection_reader(
        &mut reader,
        &mut input,
        &waiters,
        &events,
        &stop,
        &nudge,
        &controls,
    );
    let start = Instant::now();
    let write = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        nudge.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (a, _b) = tokio::io::duplex(64);
        let progress = WriteProgress::new();
        let io = PendingFlushIo {
            inner: a,
            delay: Box::pin(tokio::time::sleep(Duration::from_secs(2))),
            flushing: flushing.clone(),
        };
        let mut ws = WebSocketStream::from_raw_socket(
            ProgressIo::new(io, progress.clone()),
            Role::Client,
            None,
        )
        .await;
        assert!(controls.flush(&mut ws, &stop, &progress).await);
        let (sent, generation) = controls.subscribe().borrow().unwrap();
        assert_eq!(sent.duration_since(start), Duration::from_secs(9));
        assert_eq!(
            generation, 0,
            "captured before Ping, not after pending flush"
        );
        assert_eq!(controls.read_generation(), 1);
    };
    let peer = async {
        flushing.notified().await;
        assert!(controls.subscribe().borrow().is_none(), "flush is pending");
        tx.unbounded_send(Ok(tungstenite::Message::Pong(Vec::new().into())))
            .unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        stop.stop();
    };
    let (reason, (), ()) = tokio::join!(read, write, peer);
    assert!(
        reason.is_none(),
        "Pong while flushing must not arm a false timeout"
    );
    assert_eq!(start.elapsed(), Duration::from_secs(12));
}
