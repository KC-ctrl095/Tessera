//! Last-handle ownership, tested against throwaway loopback servers.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn headers(socket: &mut TcpStream) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
        assert!(header.len() < 8192);
    }
}

async fn disconnected(mut status: watch::Receiver<ConnectionStatus>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while status.borrow().state != ConnState::Disconnected {
            status.changed().await.unwrap();
        }
        // The final status is followed by destruction of the manager sender.
        assert!(status.changed().await.is_err());
    })
    .await
    .unwrap();
}

async fn live_server(
    machine: bool,
) -> (
    ServerConfig,
    oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ServerConfig {
        endpoints: vec![format!("http://{}", listener.local_addr().unwrap())],
        ..Default::default()
    };
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut health, _) = listener.accept().await.unwrap();
        headers(&mut health).await;
        let body = r#"{"name":"tessera"}"#;
        health
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(health);
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        let hello: ClientMessage =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(hello.id, 0);
        let request: ClientMessage =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if machine {
            assert!(matches!(request.request, Request::MachineLogin));
        } else {
            assert!(matches!(request.request, Request::ListJobs));
            ws.send(tungstenite::Message::Text(
                serde_json::to_string(&ServerMessage::Response {
                    id: request.id,
                    result: serde_json::json!("clone still works"),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        }
        request_tx.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .is_none_or(|m| m.is_err()),
            "last handle closes the actual socket"
        );
    });
    (config, request_rx, server)
}

#[tokio::test]
async fn dropping_a_clone_keeps_the_connection_but_the_last_handle_ends_the_manager() {
    let (config, request, server) = live_server(false).await;
    let client = Client::connect(config);
    let status = client.status.clone();
    let stop = client.stop.clone();
    let outbox = client.outbound.clone(); // manager-owned resources do NOT own its lifetime
    let waiters = client.waiters.clone();
    client.wait_connected(Duration::from_secs(2)).await.unwrap();
    drop(client.clone());
    assert!(!stop.stopped());
    assert_eq!(
        client
            .call_timeout(Request::ListJobs, Duration::from_secs(2))
            .await
            .unwrap(),
        serde_json::json!("clone still works")
    );
    request.await.unwrap();
    drop(client);
    assert!(stop.stopped());
    assert!(outbox.recv().await.is_none());
    disconnected(status).await;
    assert!(waiters.lock().is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn last_handle_drop_cancels_a_connecting_manager() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ServerConfig {
        endpoints: vec![format!("http://{}", listener.local_addr().unwrap())],
        ..Default::default()
    };
    let (started_tx, started_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        headers(&mut socket).await;
        started_tx.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0,
            "cancelled connector releases its socket"
        );
    });
    let client = Client::connect(config);
    let status = client.status.clone();
    let outbox = client.outbound.clone();
    started_rx.await.unwrap();
    assert_eq!(client.status().state, ConnState::Connecting);
    drop(client.clone());
    client.post(Request::ListJobs).unwrap();
    drop(client);
    assert!(outbox.recv().await.is_none());
    disconnected(status).await;
    server.await.unwrap();
}

#[tokio::test]
async fn cancelling_machine_login_drops_its_only_client_and_closes_the_socket() {
    let (config, request, server) = live_server(true).await;
    let login = tokio::spawn(async move { machine_login(&config).await });
    tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap();
    login.abort();
    assert!(login.await.unwrap_err().is_cancelled());
    server.await.unwrap();
}
