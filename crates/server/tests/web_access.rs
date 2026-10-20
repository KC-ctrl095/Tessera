//! "Use Tessera in a web browser": nothing at all while it's off; with it on,
//! the web app over https only (through a proxy on the server's computer,
//! as cloudflared is), with a strict security policy; browsers pair with a
//! code from one of the user's devices and a key of their own, connect end
//! to end encrypted only through `/web` with proof of that key, show up in
//! Devices and can be signed out there; pairing is rate-limited; turning it
//! off closes their connections. A browser is played here the way the web
//! app does it (crates/secure on the Rust side of the same protocol).

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_proto::e2e::{
    BROWSER_PROOF, PairFinished, PairGrant, PairHello, PairStarted, SessionAuth, SessionAuthReply,
};
use tessera_proto::*;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const HOST: &str = "tessera.example.com";

struct Server {
    engine: tessera_core::Engine,
    url: String,
    _dir: tempfile::TempDir,
}

/// A server with a web app in `--web` (on `with_app`), and the tunnel's
/// address recorded for devices.
async fn server(with_app: bool) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path().join("home")), Default::default())
        .await
        .unwrap();
    let web = dir.path().join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(
        web.join("index.html"),
        "<!doctype html><script>document.documentElement.dataset.theme = 'dark';</script><script type=\"module\" src=\"./assets/index-abc.js\"></script>",
    )
    .unwrap();
    std::fs::write(web.join("assets/index-abc.js"), "export {};\n").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = tessera_server::router(engine.clone(), with_app.then_some(web)).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    engine
        .handle(
            Request::SetRemoteAccess {
                urls: vec![format!("https://{HOST}")],
                headers: None,
                add_headers: None,
                remove_headers: vec![],
            },
            None,
        )
        .await
        .unwrap();
    Server {
        engine,
        url,
        _dir: dir,
    }
}

/// How cloudflared on the server's computer forwards a browser's request
/// (`ip`: the browser's address).
fn via_tunnel(r: reqwest::RequestBuilder, ip: &str) -> reqwest::RequestBuilder {
    r.header("host", HOST)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", ip)
        .header("cf-connecting-ip", ip)
        .header("cf-visitor", "{\"scheme\":\"https\"}")
}

async fn set(s: &Server, on: bool) -> RemoteStatus {
    let v = s
        .engine
        .handle(Request::SetWebAccess { on, token: None }, None)
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn code(s: &Server) -> String {
    let v = s.engine.handle(Request::CreatePairing, None).await.unwrap();
    v["code"].as_str().unwrap().to_string()
}

/// A browser's key (Web Crypto's ECDSA P-256 in the real one).
struct Key(EcdsaKeyPair);

impl Key {
    fn new() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        Self(
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap(),
        )
    }
    fn public(&self) -> String {
        tessera_secure::b64(self.0.public_key().as_ref())
    }
    fn prove(&self, handshake_hash: &[u8]) -> String {
        let mut m = BROWSER_PROOF.to_vec();
        m.extend_from_slice(handshake_hash);
        tessera_secure::b64(self.0.sign(&SystemRandom::new(), &m).unwrap().as_ref())
    }
}

/// Pair through `path` (`/web/api/pair` or the apps' `/api/pair`): the grant, or (status, error).
async fn pair(
    s: &Server,
    path: &str,
    code: &str,
    key: Option<&Key>,
    ip: &str,
) -> Result<PairGrant, (u16, String)> {
    let http = reqwest::Client::new();
    let post = |p: String, body: Value| {
        via_tunnel(http.post(format!("{}{p}", s.url)), ip)
            .header("origin", format!("https://{HOST}"))
            .json(&body)
    };
    let (pake, msg) = tessera_secure::pairing::DevicePake::start(code);
    let r = post(
        format!("{path}/start"),
        json!({"pake": tessera_secure::b64(&msg)}),
    )
    .send()
    .await
    .unwrap();
    if !r.status().is_success() {
        let st = r.status().as_u16();
        return Err((st, r.text().await.unwrap_or_default()));
    }
    let started: PairStarted = r.json().await.unwrap();
    let keys = pake
        .finish(&tessera_secure::unb64(&started.pake).unwrap())
        .unwrap();
    let hello = PairHello {
        device_name: "Firefox on Windows".into(),
        machine: false,
        device_key: key.map(Key::public),
    };
    let sealed = keys.seal_to_server(&serde_json::to_vec(&hello).unwrap());
    let r = post(
        format!("{path}/finish"),
        json!({"pairing": started.pairing, "sealed": tessera_secure::b64(&sealed)}),
    )
    .send()
    .await
    .unwrap();
    if !r.status().is_success() {
        let st = r.status().as_u16();
        return Err((st, r.text().await.unwrap_or_default()));
    }
    let done: PairFinished = r.json().await.unwrap();
    let grant = keys
        .open_from_server(&tessera_secure::unb64(&done.sealed).unwrap())
        .unwrap();
    Ok(serde_json::from_slice(&grant).unwrap())
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// An encrypted session at `path` (`/web/api/secure` or `/api/secure`) as
/// the login `token` with `proof` from `key`: the socket and channel once
/// the server said yes, or why not.
async fn session(
    s: &Server,
    path: &str,
    origin: &str,
    grant: &PairGrant,
    key: Option<&Key>,
) -> Result<(Ws, tessera_secure::Channel), String> {
    log_in(socket(s, path, origin).await?, grant, key).await
}

/// The socket at `path`, opened from a page at `origin` through the tunnel.
async fn socket(s: &Server, path: &str, origin: &str) -> Result<Ws, String> {
    let mut req = format!("{}{path}", s.url.replace("http", "ws"))
        .into_client_request()
        .unwrap();
    for (k, v) in [
        ("host", HOST),
        ("origin", origin),
        ("x-forwarded-proto", "https"),
        ("cf-connecting-ip", "203.0.113.5"),
    ] {
        req.headers_mut().insert(k, v.parse().unwrap());
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("refused: {e}"))?;
    Ok(ws)
}

/// The handshake and login on an open socket.
async fn log_in(
    mut ws: Ws,
    grant: &PairGrant,
    key: Option<&Key>,
) -> Result<(Ws, tessera_secure::Channel), String> {
    let server_key = tessera_secure::unb64(&grant.server_key).unwrap();
    let (init, first) = tessera_secure::Initiator::start(&server_key).unwrap();
    ws.send(Message::Binary(first.into())).await.unwrap();
    let reply = match ws.next().await {
        Some(Ok(Message::Binary(b))) => b.to_vec(),
        other => return Err(format!("no handshake reply: {other:?}")),
    };
    let mut ch = init.finish(&reply).unwrap();
    let auth = SessionAuth {
        token: grant.token.clone(),
        proof: key.map(|k| k.prove(&ch.handshake_hash)),
    };
    for f in ch.sealer.seal(&serde_json::to_vec(&auth).unwrap()).unwrap() {
        ws.send(Message::Binary(f.into())).await.unwrap();
    }
    let answer = loop {
        match ws.next().await {
            Some(Ok(Message::Binary(b))) => {
                if let Some(m) = ch.opener.open(&b).unwrap() {
                    break m;
                }
            }
            other => return Err(format!("no answer: {other:?}")),
        }
    };
    let answer: SessionAuthReply = serde_json::from_slice(&answer).unwrap();
    if !answer.ok {
        return Err(answer.error.map(|e| e.message).unwrap_or_default());
    }
    Ok((ws, ch))
}

/// One request on an open session: its result.
async fn ask(ws: &mut Ws, ch: &mut tessera_secure::Channel, id: u64, req: Value) -> Value {
    let mut msg = req;
    msg["id"] = json!(id);
    for f in ch.sealer.seal(msg.to_string().as_bytes()).unwrap() {
        ws.send(Message::Binary(f.into())).await.unwrap();
    }
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("an answer");
        let Some(Ok(Message::Binary(b))) = frame else {
            panic!("closed: {frame:?}");
        };
        if let Some(m) = ch.opener.open(&b).unwrap() {
            let v: Value = serde_json::from_slice(&m).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }
}

#[tokio::test]
async fn off_by_default_and_then_nothing_is_served() {
    let s = server(true).await;
    let st: RemoteStatus =
        serde_json::from_value(s.engine.handle(Request::RemoteStatus, None).await.unwrap())
            .unwrap();
    let web = st.web.expect("the switch's state");
    assert!(!web.on && web.available);
    assert_eq!(web.url.as_deref(), Some("https://tessera.example.com/web/"));
    let http = reqwest::Client::new();
    for path in [
        "/web",
        "/web/",
        "/web/index.html",
        "/web/assets/index-abc.js",
        "/web/api/health",
        "/web/api/secure",
    ] {
        for local in [true, false] {
            let r = http.get(format!("{}{path}", s.url));
            let r = if local {
                r
            } else {
                via_tunnel(r, "203.0.113.5")
            };
            let r = r.send().await.unwrap();
            assert_eq!(r.status(), 404, "{path} (local: {local})");
            assert!(
                r.bytes().await.unwrap().is_empty(),
                "{path}: nothing at all"
            );
        }
    }
    let r = via_tunnel(
        http.post(format!("{}/web/api/pair/start", s.url)),
        "203.0.113.5",
    )
    .header("origin", format!("https://{HOST}"))
    .json(&json!({"pake": "AAAA"}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 404);

    // Without a web app it can't be turned on.
    let bare = server(false).await;
    let e = bare
        .engine
        .handle(
            Request::SetWebAccess {
                on: true,
                token: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(e.message.contains("without the web app"), "{}", e.message);
}

#[tokio::test]
async fn on_the_app_comes_over_https_only_with_a_strict_policy() {
    let s = server(true).await;
    assert!(set(&s, true).await.web.unwrap().on);
    let http = reqwest::Client::new();
    let r = via_tunnel(http.get(format!("{}/web/", s.url)), "203.0.113.5")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let h = r.headers().clone();
    let csp = h["content-security-policy"].to_str().unwrap().to_string();
    assert!(csp.starts_with("default-src 'none';"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(
        csp.contains("connect-src 'self' wss://tessera.example.com;"),
        "{csp}"
    );
    // Only the page's own inline script may run; nothing from elsewhere.
    assert_eq!(csp.matches("'sha256-").count(), 1, "{csp}");
    for source in csp.split([' ', ';']) {
        assert!(
            !matches!(
                source,
                "http:" | "https:" | "ws:" | "wss:" | "*" | "'unsafe-eval'"
            ),
            "{source} in {csp}"
        );
    }
    assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
    for (name, want) in [
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-resource-policy", "same-origin"),
        ("strict-transport-security", "max-age=31536000"),
        ("cache-control", "no-cache"),
    ] {
        assert_eq!(h[name], want, "{name}");
    }
    assert!(
        h["permissions-policy"]
            .to_str()
            .unwrap()
            .contains("geolocation=()")
    );
    assert!(r.text().await.unwrap().contains("./assets/index-abc.js"));
    // Its files, and `/web` takes you to `/web/`.
    let r = via_tunnel(
        http.get(format!("{}/web/assets/index-abc.js", s.url)),
        "203.0.113.5",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    // Kept by the browser only: an edge cache in front (Cloudflare's) would
    // go on giving it out after web access is switched off.
    assert_eq!(
        r.headers()["cache-control"],
        "private, max-age=31536000, immutable"
    );
    let r = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("{}/web", s.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 308);
    assert_eq!(r.headers()["location"], "/web/");
    // On the server itself (http://localhost) too, but never over plain
    // http from elsewhere: a proxy here saying "http" is turned away.
    let r = http.get(format!("{}/web/", s.url)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers().get("strict-transport-security").is_none());
    let r = http
        .get(format!("{}/web/", s.url))
        .header("host", HOST)
        .header("x-forwarded-proto", "http")
        .header("x-forwarded-for", "203.0.113.5")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert!(r.text().await.unwrap().contains("only over https"));
}

#[tokio::test]
async fn browsers_pair_with_a_code_and_their_own_key_and_can_be_signed_out() {
    let s = server(true).await;
    set(&s, true).await;
    let key = Key::new();
    // No code made on a device: no way in.
    let e = pair(&s, "/web/api/pair", "1234-5678", Some(&key), "203.0.113.5")
        .await
        .unwrap_err();
    assert_eq!(e.0, 401, "{e:?}");
    // A browser must bring a key.
    let c = code(&s).await;
    let e = pair(&s, "/web/api/pair", &c, None, "203.0.113.5")
        .await
        .unwrap_err();
    assert_eq!(e.0, 400, "{e:?}");
    assert!(e.1.contains("key"), "{e:?}");
    // From another site's page: refused.
    let r = via_tunnel(
        reqwest::Client::new().post(format!("{}/web/api/pair/start", s.url)),
        "203.0.113.5",
    )
    .header("origin", "https://evil.example")
    .json(&json!({"pake": "AAAA"}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 403);

    let c = code(&s).await;
    let grant = pair(&s, "/web/api/pair", &c, Some(&key), "203.0.113.5")
        .await
        .expect("paired");
    assert!(!grant.machine);
    let devices: Vec<DeviceInfo> =
        serde_json::from_value(s.engine.handle(Request::ListDevices, None).await.unwrap()).unwrap();
    let me = devices.iter().find(|d| d.id == grant.device_id).unwrap();
    assert!(me.browser && me.name == "Firefox on Windows");

    // It connects at /web with the proof of its key, and works.
    let origin = format!("https://{HOST}");
    let (mut ws, mut ch) = session(&s, "/web/api/secure", &origin, &grant, Some(&key))
        .await
        .expect("connected");
    let r = ask(&mut ws, &mut ch, 1, json!({"method": "list_devices"})).await;
    assert!(r["result"].is_array(), "{r}");

    // Its page comes from whoever serves the address, so terminals never
    // open from it, and it can't turn them on: it only sees they exist.
    let r = ask(&mut ws, &mut ch, 2, json!({"method": "terminal_status"})).await;
    assert_eq!(r["result"]["private"], json!(false), "{r}");
    for (id, req) in [
        (
            3,
            json!({"method": "terminal_open", "params": {"cols": 80, "rows": 24}}),
        ),
        (
            4,
            json!({"method": "set_terminal_settings", "params": {"settings": {"access": "full", "idle_minutes": 30}}}),
        ),
    ] {
        let r = ask(&mut ws, &mut ch, id, req).await;
        assert_eq!(
            r["error"]["message"],
            json!("Terminals open only from the Tessera app, not from a web browser."),
            "{r}"
        );
    }

    // The token alone isn't enough: without the key's proof, with another
    // key's, from another site's page, through the apps' socket or the
    // plain one, it doesn't get in.
    assert!(
        session(&s, "/web/api/secure", &origin, &grant, None)
            .await
            .is_err()
    );
    assert!(
        session(&s, "/web/api/secure", &origin, &grant, Some(&Key::new()))
            .await
            .is_err()
    );
    assert!(
        session(
            &s,
            "/web/api/secure",
            "https://evil.example",
            &grant,
            Some(&key)
        )
        .await
        .is_err()
    );
    assert!(
        session(&s, "/api/secure", &origin, &grant, Some(&key))
            .await
            .is_err()
    );
    let plain = tokio_tungstenite::connect_async(format!(
        "{}/api/ws?token={}",
        s.url.replace("http", "ws"),
        grant.token
    ))
    .await;
    match plain {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("a browser's token got in on the plain socket"),
    }
    // And an app's login doesn't work at /web.
    let app_code = code(&s).await;
    let app = pair(&s, "/api/pair", &app_code, None, "203.0.113.5")
        .await
        .expect("an app paired");
    assert!(
        session(&s, "/api/secure", &origin, &app, None)
            .await
            .is_ok()
    );
    assert!(
        session(&s, "/web/api/secure", &origin, &app, Some(&key))
            .await
            .is_err()
    );

    // Signed out in Devices: its connection closes at once, and it can't
    // come back.
    s.engine
        .handle(
            Request::RevokeDevice {
                id: grant.device_id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(_))) | Some(Ok(Message::Ping(_))) => continue,
                _ => return,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the connection closed");
    let Err(e) = session(&s, "/web/api/secure", &origin, &grant, Some(&key)).await else {
        panic!("a signed-out browser got in");
    };
    assert!(e.contains("pair again"), "{e}");
}

#[tokio::test]
async fn turning_it_off_closes_browsers_and_keeps_them_out() {
    let s = server(true).await;
    set(&s, true).await;
    let key = Key::new();
    let c = code(&s).await;
    let grant = pair(&s, "/web/api/pair", &c, Some(&key), "203.0.113.5")
        .await
        .unwrap();
    let origin = format!("https://{HOST}");
    let (mut ws, _ch) = session(&s, "/web/api/secure", &origin, &grant, Some(&key))
        .await
        .unwrap();
    let st = set(&s, false).await.web.unwrap();
    assert!(!st.on);
    assert_eq!(st.browsers, 1, "still paired (signed out in Devices)");
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(_))) | Some(Ok(Message::Ping(_))) => continue,
                _ => return,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "closed when switched off");
    assert!(
        session(&s, "/web/api/secure", &origin, &grant, Some(&key))
            .await
            .is_err()
    );
    // On again: the same browser comes back without pairing again.
    set(&s, true).await;
    assert!(
        session(&s, "/web/api/secure", &origin, &grant, Some(&key))
            .await
            .is_ok()
    );
    // Switched off while it was connecting: told so (it keeps its pairing),
    // not told to pair again.
    let ws = socket(&s, "/web/api/secure", &origin).await.unwrap();
    set(&s, false).await;
    let Err(e) = log_in(ws, &grant, Some(&key)).await else {
        panic!("got in while switched off");
    };
    assert!(e.contains("turned off") && !e.contains("pair again"), "{e}");
}

#[tokio::test]
async fn pairing_at_the_web_door_is_rate_limited_by_address() {
    let s = server(true).await;
    set(&s, true).await;
    let http = reqwest::Client::new();
    let start = |ip: &'static str| {
        let (_, msg) = tessera_secure::pairing::DevicePake::start("1234-5678");
        via_tunnel(http.post(format!("{}/web/api/pair/start", s.url)), ip)
            .header("origin", format!("https://{HOST}"))
            .json(&json!({"pake": tessera_secure::b64(&msg)}))
            .send()
    };
    let mut codes = Vec::new();
    for _ in 0..12 {
        codes.push(start("198.51.100.7").await.unwrap().status().as_u16());
    }
    assert_eq!(&codes[..10], &[200; 10], "{codes:?}");
    assert_eq!(&codes[10..], &[429, 429], "{codes:?}");
    // Someone else isn't held up by that address, nor are the apps.
    assert_eq!(start("198.51.100.8").await.unwrap().status(), 200);
    let key = Key::new();
    let c = code(&s).await;
    assert!(
        pair(&s, "/api/pair", &c, Some(&key), "198.51.100.7")
            .await
            .is_ok()
    );

    // Wrong codes: a few per address, then it waits.
    let c = code(&s).await;
    let wrong = if c.starts_with('9') {
        "1111-1111"
    } else {
        "9999-9999"
    };
    let mut seen = Vec::new();
    for _ in 0..6 {
        seen.push(
            pair(&s, "/web/api/pair", wrong, Some(&key), "192.0.2.44")
                .await
                .unwrap_err()
                .0,
        );
    }
    assert!(seen.ends_with(&[429]), "{seen:?}");
    // Three wrong answers cancelled that code; a new one works from elsewhere.
    let c2 = code(&s).await;
    assert!(
        pair(&s, "/web/api/pair", &c2, Some(&key), "192.0.2.45")
            .await
            .is_ok()
    );
}

/// Addresses at the web door are whatever a proxy on this computer says, so
/// any program on it can make up as many as it likes: its brakes slow
/// everyone down but never shut the owner out. And a page from another site
/// that had its name point at this computer (DNS rebinding) is given away
/// by its own origin, whatever it adds.
#[tokio::test]
async fn nobody_can_shut_the_owner_out_of_pairing_a_browser() {
    let s = server(true).await;
    set(&s, true).await;
    let key = Key::new();
    // Wrong answers (no code is even active) from a dozen made-up
    // addresses: past the door's overall limit.
    for i in 0..12 {
        let ip = format!("198.51.100.{}", 10 + i);
        let e = pair(&s, "/web/api/pair", "1111-2222", Some(&key), &ip)
            .await
            .unwrap_err();
        assert_eq!(e.0, 401, "{e:?}");
    }
    // And attempts left hanging, enough to fill every slot in flight.
    let http = reqwest::Client::new();
    for i in 0..20 {
        let (_, msg) = tessera_secure::pairing::DevicePake::start("1111-2222");
        let r = via_tunnel(
            http.post(format!("{}/web/api/pair/start", s.url)),
            &format!("192.0.2.{}", 100 + i),
        )
        .header("origin", format!("https://{HOST}"))
        .json(&json!({"pake": tessera_secure::b64(&msg)}))
        .send()
        .await
        .unwrap();
        assert_eq!(r.status(), 200);
    }
    // The owner, from their own address with a real code, pairs all the same.
    let c = code(&s).await;
    pair(&s, "/web/api/pair", &c, Some(&key), "203.0.113.9")
        .await
        .expect("the owner pairs");
    // An address that failed before waits now.
    let e = pair(
        &s,
        "/web/api/pair",
        "1111-2222",
        Some(&key),
        "198.51.100.10",
    )
    .await
    .unwrap_err();
    assert_eq!(e.0, 429, "{e:?}");

    let r = reqwest::Client::new()
        .post(format!("{}/web/api/pair/start", s.url))
        .header("host", "evil.example")
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", "evil.example")
        .header("x-forwarded-for", "198.51.100.99")
        .header("origin", "http://evil.example")
        .json(&json!({"pake": "AAAA"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert!(
        r.text()
            .await
            .unwrap()
            .contains("not from Tessera's own page")
    );
}

/// How devices reach the server (its addresses, and the key for Cloudflare
/// Access) goes only to the user's apps on an encrypted connection, and to
/// a machine connecting from another computer (it needs them away from
/// home): never to a web browser, over a plain connection, or to a machine
/// on the server's own computer.
#[tokio::test]
async fn only_the_apps_learn_the_servers_addresses_and_access_key() {
    let s = server(true).await;
    s.engine
        .handle(
            Request::SetRemoteAccess {
                urls: vec![format!("https://{HOST}")],
                headers: None,
                add_headers: Some(
                    [
                        ("CF-Access-Client-Id".to_string(), "id.access".to_string()),
                        (
                            "CF-Access-Client-Secret".to_string(),
                            "very-secret".to_string(),
                        ),
                    ]
                    .into(),
                ),
                remove_headers: vec![],
            },
            None,
        )
        .await
        .unwrap();
    set(&s, true).await;
    let origin = format!("https://{HOST}");
    let route = json!({"method": "get_server_route"});

    // An app, through the tunnel: the addresses and the key.
    let c = code(&s).await;
    let app = pair(&s, "/api/pair", &c, None, "203.0.113.7")
        .await
        .unwrap();
    let (mut ws, mut ch) = session(&s, "/api/secure", &origin, &app, None)
        .await
        .unwrap();
    let r = ask(&mut ws, &mut ch, 1, route.clone()).await;
    assert_eq!(
        r["result"]["urls"],
        json!([format!("https://{HOST}")]),
        "{r}"
    );
    assert_eq!(
        r["result"]["headers"]["CF-Access-Client-Secret"],
        "very-secret"
    );

    // A web browser: refused, and the key isn't in the answer.
    let key = Key::new();
    let c = code(&s).await;
    let b = pair(&s, "/web/api/pair", &c, Some(&key), "203.0.113.5")
        .await
        .unwrap();
    let (mut bws, mut bch) = session(&s, "/web/api/secure", &origin, &b, Some(&key))
        .await
        .unwrap();
    let r = ask(&mut bws, &mut bch, 1, route.clone()).await;
    assert_eq!(r["error"]["code"], "unauthorized", "{r}");
    assert!(!r.to_string().contains("very-secret"));

    // The app's machine side: from another computer (through the tunnel)
    // it gets them; on the server's own computer it has nothing to learn.
    let r = ask(&mut ws, &mut ch, 2, json!({"method": "machine_login"})).await;
    let machine = PairGrant {
        token: r["result"]["token"].as_str().unwrap().to_string(),
        ..app.clone()
    };
    let (mut mws, mut mch) = session(&s, "/api/secure", &origin, &machine, None)
        .await
        .unwrap();
    let r = ask(&mut mws, &mut mch, 1, route.clone()).await;
    assert_eq!(
        r["result"]["headers"]["CF-Access-Client-Id"], "id.access",
        "{r}"
    );
    let here = format!("{}/api/secure", s.url.replace("http", "ws"));
    let (direct, _) = tokio_tungstenite::connect_async(here).await.unwrap();
    let (mut lws, mut lch) = log_in(direct, &machine, None).await.unwrap();
    let r = ask(&mut lws, &mut lch, 1, route.clone()).await;
    assert_eq!(r["error"]["code"], "unauthorized", "{r}");

    // A plain (unencrypted) connection on this computer: refused too.
    let plain = format!("{}/api/ws?token={}", s.url.replace("http", "ws"), app.token);
    let (mut pws, _) = tokio_tungstenite::connect_async(plain).await.unwrap();
    pws.send(Message::Text(
        json!({"id": 7, "method": "get_server_route"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let reply = loop {
        let m = tokio::time::timeout(Duration::from_secs(10), pws.next())
            .await
            .expect("an answer");
        if let Some(Ok(Message::Text(t))) = m {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["id"] == json!(7) {
                break v;
            }
        }
    };
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    assert!(!reply.to_string().contains("very-secret"));
}
