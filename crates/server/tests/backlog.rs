//! A device that stops reading (a phone asleep with its connection open)
//! must not make the server hold every event for it: it misses them, and is
//! asked to resync once it reads again.

use std::time::Duration;

use tessera_core::config::Paths;
use tessera_core::engine::MemoryReport;
use tessera_proto::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn bot(n: usize, instructions: String) -> Bot {
    serde_json::from_value(serde_json::json!({
        "id": format!("big{n}"), "name": format!("Big {n}"), "handle": format!("big{n}"), "avatar": "B",
        "color": "#336699", "role": "r", "personality": "p",
        "instructions": instructions, "permission": "default", "tools": "read_only"
    }))
    .unwrap()
}

async fn report(engine: &tessera_core::Engine) -> MemoryReport {
    serde_json::from_value(engine.handle(Request::MemoryReport, None).await.unwrap()).unwrap()
}

/// Server frames are unmasked: (opcode, payload) of the next whole frame.
fn next_frame(buf: &mut Vec<u8>) -> Option<(u8, Vec<u8>)> {
    if buf.len() < 2 {
        return None;
    }
    let op = buf[0] & 0x0f;
    let (len, head) = match buf[1] & 0x7f {
        126 if buf.len() >= 4 => (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4),
        127 if buf.len() >= 10 => (
            u64::from_be_bytes(buf[2..10].try_into().unwrap()) as usize,
            10,
        ),
        126 | 127 => return None,
        n => (n as usize, 2),
    };
    if buf.len() < head + len {
        return None;
    }
    let payload = buf[head..head + len].to_vec();
    buf.drain(..head + len);
    Some((op, payload))
}

#[tokio::test]
async fn a_device_that_stops_reading_misses_events_and_resyncs() {
    let dir = tempfile::tempdir().unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    let (_, token) = engine.store().add_device("sleepy phone").unwrap();

    // A plain local connection with a tiny receive buffer that then reads
    // nothing: the server's socket fills almost at once.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut tcp = socket.connect(addr).await.unwrap();
    tcp.write_all(
        format!(
            "GET /api/ws?token={token} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut buf = Vec::new();
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let mut chunk = [0u8; 512];
        let n = tcp.read(&mut chunk).await.unwrap();
        assert!(n > 0, "the upgrade was refused");
        buf.extend_from_slice(&chunk[..n]);
    }
    assert!(
        buf.starts_with(b"HTTP/1.1 101"),
        "{}",
        String::from_utf8_lossy(&buf)
    );
    let head = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    buf.drain(..head);

    // About 4 MB of events (each bot carries 400 KB of instructions).
    let big = "0123456789abcdef".repeat(25_000);
    let mut first = String::new();
    for n in 0..10 {
        let saved: Bot = serde_json::from_value(
            engine
                .handle(
                    Request::SaveBot {
                        bot: bot(n, big.clone()),
                    },
                    None,
                )
                .await
                .unwrap(),
        )
        .unwrap();
        if n == 0 {
            first = saved.id;
        }
    }
    let conn = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let r = report(&engine).await;
            if let Some(c) = r
                .connections
                .iter()
                .find(|c| c.device == "sleepy phone" && c.resyncs > 0)
            {
                return (c.clone(), r.event_queue);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the slow device falls behind");
    let (c, queue) = conn;
    // What waits for it stays near the limit (1 MB and one event over),
    // and the shared queue doesn't keep events for it.
    assert!(c.queued_bytes < 1_500_000, "{c:?}");
    assert_eq!(queue.waiting, 0, "{queue:?}");

    // It reads again: what was queued arrives, then a resync.
    let found = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Some((op, payload)) = next_frame(&mut buf) {
                if op == 1 && String::from_utf8_lossy(&payload).contains("\"type\":\"resync\"") {
                    return true;
                }
            }
            let mut chunk = vec![0u8; 1 << 16];
            let n = tcp.read(&mut chunk).await.unwrap();
            if n == 0 {
                return false;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("frames keep coming");
    assert!(found, "the device is told to resync");
    let drained = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = report(&engine).await;
            if r.connections
                .iter()
                .any(|c| c.device == "sleepy phone" && c.queued_bytes == 0)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(drained.is_ok(), "nothing stays queued once it was read");
    // A later event reaches it as usual.
    engine
        .handle(Request::DeleteBot { id: first }, None)
        .await
        .unwrap();
    let removed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Some((op, payload)) = next_frame(&mut buf) {
                if op == 1 && String::from_utf8_lossy(&payload).contains("\"type\":\"bot_removed\"")
                {
                    return;
                }
            }
            let mut chunk = vec![0u8; 1 << 16];
            let n = tcp.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await;
    assert!(removed.is_ok());
}
