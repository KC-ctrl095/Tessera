//! Remote access keeps up with the user's devices: apps learn the server's
//! addresses and Access key over their encrypted connection when they
//! change (no pairing again), a device that pairs again replaces its old
//! entry, and the year-long Cloudflare key is renewed in time.

use std::collections::BTreeMap;
use std::time::Duration;

use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    (engine, format!("http://{addr}"), dir)
}

/// Pair a device with a fresh code and connect it, end-to-end encrypted.
async fn paired(engine: &tessera_core::Engine, url: &str, name: &str) -> (Client, String, String) {
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair(&[url.to_string()], &BTreeMap::new(), &code, name)
        .await
        .unwrap();
    let client = Client::connect(ServerConfig {
        endpoints: vec![url.to_string()],
        headers: BTreeMap::new(),
        token: grant.token.clone(),
        server_key: grant.server_key,
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    (client, grant.token, grant.device_id)
}

fn set_remote(urls: &[&str], headers: &[(&str, &str)]) -> Request {
    Request::SetRemoteAccess {
        urls: urls.iter().map(|u| u.to_string()).collect(),
        headers: Some(
            headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        ),
        add_headers: None,
        remove_headers: vec![],
    }
}

/// Remote access set up after the phone paired: the phone is told, asks,
/// and gets the address and Access key over its encrypted connection; the
/// next connection uses them. Turned off: told again, and they're gone.
#[tokio::test]
async fn a_paired_app_learns_a_new_route_without_pairing_again() {
    let (engine, url, _dir) = start().await;
    let (client, _, _) = paired(&engine, &url, "Phone").await;
    let mut events = client.subscribe();
    let first = client.server_route().await.expect("a route");
    assert!(first.headers.is_empty(), "nothing remote yet: {first:?}");

    engine
        .handle(
            set_remote(
                &["https://tessera.example.com"],
                &[
                    ("CF-Access-Client-Id", "id.access"),
                    ("CF-Access-Client-Secret", "s3cret"),
                ],
            ),
            None,
        )
        .await
        .unwrap();
    let told = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(ClientEvent::Event(Event::ServerRouteChanged)) = events.recv().await {
                break;
            }
        }
    })
    .await;
    assert!(told.is_ok(), "apps are told the way in changed");
    let route = client.server_route().await.expect("a route");
    assert!(
        route
            .urls
            .contains(&"https://tessera.example.com".to_string())
    );
    assert_eq!(route.headers["CF-Access-Client-Secret"], "s3cret");

    // What the app saves (a phone paired at home: its home address).
    let home = vec!["http://192.168.1.10:7878".to_string()];
    let (endpoints, headers, known) =
        tessera_client::merge_route(&home, &BTreeMap::new(), None, &route).unwrap();
    assert_eq!(endpoints.first().unwrap(), "http://192.168.1.10:7878");
    assert!(endpoints.contains(&"https://tessera.example.com".to_string()));
    assert_eq!(headers["CF-Access-Client-Id"], "id.access");
    // The live client takes them for its next connection (reconnects fine).
    client.set_route(vec![url.clone()], headers.clone());

    // Turned off again: told, and the tunnel's address and key go.
    engine.handle(set_remote(&[], &[]), None).await.unwrap();
    let route = client.server_route().await.expect("a route");
    let (endpoints, headers, _) =
        tessera_client::merge_route(&endpoints, &headers, Some(&known), &route).unwrap();
    assert_eq!(endpoints, home);
    assert!(headers.is_empty());
    client.close();
}

/// Even if a revocation event is missed/not emitted, stale dispatch cannot
/// return route credentials or perform a mutation with the old login.
#[tokio::test]
async fn revoked_login_is_rechecked_before_sensitive_dispatch() {
    let (engine, url, _dir) = start().await;
    let (client, _, id) = paired(&engine, &url, "stale login").await;
    // Direct store change deliberately bypasses the broadcast.
    engine.store().revoke_device(&id).unwrap();
    assert!(client.server_route().await.is_none());
    assert!(
        client
            .call_timeout(
                Request::SetRemoteAccess {
                    urls: vec!["https://stale.example.com".into()],
                    headers: None,
                    add_headers: None,
                    remove_headers: vec![],
                },
                Duration::from_secs(1)
            )
            .await
            .is_err()
    );
    assert!(
        !engine
            .server_route()
            .urls
            .contains(&"https://stale.example.com".into())
    );
    client.close();
}

/// A fake route to the SAME throwaway server. Health works, but pairing
/// either has a broken start or loses the finished exchange's response.
async fn pairing_route(
    upstream: &str,
    mode: &'static str,
) -> (String, std::sync::Arc<parking_lot::Mutex<Vec<String>>>) {
    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    type S = (
        String,
        &'static str,
        std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
    );
    async fn start_pair(
        State((url, mode, seen)): State<S>,
        Json(req): Json<e2e::PairStart>,
    ) -> axum::response::Response {
        seen.lock().push(req.pake.clone());
        let response = reqwest::Client::new()
            .post(format!("{url}/api/pair/start"))
            .json(&req)
            .send()
            .await
            .unwrap();
        let mut body: serde_json::Value = response.json().await.unwrap();
        if mode == "broken start" {
            body["pake"] = serde_json::json!("invalid-base64!");
        }
        Json(body).into_response()
    }
    async fn finish_pair(
        State((url, mode, _)): State<S>,
        Json(req): Json<e2e::PairFinish>,
    ) -> axum::response::Response {
        let response = reqwest::Client::new()
            .post(format!("{url}/api/pair/finish"))
            .json(&req)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        if mode == "lost finish" {
            return StatusCode::GATEWAY_TIMEOUT.into_response();
        }
        (status, Json(body)).into_response()
    }
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({"name":"tessera"})) }),
        )
        .route("/api/pair/start", post(start_pair))
        .route("/api/pair/finish", post(finish_pair))
        .with_state((upstream.to_string(), mode, seen.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

#[tokio::test]
async fn pairing_falls_back_after_health_with_fresh_pake_and_pins_the_key() {
    let (engine, url, _dir) = start().await;
    let (broken, first) = pairing_route(&url, "broken start").await;
    let (working, second) = pairing_route(&url, "ok").await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let link = PairLink {
        name: "test".into(),
        urls: vec![broken, working.clone()],
        headers: vec![],
        code,
        fingerprint: Some(tessera_secure::fingerprint(
            tessera_server::load_identity(engine.paths())
                .unwrap()
                .public(),
        )),
    };
    let (_, endpoint, _) = tessera_client::pair_with_link(&link.to_link(), "fallback")
        .await
        .unwrap();
    assert_eq!(endpoint, working);
    assert_eq!(first.lock().len(), 1);
    assert_eq!(second.lock().len(), 1);
    assert_ne!(
        first.lock()[0],
        second.lock()[0],
        "a fresh PAKE at every address"
    );
}

#[tokio::test]
async fn pairing_never_retries_an_ambiguous_finished_exchange() {
    let (engine, url, _dir) = start().await;
    let (lost, first) = pairing_route(&url, "lost finish").await;
    let (working, second) = pairing_route(&url, "ok").await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let error = tessera_client::pair(&[lost, working], &BTreeMap::new(), &code, "lost finish")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no other address was retried"), "{error}");
    assert_eq!(first.lock().len(), 1);
    assert!(second.lock().is_empty());
    assert!(
        engine
            .store()
            .devices(None)
            .unwrap()
            .iter()
            .any(|d| d.name == "lost finish"),
        "the login was actually created"
    );
}

#[tokio::test]
async fn pairing_fallback_still_refuses_a_wrong_qr_fingerprint() {
    let (engine, url, _dir) = start().await;
    let (broken, _) = pairing_route(&url, "broken start").await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let wrong = tessera_secure::Identity::generate().unwrap();
    let link = PairLink {
        name: "test".into(),
        urls: vec![broken, url],
        headers: vec![],
        code,
        fingerprint: Some(tessera_secure::fingerprint(wrong.public())),
    };
    let error = tessera_client::pair_with_link(&link.to_link(), "wrong pin")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("doesn't match the QR code"), "{error}");
}

#[tokio::test]
async fn network_pairing_cannot_fill_the_local_owners_pending_slots() {
    let (engine, url, _dir) = start().await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let http = reqwest::Client::new();
    for i in 0..17 {
        let (_, msg) = tessera_secure::pairing::DevicePake::start("00000000");
        let response = http
            .post(format!("{url}/api/pair/start"))
            .header("x-forwarded-for", "192.0.2.123")
            .json(&e2e::PairStart {
                pake: tessera_secure::b64(&msg),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), if i < 16 { 200 } else { 429 });
    }
    let (_, grant) = tessera_client::pair(
        std::slice::from_ref(&url),
        &BTreeMap::new(),
        &code,
        "local owner",
    )
    .await
    .unwrap();
    assert!(
        engine
            .store()
            .devices(None)
            .unwrap()
            .iter()
            .any(|d| d.id == grant.device_id)
    );
    // Local capacity is bounded too, independently of the full network pool.
    for i in 0..17 {
        let (_, msg) = tessera_secure::pairing::DevicePake::start("00000000");
        let response = http
            .post(format!("{url}/api/pair/start"))
            .json(&e2e::PairStart {
                pake: tessera_secure::b64(&msg),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), if i < 16 { 200 } else { 429 });
    }
}

/// A phone that paired again: once connected it retires its old login, the
/// old entry goes from Devices, and the name the user gave it carries over.
/// Never a machine's login, nor the caller's own, nor one whose computer
/// serves as a machine under it.
#[tokio::test]
async fn pairing_again_replaces_the_old_entry() {
    let (engine, url, _dir) = start().await;
    let (old, old_token, old_id) = paired(&engine, &url, "Phone").await;
    engine
        .handle(
            Request::RenameDevice {
                id: old_id.clone(),
                name: "Alex's phone".into(),
            },
            None,
        )
        .await
        .unwrap();
    engine
        .handle(
            Request::SetDeviceNotify {
                id: old_id.clone(),
                notify: false,
            },
            None,
        )
        .await
        .unwrap();
    // A private chat the phone made: it stays with the phone.
    let private: SessionInfo = old
        .call_as(Request::CreateSession {
            cwd: None,
            // Ownership only, no inference or implicit first-run model.
            model: Some("test/unused".parse().unwrap()),
            title: None,
            bot_id: None,
            private: true,
            lean: false,
        })
        .await
        .unwrap();
    let (new, new_token, new_id) = paired(&engine, &url, "Phone").await;
    let own = new
        .call(Request::RetireLogin {
            token: new_token.clone(),
        })
        .await;
    assert!(own.is_err(), "not its own login");
    new.call(Request::RetireLogin {
        token: old_token.clone(),
    })
    .await
    .unwrap();
    let devices = engine.store().devices(None).unwrap();
    assert!(
        !devices.iter().any(|d| d.id == old_id),
        "the old entry is gone"
    );
    let me = devices.iter().find(|d| d.id == new_id).unwrap();
    assert_eq!(me.name, "Alex's phone");
    assert!(me.named && !me.notify, "its name and choice carried over");
    assert_eq!(
        engine.store().private_owner(&private.id),
        Some(Some(new_id.clone())),
        "its private chat stays with it"
    );
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while old.status().state == tessera_client::ConnState::Connected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "the old login's connection closes");
    old.close();
    // Gone already: nothing to do.
    new.call(Request::RetireLogin { token: old_token })
        .await
        .unwrap();

    // A machine's login is never retired this way.
    let grant: MachineLoginGrant = new.call_as(Request::MachineLogin).await.unwrap();
    assert!(
        new.call(Request::RetireLogin { token: grant.token })
            .await
            .is_err()
    );
    // A device that serves as a machine stays (its machine would stop).
    let (laptop, laptop_token, laptop_id) = paired(&engine, &url, "Laptop").await;
    let _: MachineLoginGrant = laptop.call_as(Request::MachineLogin).await.unwrap();
    new.call(Request::RetireLogin {
        token: laptop_token,
    })
    .await
    .unwrap();
    assert!(
        engine
            .store()
            .devices(None)
            .unwrap()
            .iter()
            .any(|d| d.id == laptop_id)
    );
    laptop.close();
    new.close();
}

fn tunnel(expires: Option<u64>) -> tessera_core::remote::TunnelRecord {
    tessera_core::remote::TunnelRecord {
        account_id: "acct1".into(),
        zone_id: "zone1".into(),
        hostname: "tessera.example.com".into(),
        tunnel_id: "tun1".into(),
        dns_record_id: "dns1".into(),
        access_app_id: "app1".into(),
        service_token_id: "st1".into(),
        client_id: "id.access".into(),
        client_secret: "s3cret".into(),
        run_token: "run".into(),
        web_app_id: String::new(),
        key_expires_at: expires,
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The devices' Cloudflare key lasts a year: Settings learns when it runs
/// out, the user's devices are told from a month before (each reminder
/// once), again once it has run out, and renewing moves the date on.
#[tokio::test]
async fn the_devices_key_is_renewed_in_time() {
    let (engine, _url, _dir) = start().await;
    let reminders = |engine: &tessera_core::Engine| -> Vec<String> {
        engine
            .store()
            .notifications(50)
            .unwrap()
            .into_iter()
            .filter(|n| n.title.starts_with("Remote access"))
            .map(|n| format!("{}: {}", n.title, n.body))
            .collect()
    };
    let day = 86_400;
    engine
        .auth()
        .set_tunnel(Some(tunnel(Some(now() + 100 * day))))
        .unwrap();
    engine.remind_remote_key();
    assert!(reminders(&engine).is_empty(), "months left: nothing yet");

    engine
        .auth()
        .set_tunnel(Some(tunnel(Some(now() + 20 * day))))
        .unwrap();
    let st: RemoteStatus =
        serde_json::from_value(engine.handle(Request::RemoteStatus, None).await.unwrap()).unwrap();
    assert!(st.tunnel.unwrap().key_expires_at.unwrap() > now());
    engine.remind_remote_key();
    engine.remind_remote_key();
    let r = reminders(&engine);
    assert_eq!(r.len(), 1, "once: {r:?}");
    assert!(
        r[0].contains("needs renewing") && r[0].contains("in 20 days"),
        "{r:?}"
    );
    assert!(!r[0].contains("s3cret"));

    // Run out: told once more.
    let expired = now() - day;
    engine
        .auth()
        .set_tunnel(Some(tunnel(Some(expired))))
        .unwrap();
    engine.remind_remote_key();
    let r = reminders(&engine);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(
        r.iter().any(|x| x.starts_with("Remote access stopped")),
        "{r:?}"
    );

    // Renewed in Cloudflare by hand: a year from now, and quiet again.
    let st: RemoteStatus = serde_json::from_value(
        engine
            .handle(Request::CloudflareRenew { token: None }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let until = st.tunnel.unwrap().key_expires_at.unwrap();
    assert!(until > now() + 360 * day, "{until}");
    engine.remind_remote_key();
    assert_eq!(reminders(&engine).len(), 2);
    // The devices keep the same key: the route didn't change.
    assert_eq!(engine.auth().tunnel().unwrap().client_secret, "s3cret");

    // Without remote access there is nothing to renew.
    engine.auth().set_tunnel(None).unwrap();
    assert!(
        engine
            .handle(Request::CloudflareRenew { token: None }, None)
            .await
            .is_err()
    );
}

/// A LAN URL may answer health on a different Wi-Fi network but never finish
/// its WebSocket or Noise handshake. All three stages must race the tunnel.
#[tokio::test]
async fn mobile_data_does_not_wait_for_a_lan_upgrade_or_login() {
    use axum::{Json, Router, extract::ws::WebSocketUpgrade, routing::get};
    let (engine, remote, _dir) = start().await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair(
        std::slice::from_ref(&remote),
        &BTreeMap::new(),
        &code,
        "Mobile",
    )
    .await
    .unwrap();
    for stage in ["health", "upgrade", "login"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let lan = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/api/health",
                get(move || async move {
                    if stage == "health" {
                        std::future::pending::<()>().await;
                    }
                    Json(serde_json::json!({"name": "tessera"}))
                }),
            )
            .route(
                "/api/secure",
                get(move |ws: WebSocketUpgrade| async move {
                    if stage == "upgrade" {
                        std::future::pending::<()>().await;
                    }
                    ws.on_upgrade(|socket| async move {
                        // Keep the upgraded socket open, but never answer Noise.
                        std::future::pending::<()>().await;
                        drop(socket);
                    })
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::connect(ServerConfig {
            endpoints: vec![lan, remote.clone()],
            headers: BTreeMap::new(),
            token: grant.token.clone(),
            server_key: grant.server_key.clone(),
        });
        let start = std::time::Instant::now();
        client
            .wait_connected(Duration::from_secs(2))
            .await
            .unwrap_or_else(|e| panic!("stalled LAN {stage}: {e}"));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{stage}: {:?}",
            start.elapsed()
        );
        assert_eq!(client.status().endpoint.as_deref(), Some(remote.as_str()));
        assert!(client.status().encrypted);
        client
            .call(Request::ListSessions { archived: false })
            .await
            .unwrap();
        client.close();
        server.abort();
    }
}

#[tokio::test]
async fn route_refresh_is_idempotent_and_keeps_the_encrypted_connection() {
    let (engine, url, _dir) = start().await;
    let (client, _, _) = paired(&engine, &url, "Phone route refresh").await;
    let mut events = client.subscribe();
    let mut endpoints = vec![url.clone(), "https://tessera.example.com".into()];
    let mut headers = BTreeMap::from([("CF-Access-Client-Secret".into(), "pasted".into())]);
    let mut known = None;
    engine
        .handle(
            set_remote(
                &["https://tessera.example.com"],
                &[("CF-Access-Client-Secret", "server")],
            ),
            None,
        )
        .await
        .unwrap();
    let mut applied = 0;
    for _ in 0..5 {
        let route = client.server_route().await.unwrap();
        if let Some((eps, hs, k)) =
            tessera_client::merge_route(&endpoints, &headers, known.as_ref(), &route)
        {
            endpoints = eps;
            headers = hs;
            known = Some(k);
            applied += 1;
            client.set_route(endpoints.clone(), headers.clone());
        }
        client
            .call(Request::ListSessions { archived: false })
            .await
            .unwrap();
    }
    assert_eq!(applied, 1, "the same answer is only applied once");
    assert_eq!(headers["CF-Access-Client-Secret"], "pasted");
    assert_eq!(endpoints.first(), Some(&url));
    assert_eq!(client.status().endpoint.as_ref(), Some(&url));
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, ClientEvent::Connected | ClientEvent::Status(_)),
            "a route refresh must not reconnect"
        );
    }
    client.close();
}

#[tokio::test]
async fn an_unreachable_lan_does_not_turn_a_revoked_login_into_a_reconnect_loop() {
    let (engine, url, _dir) = start().await;
    let (client, token, _) = paired(&engine, &url, "Revoked phone").await;
    client.close();
    let grant = engine
        .store()
        .devices(None)
        .unwrap()
        .into_iter()
        .find(|d| d.name == "Revoked phone")
        .unwrap();
    engine.store().revoke_device(&grant.id).unwrap();
    let key = tessera_secure::b64(
        tessera_server::load_identity(engine.paths())
            .unwrap()
            .public(),
    );
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lan = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let client = Client::connect(ServerConfig {
        endpoints: vec![lan, url],
        headers: BTreeMap::new(),
        token,
        server_key: key,
    });
    let mut events = client.subscribe();
    assert!(client.wait_connected(Duration::from_secs(3)).await.is_err());
    assert_eq!(client.status().state, tessera_client::ConnState::Offline);
    let error = client.status().error.unwrap_or_default();
    assert!(error.contains("pair again"), "{error}");
    let mut attempts = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, ClientEvent::Status(s) if s.state == tessera_client::ConnState::Connecting)
        {
            attempts += 1;
        }
    }
    // A fatal login retries slowly in case the server was restored. The dead
    // LAN must not disguise it as an ordinary failure with a 500 ms retry.
    assert!(attempts <= 1, "rapid retry loop: {attempts} attempts");
    client.close();
}

#[tokio::test]
async fn reconnect_and_cold_start_prefer_the_last_success_without_reordering_settings() {
    use axum::{
        Json, Router,
        extract::ws::{Message, WebSocketUpgrade},
        routing::get,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    async fn endpoint(delay: Arc<AtomicU64>, close: Arc<tokio::sync::Notify>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/api/health", get(move || {
                let delay = delay.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(delay.load(Ordering::Relaxed))).await;
                    Json(serde_json::json!({"name": "tessera"}))
                }
            }))
            .route("/api/ws", get(move |ws: WebSocketUpgrade| {
                let close = close.clone();
                async move { ws.on_upgrade(move |mut socket| async move {
                    loop {
                        tokio::select! {
                            _ = close.notified() => { let _ = socket.send(Message::Close(None)).await; break; }
                            message = socket.recv() => match message {
                                Some(Ok(Message::Text(text))) => {
                                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                                    let reply = serde_json::json!({"id": value["id"], "result": []});
                                    if socket.send(Message::Text(reply.to_string().into())).await.is_err() { break; }
                                }
                                Some(Ok(Message::Ping(p))) => { let _ = socket.send(Message::Pong(p)).await; }
                                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                                _ => {}
                            }
                        }
                    }
                }) }
            }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }
    let lan_delay = Arc::new(AtomicU64::new(5000));
    let remote_delay = Arc::new(AtomicU64::new(0));
    let close_remote = Arc::new(tokio::sync::Notify::new());
    let lan = endpoint(lan_delay.clone(), Arc::default()).await;
    let remote = endpoint(remote_delay.clone(), close_remote.clone()).await;
    let saved = vec![lan.clone(), remote.clone()];
    let config = ServerConfig {
        endpoints: saved.clone(),
        ..Default::default()
    };
    let client = Client::connect(config.clone());
    client.wait_connected(Duration::from_secs(2)).await.unwrap();
    assert_eq!(client.status().endpoint.as_ref(), Some(&remote));
    client
        .call(Request::ListSessions { archived: false })
        .await
        .unwrap();
    // Now LAN is faster, but route-follow must not reset the remembered winner.
    lan_delay.store(0, Ordering::Relaxed);
    remote_delay.store(60, Ordering::Relaxed);
    client.set_route(saved.clone(), BTreeMap::new());
    let mut events = client.subscribe();
    close_remote.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(events.recv().await, Ok(ClientEvent::Connected)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(client.status().endpoint.as_ref(), Some(&remote));
    assert_eq!(saved, [lan, remote.clone()], "settings were not reordered");
    client.close();
    let cold = Client::connect_preferred(config, Some(remote.clone()));
    cold.wait_connected(Duration::from_secs(2)).await.unwrap();
    assert_eq!(cold.status().endpoint.as_ref(), Some(&remote));
    cold.close();
}
