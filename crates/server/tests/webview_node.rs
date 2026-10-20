//! Web views on a machine ("Web views here"), end to end with real browsers:
//! a server, a computer running the node (its own Tessera home and its own
//! Chromium, end-to-end encrypted over a real socket), the desktop app on
//! that same computer (the same paired device), and a phone, with a scripted
//! model driving the `browser` tool. Covers placement by the device in use,
//! the agent driving a page on the machine, the phone seeing it relayed, who
//! may use it, what the machine refuses, switching web views off there, and
//! the machine going offline. Skipped when no Chromium-family browser is
//! installed.

#[path = "../../core/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{Mock, Reply, tool_results};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_client::{Client, ClientEvent, ConnState, ServerConfig};
use tessera_core::config::Paths;
use tessera_core::node::{Exit, Link, LinkEvent, Node, NodeConfig};
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;
use tokio_util::sync::CancellationToken;

struct ClientLink(Client);

impl Link for ClientLink {
    fn call(&self, req: Request) -> BoxFuture<'static, Result<Value, ApiError>> {
        let c = self.0.clone();
        Box::pin(async move { c.call_timeout(req, Duration::from_secs(30)).await })
    }
}

/// What `tessera node run` does: the paired device's connection, as events.
fn pump(client: &Client) -> tokio::sync::mpsc::UnboundedReceiver<LinkEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut events = client.subscribe();
    let client = client.clone();
    tokio::spawn(async move {
        let mut up = false;
        if client.status().state == ConnState::Connected {
            up = true;
            let _ = tx.send(LinkEvent::Up(client.status().endpoint));
        }
        while let Ok(ev) = events.recv().await {
            let _ = match ev {
                ClientEvent::Connected => {
                    up = true;
                    tx.send(LinkEvent::Up(client.status().endpoint))
                }
                ClientEvent::Status(s) if s.state != ConnState::Connected && up => {
                    up = false;
                    tx.send(LinkEvent::Down(s.error))
                }
                ClientEvent::Event(e) => tx.send(LinkEvent::Event(e)),
                _ => Ok(()),
            };
        }
    });
    rx
}

struct Machine {
    task: tokio::task::JoinHandle<Exit>,
    client: Client,
    stop: CancellationToken,
}

fn start_machine(paths: &Paths, login: &ServerConfig) -> Machine {
    let node = Node::open(paths.clone()).unwrap();
    let client = Client::connect(login.clone());
    let events = pump(&client);
    let stop = CancellationToken::new();
    let task = tokio::spawn(node.serve(Arc::new(ClientLink(client.clone())), events, stop.clone()));
    Machine { task, client, stop }
}

fn set(paths: &Paths, f: impl FnOnce(&mut NodeConfig)) {
    let mut c = NodeConfig::load(paths).unwrap();
    f(&mut c);
    c.save(paths).unwrap();
}

async fn wait_for(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..300 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn machines(engine: &Engine) -> Vec<MachineInfo> {
    serde_json::from_value(engine.handle(Request::ListMachines, None).await.unwrap()).unwrap()
}

/// Approve everything the chat asks.
fn approver(engine: Engine) -> tokio::task::JoinHandle<()> {
    let mut rx = engine.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Event::ApprovalRequested {
                session_id,
                request,
            } = ev
            {
                let _ = engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision: ApprovalDecision::Allow,
                        },
                        None,
                    )
                    .await;
            }
        }
    })
}

/// One turn in which the model calls `browser` with `args`: (is_error, text).
async fn browse(engine: &Engine, mock: &Mock, sid: &str, args: Value) -> (bool, String) {
    mock.push(Reply::Tools(vec![("browser".into(), args)]));
    mock.push(Reply::Text("done".into()));
    engine
        .handle(
            Request::Send {
                session_id: sid.into(),
                content: vec![ContentPart::text("use the page")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(60), engine.wait_idle(sid))
        .await
        .expect("the chat finishes");
    let v = engine
        .handle(
            Request::GetSession {
                session_id: sid.into(),
                since_seq: None,
            },
            None,
        )
        .await
        .unwrap();
    let snap: SessionSnapshot = serde_json::from_value(v).unwrap();
    tool_results(&snap.entries)
        .into_iter()
        .rev()
        .find(|(t, _, _)| t == "browser")
        .map(|(_, e, t)| (e, t))
        .expect("a browser result")
}

/// A small site the browsers can reach.
async fn site() -> String {
    use axum::routing::get;
    let app = axum::Router::new()
        .route(
            "/",
            get(|| async {
                axum::response::Html(
                    "<!doctype html><title>Shop</title><h1>Welcome to the test shop</h1><p><a href=\"/two\" style=\"font-size:30px\">Second page</a></p><input id=q placeholder=Search>",
                )
            }),
        )
        .route(
            "/two",
            get(|| async {
                axum::response::Html("<!doctype html><title>Two</title><p>Page two text</p>")
            }),
        )
        .route("/upload", get(|| async { axum::response::Html(r#"<!doctype html><title>Upload</title><button onclick="f.click()">Choose files</button><input id=f type=file multiple accept=".txt" hidden><pre id=out>No files</pre><script>f.onchange=async e=>{out.textContent=JSON.stringify({trusted:e.isTrusted,files:await Promise.all([...f.files].map(async x=>({name:x.name,size:x.size,hash:[...new Uint8Array(await crypto.subtle.digest('SHA-256',await x.arrayBuffer()))].map(b=>b.toString(16).padStart(2,'0')).join('')})))})}</script>"#) }))
        // Offers a security key only when the browser has them, and asks
        // for one either way.
        .route(
            "/keys",
            get(|| async {
                axum::response::Html(
                    "<!doctype html><title>Keys</title><p>Sign in</p><script>(async () => { const t = performance.now(); let r; try { await navigator.credentials.get({ publicKey: { challenge: new Uint8Array(32), timeout: 60000 } }); r = 'ok'; } catch (e) { r = e.name; } document.title = 'offer:' + (window.PublicKeyCredential ? 'key' : 'password') + ' get:' + r + (performance.now() - t < 5000 ? ' at-once' : ' slow'); })();</script>",
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// The next frame of `board` on this connection (acknowledged).
async fn frame(
    client: &Client,
    rx: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    board: &str,
) -> Option<(u64, usize)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .ok()?
            .ok()?;
        if let ClientEvent::Event(Event::BrowserFrame {
            board_id,
            frame,
            data,
            ..
        }) = ev
            && board_id == board
        {
            let _ = client
                .call(Request::BrowserAck {
                    board_id: board.into(),
                    frame,
                })
                .await;
            return Some((frame, data.len()));
        }
    }
}

fn attach_req(board: &str, mobile: bool) -> Request {
    Request::BrowserAttach {
        board_id: board.into(),
        width: if mobile { 412 } else { 1280 },
        height: if mobile { 900 } else { 800 },
        dpr: if mobile { 2.0 } else { 1.0 },
        mobile,
        pipelined: true,
        formats: Vec::new(),
    }
}

async fn create_board(client: &Client, sid: &str, url: &str) -> Whiteboard {
    client
        .call_as(Request::CreateBoard {
            session_id: sid.into(),
            title: "Web".into(),
            content: BoardContent::Url { url: url.into() },
            placed: true,
        })
        .await
        .unwrap()
}

/// Whether a browser runs with this profile folder.
fn browser_running(profile: &Path) -> bool {
    let needle = format!("--user-data-dir={}", profile.display());
    std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .map(|c| String::from_utf8_lossy(&c).split('\0').any(|a| a == needle))
            .unwrap_or(false)
    })
}

#[test]
fn web_views_run_on_the_machine_the_user_is_on() {
    if !tessera_core::browser::installed(None) {
        eprintln!("no Chromium-family browser installed; skipping");
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(scenario());
}

async fn scenario() {
    let mock = Mock::start(Vec::new()).await;
    let root = tempfile::tempdir().unwrap();
    let root_path: PathBuf = root.path().canonicalize().unwrap();
    let server_dir = root_path.join("server-home");
    std::fs::create_dir_all(&server_dir).unwrap();
    std::fs::write(
        server_dir.join("config.toml"),
        format!(
            "default_model = \"mock/m\"\n[agent]\nreview_interval = 0\n[server]\nname = \"Home server\"\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nname = \"Mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\nmax_output = 2000\n",
            mock.url
        ),
    )
    .unwrap();
    let engine = Engine::open(Paths::at(&server_dir), EngineOptions::default())
        .await
        .unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = tessera_server::serve_on(listener, app).await;
    });
    let url = format!("http://127.0.0.1:{port}");
    let key = tessera_secure::b64(
        tessera_server::load_identity(engine.paths())
            .unwrap()
            .public(),
    );
    let login = |token: String| ServerConfig {
        endpoints: vec![url.clone()],
        headers: Default::default(),
        token,
        server_key: key.clone(),
    };
    let (desktop, desktop_token) = engine.store().add_device("Desktop").unwrap();
    let (_, phone_token) = engine.store().add_device("Phone").unwrap();
    let _approving = approver(engine.clone());
    let site = site().await;
    let sid: String = {
        let v = engine
            .handle(
                Request::CreateSession {
                    cwd: Some(root_path.display().to_string()),
                    model: None,
                    title: Some("Shopping".into()),
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap();
        serde_json::from_value::<SessionInfo>(v).unwrap().id
    };

    // The desktop: a machine with web views on (its own Tessera home).
    let node_paths = Paths::at(root_path.join("desktop-home"));
    let upload_dir = root_path.join("upload-sources");
    std::fs::create_dir_all(&upload_dir).unwrap();
    node_paths.ensure().unwrap();
    set(&node_paths, |c| {
        c.enabled = true;
        c.name = "Desktop".into();
        c.generation = 1;
        c.scope = MachineScope {
            ask: MachineAsk::Risky,
            timeout_secs: 60,
            webviews: true,
            folders: vec![FolderAccess {
                path: upload_dir.to_string_lossy().into_owned(),
                write: false,
            }],
            ..Default::default()
        };
    });
    let profile = tessera_core::node::browser::dir(&node_paths).join("profile");
    let machine = start_machine(&node_paths, &login(desktop_token.clone()));
    wait_for("the desktop to come online", async || {
        machines(&engine)
            .await
            .first()
            .is_some_and(|m| m.online && m.scope.webviews)
    })
    .await;

    // ── made on the desktop: it runs there ────────────────────────────────
    // The desktop app: the same paired device as the machine.
    let app = Client::connect(login(desktop_token.clone()));
    app.wait_connected(Duration::from_secs(10)).await.unwrap();
    let mut app_events = app.subscribe();
    let _: Value = app
        .call(Request::GetSession {
            session_id: sid.clone(),
            since_seq: None,
        })
        .await
        .unwrap();
    let board = create_board(&app, &sid, &format!("{site}/")).await;
    assert_eq!(
        board.host.as_deref(),
        Some(desktop.as_str()),
        "on the desktop"
    );
    let tab: BrowserTab = app.call_as(attach_req(&board.id, false)).await.unwrap();
    assert_eq!(tab.board_id, board.id);
    let f = frame(&app, &mut app_events, &board.id).await;
    assert!(f.is_some_and(|(_, n)| n > 1000), "the desktop gets frames");
    assert!(
        browser_running(&profile),
        "the desktop's own browser draws it"
    );
    assert!(
        !engine.browser().stats().running,
        "the server's browser never started"
    );
    let on_desktop = engine.browsers().machine(&desktop).unwrap();
    assert!(on_desktop.stats().running && on_desktop.stats().frames >= 1);

    // ── the agent drives it from the server ─────────────────────────────────
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "snapshot", "tab": board.id}),
    )
    .await;
    assert!(!err && text.contains("Welcome to the test shop"), "{text}");
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "click", "tab": board.id, "text": "Second page"}),
    )
    .await;
    assert!(!err && text.contains("Page two text"), "{text}");
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "navigate", "tab": board.id, "url": format!("{site}/")}),
    )
    .await;
    assert!(!err && text.contains("Welcome"), "{text}");
    // Hand approvals to the upload-specific verifier, rather than racing
    // the scenario's general responder for the same pending request.
    _approving.abort();
    let _ = _approving.await;
    let upload_asks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = upload_asks.clone();
    let answer_engine = engine.clone();
    let mut approvals = engine.subscribe();
    let upload_answers = tokio::spawn(async move {
        while let Ok(event) = approvals.recv().await {
            if let Event::ApprovalRequested {
                session_id,
                request,
            } = event
            {
                if request.reason.contains("Sends these files") {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    assert!(
                        request.summary.contains("node-notes.txt")
                            && request.reason.contains("http://127.0.0.1")
                    );
                }
                answer_engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision: ApprovalDecision::Allow,
                        },
                        None,
                    )
                    .await
                    .unwrap();
            }
        }
    });
    // Agent uploads read only the node's own allowed folder, then deliver
    // those exact bytes through a native hidden input's chooser.
    let file = upload_dir.join("node-notes.txt");
    std::fs::write(&file, "selected machine notes").unwrap();
    let outside = root_path.join("outside.txt");
    std::fs::write(&outside, "outside notes").unwrap();
    let key_file = upload_dir.join("notes.key");
    std::fs::write(&key_file, "fixture").unwrap();
    let (_, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action":"navigate","tab":board.id,"url":format!("{site}/upload")}),
    )
    .await;
    assert!(text.contains("Choose files"), "{text}");
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action":"click","tab":board.id,"text":"Choose files"}),
    )
    .await;
    assert!(!err && text.contains("file picker"), "{text}");
    for path in [
        outside.clone(),
        key_file,
        node_paths.data_dir.join("fixture.txt"),
    ] {
        let (err, text) = browse(
            &engine,
            &mock,
            &sid,
            json!({"action":"upload","tab":board.id,"files":[path]}),
        )
        .await;
        assert!(
            err && (text.contains("outside")
                || text.contains("protected")
                || text.contains("Tessera")),
            "{text}"
        );
    }
    #[cfg(unix)]
    {
        let link = upload_dir.join("linked.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let (err, text) = browse(
            &engine,
            &mock,
            &sid,
            json!({"action":"upload","tab":board.id,"files":[link]}),
        )
        .await;
        assert!(err && text.contains("outside"), "{text}");
    }
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action":"upload","tab":board.id,"files":[file]}),
    )
    .await;
    assert!(
        !err && text.contains("node-notes.txt") && text.contains("trusted\":true"),
        "{text}"
    );
    assert_eq!(
        upload_asks.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the machine's risky-action rule asks even for an Auto chat"
    );
    upload_answers.abort();
    let _ = upload_answers.await;
    let _approving = approver(engine.clone());
    use sha2::{Digest, Sha256};
    assert!(
        text.contains(&format!("{:x}", Sha256::digest(b"selected machine notes"))),
        "{text}"
    );
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action":"navigate","tab":board.id,"url":format!("{site}/")}),
    )
    .await;
    assert!(!err && text.contains("Welcome"), "{text}");
    // No security keys on the machine's web views either: the page sees
    // none, and a request for one is turned down at once.
    let keys = format!("{}/keys", site.replace("127.0.0.1", "localhost"));
    let (err, _) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "navigate", "tab": board.id, "url": keys}),
    )
    .await;
    assert!(!err);
    let mut seen = String::new();
    for _ in 0..10 {
        let (_, text) = browse(
            &engine,
            &mock,
            &sid,
            json!({"action": "snapshot", "tab": board.id}),
        )
        .await;
        seen = text;
        if seen.contains("offer:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(
        seen.contains("offer:password get:NotAllowedError at-once"),
        "{seen}"
    );
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "navigate", "tab": board.id, "url": format!("{site}/")}),
    )
    .await;
    assert!(!err && text.contains("Welcome"), "{text}");
    // Every action is in the machine's own log, with the chat.
    let log_file = tessera_core::node::log_file(&node_paths);
    wait_for("the actions in the machine's log", async || {
        let log = tessera_core::node::log::NodeLog::read(&log_file, 400);
        let has = |what: &str| {
            log.iter().any(|e| {
                e.event == NodeLogEvent::Done
                    && e.detail == format!("web view {what}")
                    && e.chat.as_deref() == Some("Shopping")
            })
        };
        has("snapshot") && has("click") && has("navigate")
    })
    .await;

    // ── the phone sees the same page, relayed ───────────────────────────────
    let phone = Client::connect(login(phone_token));
    phone.wait_connected(Duration::from_secs(10)).await.unwrap();
    let mut phone_events = phone.subscribe();
    let _: Value = phone.call(attach_req(&board.id, true)).await.unwrap();
    assert!(
        frame(&phone, &mut phone_events, &board.id).await.is_some(),
        "the phone gets the desktop's frames"
    );
    assert!(!engine.browser().stats().running);

    // ── made on the phone: the server's ─────────────────────────────────────
    let _: Value = phone
        .call(Request::GetSession {
            session_id: sid.clone(),
            since_seq: None,
        })
        .await
        .unwrap();
    let board2 = create_board(&phone, &sid, &format!("{site}/two")).await;
    assert_eq!(board2.host, None, "made on the phone: the server's");
    // The agent, while the user is on the phone: the server too.
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "open", "url": format!("{site}/two")}),
    )
    .await;
    assert!(
        !err && text.contains("Page two text") && !text.contains("on Desktop"),
        "{text}"
    );
    // Back on the desktop: the agent's new tabs run there.
    let _: Value = app
        .call(Request::GetSession {
            session_id: sid.clone(),
            since_seq: None,
        })
        .await
        .unwrap();
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "open", "url": format!("{site}/")}),
    )
    .await;
    assert!(
        !err && text.contains("on Desktop") && text.contains("Welcome"),
        "{text}"
    );
    // The desktop's web view kept its host all along.
    let b: Whiteboard = app
        .call_as(Request::GetBoard {
            id: board.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(b.host.as_deref(), Some(desktop.as_str()));

    // ── what the machine refuses, whatever the server sends ─────────────────
    let secret = root_path.join("private-notes.txt");
    std::fs::write(&secret, "a private note").unwrap();
    for (method, params) in [
        (
            "Page.navigate",
            json!({"url": format!("file://{}", secret.display())}),
        ),
        ("Page.navigate", json!({"url": "chrome://settings"})),
        (
            "Browser.setDownloadBehavior",
            json!({"behavior": "allow", "downloadPath": root_path.display().to_string()}),
        ),
        ("Target.attachToBrowserTarget", json!({})),
        ("Target.createTarget", json!({"url": "file:///etc/passwd"})),
        (
            "Extensions.loadUnpacked",
            json!({"path": root_path.display().to_string()}),
        ),
        (
            "DOM.setFileInputFiles",
            json!({"files": [secret.display().to_string()], "nodeId": 1}),
        ),
        (
            "Input.dispatchDragEvent",
            json!({"type": "drop", "x": 5, "y": 5, "data": {"items": [], "files": [secret.display().to_string()]}}),
        ),
        ("Network.getAllCookies", json!({})),
    ] {
        let r = on_desktop.raw_cdp(&board.id, method, params).await;
        let e = r.expect_err(method);
        assert!(e.contains("refused by this machine"), "{method}: {e}");
    }
    // The agent and the user can't ask for files either.
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "navigate", "tab": board.id, "url": format!("file://{}", secret.display())}),
    )
    .await;
    assert!(err && !text.contains("a private note"), "{text}");
    let e = app
        .call(Request::BrowserNav {
            board_id: board.id.clone(),
            action: BrowserNavAction::Go {
                url: format!("file://{}", secret.display()),
            },
        })
        .await
        .unwrap_err();
    assert!(e.message.contains("can't be opened"), "{}", e.message);
    {
        let log = tessera_core::node::log::NodeLog::read(&log_file, 400);
        assert!(
            log.iter().any(|e| e.event == NodeLogEvent::Refused
                && e.detail.contains("`DOM.setFileInputFiles`")),
            "the refusals are in its log"
        );
    }

    // ── who may use it: only a bot now ──────────────────────────────────────
    set(&node_paths, |c| c.scope.bots = Some(vec!["sweeper".into()]));
    wait_for("the server to hear who may use it", async || {
        machines(&engine)
            .await
            .first()
            .is_some_and(|m| m.scope.bots.is_some())
    })
    .await;
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "snapshot", "tab": board.id}),
    )
    .await;
    assert!(
        err && text.contains("runs on Desktop") && text.contains("only certain bots"),
        "{text}"
    );
    // The user's own devices still show it.
    let _: Value = app.call(attach_req(&board.id, false)).await.unwrap();
    set(&node_paths, |c| c.scope.bots = None);
    wait_for("any chat again", async || {
        machines(&engine)
            .await
            .first()
            .is_some_and(|m| m.scope.bots.is_none())
    })
    .await;

    // ── web views switched off on the desktop ───────────────────────────────
    set(&node_paths, |c| c.scope.webviews = false);
    wait_for("the desktop's browser to stop", async || {
        !browser_running(&profile)
    })
    .await;
    wait_for("the server to hear it", async || {
        machines(&engine)
            .await
            .first()
            .is_some_and(|m| !m.scope.webviews)
    })
    .await;
    let e = app.call(attach_req(&board.id, false)).await.unwrap_err();
    assert!(
        e.message.contains("web views are off on Desktop"),
        "{}",
        e.message
    );
    set(&node_paths, |c| c.scope.webviews = true);
    wait_for("web views on again", async || {
        machines(&engine)
            .await
            .first()
            .is_some_and(|m| m.scope.webviews)
    })
    .await;
    let _: Value = app.call(attach_req(&board.id, false)).await.unwrap();
    assert!(
        frame(&app, &mut app_events, &board.id).await.is_some(),
        "back"
    );

    // ── the desktop goes offline ────────────────────────────────────────────
    machine.stop.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), machine.task).await;
    machine.client.close();
    wait_for("the desktop to go offline", async || {
        machines(&engine).await.first().is_some_and(|m| !m.online)
    })
    .await;
    wait_for("its browser to stop with it", async || {
        !browser_running(&profile)
    })
    .await;
    let e = phone.call(attach_req(&board.id, true)).await.unwrap_err();
    assert!(e.message.contains("Desktop is offline"), "{}", e.message);
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "snapshot", "tab": board.id}),
    )
    .await;
    assert!(err && text.contains("Desktop is offline"), "{text}");
    // Opened on the server instead: it shows there.
    let moved: Whiteboard = phone
        .call_as(Request::BrowserToServer {
            board_id: board.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(moved.host, None);
    let _: Value = phone.call(attach_req(&board.id, true)).await.unwrap();
    assert!(
        frame(&phone, &mut phone_events, &board.id).await.is_some(),
        "the server's browser shows it now"
    );
    let (err, text) = browse(
        &engine,
        &mock,
        &sid,
        json!({"action": "snapshot", "tab": board.id}),
    )
    .await;
    assert!(!err && text.contains("Welcome"), "{text}");
    engine.shutdown().await;
}
