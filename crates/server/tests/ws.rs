//! Server + client over real sockets: pairing, auth, requests and events.

use std::collections::BTreeMap;
use std::time::Duration;

use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    // A permanent fixture rejection ends turns immediately, unlike a closed
    // port which correctly exercises the provider's unattended retry budget.
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model.local_addr().unwrap();
    tokio::spawn(async move {
        let mock = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    "fixture model rejection",
                )
            }),
        );
        axum::serve(model, mock).await.unwrap();
    });
    std::fs::write(
        dir.path().join("config.toml"),
        format!("default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://{model_addr}/v1\"\n[[provider.models]]\nid = \"m\"\n"),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    (engine, format!("http://{addr}"), dir)
}

fn bot(instructions: String) -> Bot {
    serde_json::from_value(serde_json::json!({
        "id": "", "name": "Big", "handle": "big", "avatar": "B", "color": "#336699", "role": "r", "personality": "p",
        "instructions": instructions, "permission": "default", "tools": "read_only"
    }))
    .unwrap()
}

#[tokio::test]
async fn encrypted_pair_connect_call_and_receive_events() {
    let (engine, url, _dir) = start().await;
    let headers = BTreeMap::from([("CF-Access-Client-Id".to_string(), "abc".to_string())]);

    // Unreachable first endpoint, then the real one: the client falls through.
    let endpoints = vec!["http://127.0.0.1:1".to_string(), url.clone()];
    let err = tessera_client::pair(&endpoints, &headers, "0000-0000", "phone")
        .await
        .unwrap_err()
        .to_string();
    // Whether a code is active isn't given away: it reads as a wrong code.
    assert!(err.contains("wrong pairing code"), "{err}");
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (used, grant) = tessera_client::pair(&endpoints, &headers, &code, "Pixel Fold")
        .await
        .unwrap();
    assert_eq!(used, url);
    let identity = tessera_server::load_identity(engine.paths()).unwrap();
    assert_eq!(
        tessera_secure::unb64(&grant.server_key).unwrap(),
        identity.public(),
        "the real server key is pinned"
    );
    assert!(
        tessera_client::pair(&endpoints, &headers, &code, "again")
            .await
            .is_err(),
        "codes are single use"
    );

    let config = ServerConfig {
        endpoints: endpoints.clone(),
        headers: headers.clone(),
        token: grant.token.clone(),
        server_key: grant.server_key.clone(),
    };
    let client = Client::connect(config.clone());
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    let status = client.status();
    assert_eq!(status.endpoint.as_deref(), Some(url.as_str()));
    assert!(status.encrypted, "paired devices are end-to-end encrypted");
    let mut events = client.subscribe();
    // Counted while connected (a whiteboard `look` waits for every client).
    let clients = |n: usize| {
        let engine = engine.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                while engine.connected_clients() != n {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .is_ok()
        }
    };
    assert!(clients(1).await);

    let info: SessionInfo = client
        .call_as(Request::CreateSession {
            cwd: Some(std::env::temp_dir().display().to_string()),
            model: None,
            title: Some("remote".into()),
            bot_id: None,
            private: false,
            lean: false,
        })
        .await
        .unwrap();
    assert_eq!(info.title, "remote");
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(ClientEvent::Event(Event::SessionUpdated { session })) = events.recv().await
                && session.id == info.id
            {
                return session;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(got.id, info.id);

    // Messages far bigger than one Noise frame survive both directions.
    let big = "0123456789abcdef".repeat(20_000);
    let saved: Bot = client
        .call_as(Request::SaveBot {
            bot: bot(big.clone()),
        })
        .await
        .unwrap();
    assert_eq!(saved.instructions.len(), big.len());
    let bots: Vec<Bot> = client.call_as(Request::ListBots).await.unwrap();
    assert!(bots.iter().any(|b| b.instructions == big));

    let devices: Vec<DeviceInfo> = client.call_as(Request::ListDevices).await.unwrap();
    assert!(devices.iter().any(|d| d.name == "Pixel Fold" && d.current));

    // API errors come back typed.
    let err = client
        .call(Request::GetSession {
            session_id: "nope".into(),
            since_seq: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    client.close();
    assert!(clients(0).await, "a closed connection no longer counts");

    // A different pinned key (an impostor, or a proxy swapping keys) fails
    // closed instead of falling back to plaintext.
    let other = tessera_secure::Identity::generate().unwrap();
    let wrong = Client::connect(ServerConfig {
        server_key: tessera_secure::b64(other.public()),
        ..config.clone()
    });
    assert!(wrong.wait_connected(Duration::from_secs(3)).await.is_err());
    assert!(
        wrong
            .status()
            .error
            .unwrap_or_default()
            .contains("prove its identity")
    );
    wrong.close();

    // Revoked tokens are refused on the next connection.
    engine.store().revoke_device(&grant.device_id).unwrap();
    let again = Client::connect(config);
    assert!(again.wait_connected(Duration::from_secs(3)).await.is_err());
    assert!(
        again
            .status()
            .error
            .unwrap_or_default()
            .contains("pair again")
    );
    again.close();
}

#[tokio::test]
async fn a_stranger_at_the_home_address_doesnt_keep_the_client_from_the_server() {
    // On another network the home address (tried first) can be anyone's:
    // here another Tessera server, which can't prove the pinned key.
    let (engine, url, _dir) = start().await;
    let (_stranger, stranger_url, _dir2) = start().await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) =
        tessera_client::pair(std::slice::from_ref(&url), &BTreeMap::new(), &code, "phone")
            .await
            .unwrap();
    let client = Client::connect(ServerConfig {
        endpoints: vec![stranger_url.clone(), url.clone()],
        headers: BTreeMap::new(),
        token: grant.token.clone(),
        server_key: grant.server_key.clone(),
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(client.status().endpoint.as_deref(), Some(url.as_str()));
    let _: Vec<SessionInfo> = client
        .call_as(Request::ListSessions { archived: false })
        .await
        .unwrap();
    client.close();
    // With only the stranger to talk to, it says why and doesn't connect.
    let alone = Client::connect(ServerConfig {
        endpoints: vec![stranger_url],
        headers: BTreeMap::new(),
        token: grant.token,
        server_key: grant.server_key,
    });
    assert!(alone.wait_connected(Duration::from_secs(3)).await.is_err());
    assert!(
        alone
            .status()
            .error
            .unwrap_or_default()
            .contains("prove its identity")
    );
    alone.close();
}

#[tokio::test]
async fn wrong_codes_cancel_the_code() {
    let (engine, url, _dir) = start().await;
    let endpoints = vec![url];
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let digits: String = code.chars().filter(char::is_ascii_digit).collect();
    let wrong = format!("{:08}", (digits.parse::<u32>().unwrap() + 1) % 100_000_000);
    for i in 0..3 {
        let err = tessera_client::pair(&endpoints, &BTreeMap::new(), &wrong, "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("wrong pairing code"), "{err}");
        if i == 2 {
            assert!(err.contains("cancelled"), "{err}");
        }
    }
    // Even the right code no longer works: an attacker gets 3 guesses in 10^8.
    assert!(
        tessera_client::pair(&endpoints, &BTreeMap::new(), &code, "x")
            .await
            .is_err()
    );
    assert!(engine.store().active_pairing_code().unwrap().is_none());
}

#[tokio::test]
async fn strangers_cant_lock_the_owner_out_of_pairing_at_the_server() {
    let (engine, url, _dir) = start().await;
    let http = reqwest::Client::new();
    // Encrypted pairing through the tunnel (cloudflared on this computer
    // names the client), with guesses.
    let through_tunnel = |guess: &'static str| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            let (pake, msg) = tessera_secure::pairing::DevicePake::start(guess);
            let r = http
                .post(format!("{url}/api/pair/start"))
                .header("cf-connecting-ip", "203.0.113.9")
                .json(&serde_json::json!({ "pake": tessera_secure::b64(&msg) }))
                .send()
                .await
                .unwrap();
            if r.status() != 200 {
                return r.status().as_u16();
            }
            let started: tessera_proto::e2e::PairStarted = r.json().await.unwrap();
            let keys = pake
                .finish(&tessera_secure::unb64(&started.pake).unwrap())
                .unwrap();
            let sealed = keys.seal_to_server(br#"{"device_name":"x"}"#);
            http.post(format!("{url}/api/pair/finish"))
                .header("cf-connecting-ip", "203.0.113.9")
                .json(&serde_json::json!({
                    "pairing": started.pairing,
                    "sealed": tessera_secure::b64(&sealed),
                }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    // No code is active, yet the first step answers as if one were.
    let mut statuses = Vec::new();
    for _ in 0..12 {
        statuses.push(through_tunnel("1234-5678").await);
    }
    assert_eq!(&statuses[..10], &[401; 10], "{statuses:?}");
    assert_eq!(statuses[11], 429, "the network's tries are used up");
    // On the server itself pairing still works, plain and encrypted.
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let r = http
        .post(format!("{url}/api/pair"))
        .json(&serde_json::json!({"code": code, "device_name": "browser"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    tessera_client::pair(std::slice::from_ref(&url), &BTreeMap::new(), &code, "app")
        .await
        .unwrap();
    // The network still waits.
    let (_code, _) = engine.store().create_pairing_code(60_000).unwrap();
    assert_eq!(through_tunnel("1234-5678").await, 429);
}

#[tokio::test]
async fn plain_access_is_local_only() {
    let (engine, url, _dir) = start().await;
    let http = reqwest::Client::new();
    let upgrade = |rb: reqwest::RequestBuilder| {
        rb.header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
    };
    // Directly from this machine the browser path works as before…
    let r = upgrade(http.get(format!("{url}/api/ws")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "no token");
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let paired: PairResponse = http
        .post(format!("{url}/api/pair"))
        .json(&serde_json::json!({"code": code, "device_name": "browser"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let local = Client::connect(ServerConfig {
        endpoints: vec![url.clone()],
        token: paired.token.clone(),
        ..Default::default()
    });
    local.wait_connected(Duration::from_secs(5)).await.unwrap();
    assert!(!local.status().encrypted);
    local.close();

    // …but anything that came through a proxy or tunnel is refused.
    for (name, value) in [
        ("cf-connecting-ip", "203.0.113.9"),
        ("x-forwarded-for", "203.0.113.9"),
        ("origin", "https://tessera.example.com"),
        ("host", "tessera.example.com"),
    ] {
        let r = upgrade(http.get(format!("{url}/api/ws?token={}", paired.token)))
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "plain socket via {name}");
        let r = http
            .post(format!("{url}/api/pair"))
            .header(name, value)
            .json(&serde_json::json!({"code": "1234-5678", "device_name": "x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "plain pairing via {name}");
    }
    // (Clients send proxy headers only to https addresses, and a token
    // without a pinned key only to this computer: tessera-client's tests.)
}

#[tokio::test]
async fn unauthenticated_socket_is_refused_and_pairing_is_rate_limited() {
    let (_engine, url, _dir) = start().await;
    let http = reqwest::Client::new();
    let r = http
        .get(format!("{url}/api/ws"))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let health: serde_json::Value = http
        .get(format!("{url}/api/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["name"], "tessera");
    assert_eq!(health["e2ee"], true);
    assert!(
        health.get("server_name").is_none(),
        "health reveals nothing about the owner"
    );
    let mut last = 0;
    for _ in 0..12 {
        last = http
            .post(format!("{url}/api/pair"))
            .json(&serde_json::json!({"code": "1234-5678", "device_name": "x"}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
    }
    assert_eq!(last, 429, "brute force is throttled");
}

/// A TCP relay that records every byte in both directions: what a tunnel or
/// TLS-terminating proxy (e.g. Cloudflare) would see.
async fn recording_relay(target: String) -> (String, std::sync::Arc<parking_lot::Mutex<Vec<u8>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = target.trim_start_matches("http://").to_string();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            let upstream = upstream.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let outbound = tokio::net::TcpStream::connect(upstream).await.unwrap();
                let (mut ri, mut wi) = inbound.into_split();
                let (mut ro, mut wo) = outbound.into_split();
                let l1 = log.clone();
                let up = tokio::spawn(async move {
                    let mut buf = vec![0u8; 16384];
                    while let Ok(n) = ri.read(&mut buf).await {
                        if n == 0 || wo.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        l1.lock().extend_from_slice(&buf[..n]);
                    }
                });
                let mut buf = vec![0u8; 16384];
                while let Ok(n) = ro.read(&mut buf).await {
                    if n == 0 || wi.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    log.lock().extend_from_slice(&buf[..n]);
                }
                up.abort();
            });
        }
    });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn a_proxy_that_doesnt_say_who_connects_is_noticed() {
    let (_engine, url, _dir) = start().await;
    // A reverse proxy on the server's computer that adds no forwarding
    // headers: everyone coming through it looks local.
    let (relay, _seen) = recording_relay(url.clone()).await;
    assert_eq!(
        tessera_client::seen_as_local(&relay, &BTreeMap::new()).await,
        Some(true)
    );
    // One that does (cloudflared adds Cf-Connecting-Ip) is told apart.
    let health: serde_json::Value = reqwest::Client::new()
        .get(format!("{url}/api/health"))
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["local"], false);
    assert_eq!(
        tessera_client::seen_as_local("http://127.0.0.1:1", &BTreeMap::new()).await,
        None,
        "unreachable"
    );
}

#[tokio::test]
async fn a_proxy_in_the_middle_sees_only_ciphertext() {
    let (engine, url, _dir) = start().await;
    let (relay, seen) = recording_relay(url).await;
    // Plain http here stands in for a proxy that has already stripped TLS.
    let endpoints = vec![relay];
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair(&endpoints, &BTreeMap::new(), &code, "Secret Phone Name")
        .await
        .unwrap();
    let client = Client::connect(ServerConfig {
        endpoints,
        headers: BTreeMap::new(),
        token: grant.token.clone(),
        server_key: grant.server_key.clone(),
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    let info: SessionInfo = client
        .call_as(Request::CreateSession {
            cwd: None,
            model: None,
            title: Some("launch codes for the moon base".into()),
            bot_id: None,
            private: false,
            lean: false,
        })
        .await
        .unwrap();
    let _: Vec<SessionInfo> = client
        .call_as(Request::ListSessions { archived: false })
        .await
        .unwrap();
    client.close();

    let seen = String::from_utf8_lossy(&seen.lock()).to_string();
    assert!(
        seen.contains("/api/secure"),
        "the relay did carry the traffic"
    );
    let digits: String = code.chars().filter(char::is_ascii_digit).collect();
    for secret in [
        grant.token.as_str(),
        &grant.server_key,
        "Secret Phone Name",
        "launch codes",
        &info.id,
        &digits,
        &code,
        "list_sessions",
        "create_session",
    ] {
        assert!(!seen.contains(secret), "the proxy saw `{secret}`");
    }

    // Control: the same relay does see a plain (browser-style) connection, so
    // the check above is meaningful. (Allowed here only because the relay is
    // an unmarked loopback hop.)
    let (engine2, url2, _dir2) = start().await;
    let (relay2, seen2) = recording_relay(url2.clone()).await;
    let (code2, _) = engine2.store().create_pairing_code(60_000).unwrap();
    let paired: PairResponse = reqwest::Client::new()
        .post(format!("{url2}/api/pair"))
        .json(&serde_json::json!({"code": code2, "device_name": "b"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plain = Client::connect(ServerConfig {
        endpoints: vec![relay2],
        token: paired.token.clone(),
        ..Default::default()
    });
    plain.wait_connected(Duration::from_secs(5)).await.unwrap();
    let _: SessionInfo = plain
        .call_as(Request::CreateSession {
            cwd: None,
            model: None,
            title: Some("visible title".into()),
            bot_id: None,
            private: false,
            lean: false,
        })
        .await
        .unwrap();
    plain.close();
    let seen2 = String::from_utf8_lossy(&seen2.lock()).to_string();
    assert!(seen2.contains("visible title") && seen2.contains(&paired.token));
}

#[tokio::test]
async fn responses_carry_security_headers_and_the_ui_a_strict_csp() {
    let (engine, _url, dir) = start().await;
    let web = dir.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    let inline = "document.documentElement.dataset.theme = 'dark';";
    std::fs::write(
        web.join("index.html"),
        format!("<!doctype html><script>{inline}</script><script type=\"module\" src=\"/app.js\"></script>"),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = tessera_server::router(engine, Some(web)).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });

    let page = reqwest::get(format!("{url}/")).await.unwrap();
    let h = page.headers();
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["referrer-policy"], "no-referrer");
    let csp = h["content-security-policy"].to_str().unwrap().to_string();
    use sha2::Digest as _;
    let hash = tessera_secure::b64(&sha2::Sha256::digest(inline.as_bytes()));
    assert!(csp.contains(&format!("'sha256-{hash}'")), "{csp}");
    assert!(csp.contains("img-src 'self' data: blob:;"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    // Frames only from this server (its board and preview pages): a board
    // can't take itself to another page on the network.
    assert!(csp.contains("frame-src 'self';"), "{csp}");
    assert!(!csp.contains("script-src 'self' 'unsafe-inline'"), "{csp}");
    let api = reqwest::get(format!("{url}/api/health")).await.unwrap();
    assert_eq!(api.headers()["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn unauthenticated_handshakes_are_capped() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_engine, url, _dir) = start().await;
    let addr = url.trim_start_matches("http://").to_string();
    // Through a tunnel on this computer: the client it names is who counts.
    let upgrade = |addr: String, from: String| async move {
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let req = format!(
            "GET /api/secure HTTP/1.1\r\nHost: {addr}\r\nCF-Connecting-IP: {from}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).await.unwrap();
        (String::from_utf8_lossy(&buf[..n]).to_string(), s)
    };
    // Sockets that upgrade and then never say anything hold a slot each, a
    // few per address…
    let mut held = Vec::new();
    for _ in 0..8 {
        let (status, s) = upgrade(addr.clone(), "203.0.113.1".into()).await;
        assert!(status.starts_with("HTTP/1.1 101"), "{status}");
        held.push(s);
    }
    let (status, _) = upgrade(addr.clone(), "203.0.113.1".into()).await;
    assert!(status.starts_with("HTTP/1.1 503"), "one address: {status}");
    // …and once they're all taken, more are turned away instead of piling up.
    for i in 2..9 {
        for _ in 0..8 {
            let (status, s) = upgrade(addr.clone(), format!("203.0.113.{i}")).await;
            assert!(status.starts_with("HTTP/1.1 101"), "{status}");
            held.push(s);
        }
    }
    let (status, _) = upgrade(addr.clone(), "198.51.100.7".into()).await;
    assert!(status.starts_with("HTTP/1.1 503"), "all: {status}");
    drop(held);
    // One that never says anything is closed after a few seconds.
    let (status, mut silent) = upgrade(addr.clone(), "198.51.100.8".into()).await;
    assert!(status.starts_with("HTTP/1.1 101"), "{status}");
    let started = std::time::Instant::now();
    let mut buf = [0u8; 256];
    let closed = tokio::time::timeout(Duration::from_secs(9), async {
        loop {
            match silent.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "still open after 9 s");
    assert!(started.elapsed() >= Duration::from_secs(4));
}

#[tokio::test]
async fn slow_headers_are_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_engine, url, _dir) = start().await;
    let addr = url.trim_start_matches("http://").to_string();
    // A request whose headers never end (one byte now and then would do the
    // same) is closed instead of holding the connection forever.
    let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
    s.write_all(format!("GET /api/health HTTP/1.1\r\nHost: {addr}\r\nX-Slow: ").as_bytes())
        .await
        .unwrap();
    let mut buf = [0u8; 256];
    let closed = tokio::time::timeout(Duration::from_secs(14), async {
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "a request with unfinished headers stayed open"
    );
    // Ordinary requests are untouched.
    let health = reqwest::get(format!("{url}/api/health")).await.unwrap();
    assert_eq!(health.status(), 200);
}

#[tokio::test]
async fn qr_links_pair_and_pin_the_right_key() {
    let (engine, url, _dir) = start().await;
    let header = || {
        BTreeMap::from([
            ("CF-Access-Client-Id".to_string(), "id.access".to_string()),
            (
                "CF-Access-Client-Secret".to_string(),
                "very-secret-value".to_string(),
            ),
        ])
    };
    // The owner records how devices reach the server; header values never come back.
    let info: RemoteAccessInfo = serde_json::from_value(
        engine
            .handle(
                Request::SetRemoteAccess {
                    urls: vec![url.clone()],
                    headers: Some(header()),
                    add_headers: None,
                    remove_headers: Vec::new(),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.urls, vec![url.clone()]);
    assert!(
        !serde_json::to_string(&info)
            .unwrap()
            .contains("very-secret-value")
    );

    let pc: PairingCode =
        serde_json::from_value(engine.handle(Request::CreatePairing, None).await.unwrap()).unwrap();
    let link = pc.link.clone().expect("a pairing link");
    let parsed = PairLink::parse(&link).unwrap();
    assert_eq!(parsed.urls, vec![url.clone()]);
    assert_eq!(parsed.code, pc.code);
    assert_eq!(parsed.fingerprint, pc.fingerprint);
    assert!(parsed.headers.contains(&(
        "CF-Access-Client-Secret".to_string(),
        "very-secret-value".to_string()
    )));

    // Clients already connected on this machine can learn the key to pin it.
    let key: ServerKeyInfo =
        serde_json::from_value(engine.handle(Request::ServerKey, None).await.unwrap()).unwrap();
    assert_eq!(Some(key.fingerprint.clone()), pc.fingerprint);
    assert_eq!(tessera_client::key_fingerprint(&key.key), pc.fingerprint);

    // A link whose fingerprint was swapped (e.g. a doctored QR) is refused.
    let forged = PairLink {
        fingerprint: Some("0000 0000 0000 0000 0000".into()),
        ..parsed.clone()
    }
    .to_link();
    let err = tessera_client::pair_with_link(&forged, "phone")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("doesn't match"), "{err}");

    // The genuine one pairs, end-to-end encrypted, with the pinned key matching.
    let pc: PairingCode =
        serde_json::from_value(engine.handle(Request::CreatePairing, None).await.unwrap()).unwrap();
    let (link, endpoint, grant) = tessera_client::pair_with_link(&pc.link.unwrap(), "Pixel")
        .await
        .unwrap();
    assert_eq!(endpoint, url);
    assert_eq!(
        tessera_client::key_fingerprint(&grant.server_key),
        link.fingerprint
    );
    let client = Client::connect(ServerConfig {
        endpoints: link.urls,
        headers: link.headers.into_iter().collect(),
        token: grant.token,
        server_key: grant.server_key,
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    assert!(client.status().encrypted);
    client.close();
}

#[tokio::test]
async fn note_previews_are_served_like_board_pages_on_this_machine_only() {
    let (engine, url, _dir) = start().await;
    let link = engine
        .handle(
            Request::PreviewPage {
                html: "<p id=x>preview</p>".into(),
            },
            None,
        )
        .await
        .unwrap();
    let page = format!("{url}{}", link["path"].as_str().unwrap());
    let r = reqwest::get(&page).await.unwrap();
    assert_eq!(r.status(), 200);
    let h = r.headers().clone();
    let csp = h["content-security-policy"].to_str().unwrap().to_string();
    assert!(csp.contains("connect-src 'none'"), "{csp}");
    // Opened on its own it still runs apart from the UI's origin.
    assert!(csp.contains("sandbox allow-scripts"), "{csp}");
    assert_eq!(h["x-frame-options"], "SAMEORIGIN");
    assert_eq!(h["cache-control"], "no-store");
    let body = r.text().await.unwrap();
    assert!(body.contains("<p id=x>preview</p>") && body.contains("Content-Security-Policy"));
    // An unknown token, or a request that came through a tunnel, gets nothing.
    let r = reqwest::get(format!("{url}/preview/nope/")).await.unwrap();
    assert_eq!(r.status(), 404);
    let r = reqwest::Client::new()
        .get(&page)
        .header("cf-connecting-ip", "203.0.113.9")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn whiteboard_page_grants_are_bounded_and_cannot_read_a_later_revision() {
    let (engine, url, _dir) = start().await;
    let session: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: None,
                    model: None,
                    title: Some("Board fixture".into()),
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
    let mut board = Whiteboard {
        id: "wb_grant_fixture".into(),
        session_id: session.id,
        title: "Fixture".into(),
        content: BoardContent::Html {
            html: "<p>first revision</p>".into(),
        },
        version: 1,
        created_at: 0,
        updated_at: 0,
        ask: None,
        by_user: true,
        host: None,
    };
    engine.store().put_board(&board).unwrap();
    let link = engine
        .handle(
            Request::BoardLink {
                id: board.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let path = link["path"].as_str().unwrap();
    let token = path.trim_end_matches('/').rsplit('/').next().unwrap();
    let grant: Vec<_> = token.split('.').collect();
    assert_eq!(&grant[..2], ["v1", "1"]);
    let expires: u64 = grant[2].parse().unwrap();
    let now = tessera_core::util::now_ms();
    assert!(expires > now && expires <= now + 86_400_000);
    let page = format!("{url}{path}");
    let response = reqwest::get(&page).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.text().await.unwrap().contains("first revision"));
    let expired = path.replace(grant[2], "0");
    assert_eq!(
        reqwest::get(format!("{url}{expired}"))
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        reqwest::Client::new()
            .get(&page)
            .header("cf-connecting-ip", "203.0.113.9")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    board.version += 1;
    board.content = BoardContent::Html {
        html: "<p>later revision</p>".into(),
    };
    engine.store().put_board(&board).unwrap();
    let stale = reqwest::get(&page).await.unwrap();
    assert_eq!(stale.status(), 404);
    assert!(!stale.text().await.unwrap().contains("later revision"));
    let fresh = engine
        .handle(
            Request::BoardLink {
                id: board.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let response = reqwest::get(format!("{url}{}", fresh["path"].as_str().unwrap()))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("later revision"));
    engine.store().delete_board(&board.id).unwrap();
    assert_eq!(
        reqwest::get(format!("{url}{}", fresh["path"].as_str().unwrap()))
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn plain_maximum_content_round_trip_and_session_fetch() {
    maximum_content_round_trip(false).await;
}

#[tokio::test]
async fn noise_maximum_content_round_trip_and_session_fetch() {
    maximum_content_round_trip(true).await;
}

async fn maximum_content_round_trip(encrypted: bool) {
    tokio::time::timeout(Duration::from_secs(240), async {
        let (engine, url, _dir) = start().await;
        let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
        let (_, grant) = tessera_client::pair(std::slice::from_ref(&url), &BTreeMap::new(), &code, "maximum-content-fixture").await.unwrap();
        let client = Client::connect(ServerConfig {
            endpoints: vec![url], headers: BTreeMap::new(), token: grant.token,
            server_key: if encrypted { grant.server_key } else { String::new() },
        });
        client.wait_connected(Duration::from_secs(10)).await.unwrap();
        let info: SessionInfo = client.call_as(Request::CreateSession {
            cwd: Some(std::env::temp_dir().display().to_string()), model: None,
            title: Some("maximum content".into()), bot_id: None, private: false, lean: false,
        }).await.unwrap();
        let make = |text: String| Request::Send { session_id: info.id.clone(),
            content: vec![ContentPart::text(text)], delivery: Delivery::Auto, attachments: vec![] };
        // First create_session is id 1, then sends are ids 2 and 3. Both have
        // the same envelope length. Exercise the actual serialized ceiling.
        let overhead = serde_json::to_vec(&ClientMessage { id: 2, request: make(String::new()) }).unwrap().len();
        let content_len = MAX_SERIALIZED_REQUEST_BYTES - overhead;
        let mut events = client.subscribe();
        for _ in 0..2 {
            let sent = client.call_timeout(make("x".repeat(content_len)), Duration::from_secs(120));
            let echo = async {
                loop {
                    match events.recv().await.unwrap() {
                        ClientEvent::Event(Event::EntryAdded { session_id, entry }) if session_id == info.id => {
                            if let EntryBody::User { content, .. } = entry.body {
                                assert!(matches!(&content[0], ContentPart::Text { text } if text.len() == content_len));
                                break;
                            }
                        }
                        ClientEvent::Status(status) if status.state != tessera_client::ConnState::Connected => panic!("admitted content disconnected the receiver: {:?}", status.error),
                        _ => {}
                    }
                }
            };
            let (result, ()) = tokio::join!(sent, echo);
            result.unwrap();
        }
        let value = client.call_timeout(Request::GetSession { session_id: info.id.clone(), since_seq: None }, Duration::from_secs(120)).await.unwrap();
        assert!(serde_json::to_vec(&ServerMessage::Response { id: 4, result: value.clone() }).unwrap().len() <= MAX_SERIALIZED_RESPONSE_BYTES);
        let snapshot: SessionSnapshot = serde_json::from_value(value).unwrap();
        assert!(snapshot.entries.iter().any(|entry| matches!(&entry.body, EntryBody::Notice { text, .. } if text.contains("too large to show here"))));
        let full = snapshot.entries.iter().filter(|entry| matches!(&entry.body, EntryBody::User { content, .. } if matches!(&content[0], ContentPart::Text { text } if text.len() == content_len))).count();
        assert_eq!(full, 1, "snapshot stubs the older body but keeps the latest maximum entry complete");
        let persisted = engine.store().entries(&info.id, 0).unwrap();
        assert_eq!(persisted.iter().filter(|entry| matches!(&entry.body, EntryBody::User { content, .. } if matches!(&content[0], ContentPart::Text { text } if text.len() == content_len))).count(), 2, "display stubs never rewrite stored history");
        client.call_timeout(Request::ListJobs, Duration::from_secs(10)).await.unwrap();
        client.close();
    }).await.unwrap();
}

#[tokio::test]
async fn plain_private_escaped_ten_files_are_refused_before_append() {
    private_escaped_ten_files_are_refused(false).await;
}

#[tokio::test]
async fn noise_private_escaped_ten_files_are_refused_before_append() {
    private_escaped_ten_files_are_refused(true).await;
}

async fn private_escaped_ten_files_are_refused(encrypted: bool) {
    tokio::time::timeout(Duration::from_secs(240), async {
        let (engine, url, _dir) = start().await;
        let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
        let (_, grant) = tessera_client::pair(std::slice::from_ref(&url), &BTreeMap::new(), &code, "private-expanded-entry-fixture").await.unwrap();
        let client = Client::connect(ServerConfig {
            endpoints: vec![url], headers: BTreeMap::new(), token: grant.token,
            server_key: if encrypted { grant.server_key } else { String::new() },
        });
        client.wait_connected(Duration::from_secs(10)).await.unwrap();
        let info: SessionInfo = client.call_as(Request::CreateSession {
            cwd: Some(std::env::temp_dir().display().to_string()), model: None,
            title: Some("private expanded entry".into()), bot_id: None, private: true, lean: false,
        }).await.unwrap();
        let attachments: Vec<_> = (0..10).map(|i| Attachment {
            name: format!("escaped-{i}.txt"), mime: "text/plain".into(),
            data: tessera_secure::b64(&vec![1; 256 * 1024]),
        }).collect();
        let make = |text: String| Request::Send { session_id: info.id.clone(),
            content: vec![ContentPart::text(text)], delivery: Delivery::Auto, attachments: attachments.clone() };
        // Each file is within the private inline cap, but U+0001 expands to
        // six JSON bytes after base64 decoding. Fill the admitted request to
        // its exact ceiling so transformed entry + files exceeds reception.
        let overhead = serde_json::to_vec(&ClientMessage { id: 2, request: make(String::new()) }).unwrap().len();
        let text_len = MAX_SERIALIZED_REQUEST_BYTES - overhead;
        assert!(text_len + 10 * 256 * 1024 * 6 > MAX_SERIALIZED_RESPONSE_BYTES);
        let mut events = client.subscribe();
        let error = client.call_timeout(make("x".repeat(text_len)), Duration::from_secs(120)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert_eq!(error.message, "These files are too large to include in a private chat message; send fewer or smaller files.");
        assert!(engine.store().entries(&info.id, 0).unwrap().is_empty(), "refusal appends nothing, not even a truncated entry");
        let snapshot: SessionSnapshot = client.call_as(Request::GetSession { session_id: info.id.clone(), since_seq: None }).await.unwrap();
        assert!(snapshot.entries.is_empty());
        assert!(snapshot.queued.is_empty());
        assert_eq!(snapshot.info.last_seq, 0);
        assert!(!engine.paths().uploads_dir().join(&info.id).exists(), "private refusal never saves these files");
        client.call_timeout(Request::ListJobs, Duration::from_secs(10)).await.unwrap();
        assert_eq!(client.status().state, tessera_client::ConnState::Connected);
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, ClientEvent::Event(Event::EntryAdded { session_id, .. }) if session_id == info.id), "refusal emits no appended entry");
        }
        client.close();
    }).await.unwrap();
}
