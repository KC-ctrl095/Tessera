//! Machines end to end: a server, a computer running the node (its own
//! Tessera home and paired device, end-to-end encrypted over a real socket)
//! and a phone, with a scripted model driving the `machines` tool.
//! Covers approvals in the chat, the machine's own refusals, commands off,
//! pause and revoke from another device, cancelling, time limits, turning
//! it off on the machine, Tessera's data hidden from commands, and the node
//! finding its way back after the server restarts.

#[path = "../../core/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{Mock, Reply, tool_results};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_client::{Client, ClientEvent, ConnState, ServerConfig};
use tessera_core::config::Paths;
use tessera_core::node::{Exit, Link, LinkEvent, Node, NodeConfig};
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;
use tokio_util::sync::CancellationToken;

/// A server in its own runtime, so it can be killed like a process.
struct Server {
    rt: Option<tokio::runtime::Runtime>,
    engine: Engine,
}

impl Server {
    fn start(dir: PathBuf, port: u16) -> Self {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let engine = rt
                .block_on(Engine::open(Paths::at(&dir), EngineOptions::default()))
                .unwrap();
            let app = tessera_server::router(engine.clone(), None).unwrap();
            let listener = rt
                .block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))
                .unwrap();
            rt.spawn(async move {
                let _ = tessera_server::serve_on(listener, app).await;
            });
            Server {
                rt: Some(rt),
                engine,
            }
        })
        .join()
        .unwrap()
    }

    /// Gone at once, connections and all.
    fn kill(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

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
}

fn start_machine(paths: &Paths, login: &ServerConfig) -> Machine {
    let node = Node::open(paths.clone()).unwrap();
    let client = Client::connect(login.clone());
    let events = pump(&client);
    let task = tokio::spawn(node.serve(
        Arc::new(ClientLink(client.clone())),
        events,
        CancellationToken::new(),
    ));
    Machine { task, client }
}

/// Approve (or decline) every approval the chat asks for, keeping them.
fn approver(
    engine: Engine,
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
    deny: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    let mut rx = engine.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Event::ApprovalRequested {
                session_id,
                request,
            } = ev
            {
                seen.lock().push(request.clone());
                let decision = if deny.load(Ordering::SeqCst) {
                    ApprovalDecision::Deny
                } else {
                    ApprovalDecision::Allow
                };
                let _ = engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision,
                        },
                        None,
                    )
                    .await;
            }
        }
    })
}

async fn machines(engine: &Engine) -> Vec<MachineInfo> {
    serde_json::from_value(engine.handle(Request::ListMachines, None).await.unwrap()).unwrap()
}

async fn wait_for(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn session(engine: &Engine, cwd: &Path) -> String {
    let v = engine
        .handle(
            Request::CreateSession {
                cwd: Some(cwd.display().to_string()),
                model: None,
                title: None,
                bot_id: None,
                private: false,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value::<SessionInfo>(v).unwrap().id
}

async fn send(engine: &Engine, sid: &str, text: &str) {
    engine
        .handle(
            Request::Send {
                session_id: sid.into(),
                content: vec![ContentPart::text(text)],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
}

async fn settle(engine: &Engine, sid: &str) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(40), engine.wait_idle(sid))
        .await
        .expect("the chat finishes");
}

/// The last `machines` result in the chat: (is_error, text).
async fn last_result(engine: &Engine, sid: &str) -> (bool, String) {
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
        .find(|(t, _, _)| t == "machines")
        .map(|(_, e, t)| (e, t))
        .expect("a machines result")
}

/// One turn in which the model calls `machines` with `args`.
async fn use_machine(engine: &Engine, mock: &Mock, sid: &str, args: Value) -> (bool, String) {
    mock.push(Reply::Tools(vec![("machines".into(), args)]));
    mock.push(Reply::Text("done".into()));
    send(engine, sid, "use the desktop").await;
    settle(engine, sid).await;
    last_result(engine, sid).await
}

fn running(marker: &str) -> bool {
    std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .map(|c| String::from_utf8_lossy(&c).replace('\0', " ").trim() == marker)
            .unwrap_or(false)
    })
}

fn set(paths: &Paths, f: impl FnOnce(&mut NodeConfig)) {
    let mut c = NodeConfig::load(paths).unwrap();
    f(&mut c);
    c.save(paths).unwrap();
}

#[test]
fn a_phone_uses_the_desktop_through_the_server() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(scenario());
}

async fn scenario() {
    let commands = tessera_core::sandbox::confines_writes();
    if !commands {
        eprintln!(
            "commands on machines need Landlock; sandbox here: {} (command steps skipped)",
            tessera_core::sandbox::describe()
        );
    }
    let mock = Mock::start(Vec::new()).await;
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
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
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = Server::start(server_dir.clone(), port);
    let url = format!("http://127.0.0.1:{port}");
    let key = tessera_secure::b64(
        tessera_server::load_identity(server.engine.paths())
            .unwrap()
            .public(),
    );
    let login = |token: String| ServerConfig {
        endpoints: vec![url.clone()],
        headers: Default::default(),
        token,
        server_key: key.clone(),
    };
    let (desktop_id, desktop_token) = server.engine.store().add_device("Desktop").unwrap();
    let (_, phone_token) = server.engine.store().add_device("Phone").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let deny = Arc::new(AtomicBool::new(false));
    let mut approving = approver(server.engine.clone(), seen.clone(), deny.clone());
    let chat_dir = root_path.join("server-chat");
    std::fs::create_dir_all(&chat_dir).unwrap();

    // The desktop: its own Tessera home, turned on with two folders.
    let node_paths = Paths::at(root_path.join("desktop-home"));
    node_paths.ensure().unwrap();
    std::fs::write(
        node_paths.data_dir.join("secret-marker"),
        "desktop-tessera-secret",
    )
    .unwrap();
    let work = root_path.join("desktop-code");
    let docs = root_path.join("desktop-docs");
    let outside = root_path.join("desktop-private");
    for d in [&work, &docs, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(outside.join("private.txt"), "not for agents").unwrap();
    set(&node_paths, |c| {
        c.enabled = true;
        c.name = "Desktop".into();
        c.generation = 1;
        c.scope = MachineScope {
            folders: vec![
                FolderAccess {
                    path: work.display().to_string(),
                    write: true,
                },
                FolderAccess {
                    path: docs.display().to_string(),
                    write: false,
                },
            ],
            commands: true,
            ask: MachineAsk::Always,
            timeout_secs: 60,
            ..Default::default()
        };
    });
    let machine = start_machine(&node_paths, &login(desktop_token.clone()));
    {
        let engine = server.engine.clone();
        wait_for("the machine to come online", async || {
            machines(&engine).await.first().is_some_and(|m| m.online)
        })
        .await;
    }
    // Once it is a machine, the node's connection hears nothing of the chats.
    let mut node_hears = machine.client.subscribe();

    // A phone, on its own connection.
    let phone = Client::connect(login(phone_token));
    phone.wait_connected(Duration::from_secs(10)).await.unwrap();
    let mut phone_events = phone.subscribe();
    let listed: Vec<MachineInfo> = phone.call_as(Request::ListMachines).await.unwrap();
    assert_eq!(listed.len(), 1);
    let m = &listed[0];
    assert_eq!(
        (m.id.as_str(), m.name.as_str()),
        (desktop_id.as_str(), "Desktop")
    );
    assert!(m.online && !m.paused && m.scope.commands && m.scope.folders.len() == 2);

    let engine = server.engine.clone();
    let sid = session(&engine, &chat_dir).await;
    let (err, text) = use_machine(&engine, &mock, &sid, json!({"action": "list"})).await;
    assert!(
        !err && text.contains("Desktop") && text.contains("online"),
        "{text}"
    );
    assert!(
        text.contains(&format!("read-write {}", work.display())),
        "{text}"
    );
    let offered = mock.agent_requests();
    let tools: Vec<String> = offered.last().unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(tools.contains(&"machines".to_string()), "{tools:?}");

    // Reading asks first (this machine asks for everything), naming it.
    std::fs::write(docs.join("readme.txt"), "desktop docs\n").unwrap();
    let (err, text) = use_machine(
        &engine,
        &mock,
        &sid,
        json!({"action": "read", "machine": "Desktop", "path": docs.join("readme.txt")}),
    )
    .await;
    assert!(!err && text.contains("desktop docs"), "{text}");
    {
        let seen = seen.lock();
        let card = seen.last().expect("an approval");
        assert_eq!(card.tool, "machines");
        assert!(card.summary.starts_with("on Desktop: "), "{}", card.summary);
        assert!(
            card.reason.contains("asks before every action"),
            "{}",
            card.reason
        );
        assert_eq!(card.arguments["machine"], "Desktop");
        assert_eq!(card.arguments["machine_id"], json!(desktop_id));
    }
    // Declined: nothing happens on the machine.
    deny.store(true, Ordering::SeqCst);
    let (err, text) = use_machine(
        &engine,
        &mock,
        &sid,
        json!({"action": "write", "path": work.join("declined.txt"), "content": "x"}),
    )
    .await;
    assert!(err && text.contains("declined"), "{text}");
    assert!(!work.join("declined.txt").exists());
    deny.store(false, Ordering::SeqCst);
    // Outside its folders: refused before asking anyone.
    let asked = seen.lock().len();
    let (err, text) = use_machine(
        &engine,
        &mock,
        &sid,
        json!({"action": "read", "path": outside.join("private.txt")}),
    )
    .await;
    assert!(
        err && text.contains("outside the folders") && !text.contains("not for agents"),
        "{text}"
    );
    assert_eq!(seen.lock().len(), asked);
    // Neither ran on the desktop, and both are in its own log anyway.
    {
        let log_file = tessera_core::node::log_file(&node_paths);
        wait_for("the refusals in the machine's log", async || {
            let log = tessera_core::node::log::NodeLog::read(&log_file, 200);
            let has = |what: &str, target: &str| {
                log.iter().any(|e| {
                    e.event == NodeLogEvent::Refused
                        && e.detail.contains(what)
                        && e.target.as_deref().is_some_and(|t| t.ends_with(target))
                })
            };
            has("declined this machines action", "declined.txt")
                && has("outside the folders", "private.txt")
        })
        .await;
    }
    // Through `..` it reaches the machine, which refuses it itself.
    let (err, text) = use_machine(
        &engine,
        &mock,
        &sid,
        json!({"action": "read", "path": format!("{}/../desktop-private/private.txt", work.display())}),
    )
    .await;
    assert!(
        err && text.contains("Desktop refused this") && !text.contains("not for agents"),
        "{text}"
    );
    let (err, text) = use_machine(
        &engine,
        &mock,
        &sid,
        json!({"action": "write", "path": work.join("src/lib.rs"), "content": "pub fn hi() {}\n"}),
    )
    .await;
    assert!(!err, "{text}");
    assert_eq!(
        std::fs::read_to_string(work.join("src/lib.rs")).unwrap(),
        "pub fn hi() {}\n"
    );

    if commands {
        // A build on the desktop: runs there, live output, approval names it.
        let (err, text) = use_machine(
            &engine,
            &mock,
            &sid,
            json!({"action": "run", "command": "echo built-on-desktop > out.txt && cat out.txt", "cwd": work}),
        )
        .await;
        assert!(
            !err && text.contains("built-on-desktop") && text.contains("[exit 0"),
            "{text}"
        );
        assert!(work.join("out.txt").exists());
        assert!(
            seen.lock()
                .last()
                .unwrap()
                .summary
                .starts_with("on Desktop: echo built-on-desktop")
        );
        // Tessera's own data on the desktop doesn't exist for commands, and
        // the read-only folder stays read-only.
        let (_, text) = use_machine(
            &engine,
            &mock,
            &sid,
            json!({"action": "run", "cwd": work, "command": format!(
                "cat '{home}/secret-marker' 2>&1; ls -a '{home}'; echo x > '{docs}/new.txt' 2>&1 || echo docs-stay-read-only",
                home = node_paths.data_dir.display(), docs = docs.display())}),
        )
        .await;
        assert!(
            !text.contains("desktop-tessera-secret") && !text.contains("node.json"),
            "{text}"
        );
        assert!(
            text.contains("docs-stay-read-only") && !docs.join("new.txt").exists(),
            "{text}"
        );
        // The time limit.
        let (err, text) = use_machine(
            &engine,
            &mock,
            &sid,
            json!({"action": "run", "command": "sleep 41.75", "cwd": work, "timeout": 1}),
        )
        .await;
        assert!(err && text.contains("time limit"), "{text}");
        // Stopped from the chat: the command dies on the desktop.
        mock.push(Reply::Tools(vec![(
            "machines".into(),
            json!({"action": "run", "command": "sleep 42.75", "cwd": work}),
        )]));
        mock.push(Reply::Text("done".into()));
        send(&engine, &sid, "a long one").await;
        wait_for("the command to start", async || running("sleep 42.75")).await;
        engine
            .handle(
                Request::Abort {
                    session_id: sid.clone(),
                },
                None,
            )
            .await
            .unwrap();
        settle(&engine, &sid).await;
        wait_for("the command to stop", async || !running("sleep 42.75")).await;
        let (_, text) = last_result(&engine, &sid).await;
        assert!(
            text.contains("stopped") || text.contains("Interrupted"),
            "{text}"
        );
        // The stopped turn never asked for its closing reply.
        mock.state.script.lock().clear();

        // Commands switched off on the desktop: the server hears of it and
        // refuses before asking.
        set(&node_paths, |c| c.scope.commands = false);
        wait_for("the new scope to arrive", async || {
            machines(&engine)
                .await
                .first()
                .is_some_and(|m| !m.scope.commands)
        })
        .await;
        let (err, text) = use_machine(
            &engine,
            &mock,
            &sid,
            json!({"action": "run", "command": "true", "cwd": work}),
        )
        .await;
        assert!(
            err && text.contains("Commands are off on Desktop"),
            "{text}"
        );
        set(&node_paths, |c| c.scope.commands = true);
        wait_for("commands back on", async || {
            machines(&engine)
                .await
                .first()
                .is_some_and(|m| m.scope.commands)
        })
        .await;
    }

    // Paused from the phone: refused until resumed.
    let paused: MachineInfo = phone
        .call_as(Request::SetMachinePaused {
            id: desktop_id.clone(),
            paused: true,
        })
        .await
        .unwrap();
    assert!(paused.paused);
    let (err, text) =
        use_machine(&engine, &mock, &sid, json!({"action": "ls", "path": work})).await;
    assert!(err && text.contains("paused"), "{text}");
    let _: MachineInfo = phone
        .call_as(Request::SetMachinePaused {
            id: desktop_id.clone(),
            paused: false,
        })
        .await
        .unwrap();
    let (err, text) =
        use_machine(&engine, &mock, &sid, json!({"action": "ls", "path": work})).await;
    assert!(!err && text.contains("src/"), "{text}");
    // The phone heard about all of it; the node's connection heard none of the chat.
    let mut phone_saw_machines = false;
    while let Ok(ev) = phone_events.try_recv() {
        if matches!(ev, ClientEvent::Event(Event::MachinesChanged)) {
            phone_saw_machines = true;
        }
    }
    assert!(phone_saw_machines);
    while let Ok(ev) = node_hears.try_recv() {
        if let ClientEvent::Event(e) = ev {
            assert!(
                matches!(
                    e,
                    Event::NodeCall { .. }
                        | Event::NodeCancel { .. }
                        | Event::NodeRefused { .. }
                        | Event::NodeControl { .. }
                ),
                "the node's connection got {e:?}"
            );
        }
    }

    // The server restarts: the desktop finds its way back by itself.
    approving.abort();
    server.kill();
    drop(engine);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut server = Server::start(server_dir.clone(), port);
    let engine = server.engine.clone();
    assert_eq!(machines(&engine).await.len(), 1, "machines are remembered");
    approving = approver(engine.clone(), seen.clone(), deny.clone());
    wait_for("the machine to reconnect", async || {
        machines(&engine).await.first().is_some_and(|m| m.online)
    })
    .await;
    let sid = session(&engine, &chat_dir).await;
    let (err, text) =
        use_machine(&engine, &mock, &sid, json!({"action": "ls", "path": work})).await;
    assert!(!err && text.contains("src/"), "{text}");

    if commands {
        // Turned off on the desktop while a command runs: it stops at once.
        mock.push(Reply::Tools(vec![(
            "machines".into(),
            json!({"action": "run", "command": "sleep 43.75", "cwd": work}),
        )]));
        mock.push(Reply::Text("done".into()));
        send(&engine, &sid, "another long one").await;
        wait_for("the command to start", async || running("sleep 43.75")).await;
        set(&node_paths, |c| c.enabled = false);
        settle(&engine, &sid).await;
        wait_for("the command to stop", async || !running("sleep 43.75")).await;
        let (err, text) = last_result(&engine, &sid).await;
        assert!(
            err && (text.contains("turned off") || text.contains("connection")),
            "{text}"
        );
        mock.state.script.lock().clear();
    } else {
        set(&node_paths, |c| c.enabled = false);
    }
    let exit = tokio::time::timeout(Duration::from_secs(10), machine.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Disabled);
    machine.client.close();
    wait_for("the machine to go offline", async || {
        machines(&engine).await.first().is_some_and(|m| !m.online)
    })
    .await;

    // On again (`tessera node enable`: a new generation), then revoked from
    // the phone: it turns itself off and only the desktop can undo that.
    set(&node_paths, |c| {
        c.enabled = true;
        c.generation += 1;
    });
    let machine = start_machine(&node_paths, &login(desktop_token.clone()));
    wait_for("the machine to come back", async || {
        machines(&engine).await.first().is_some_and(|m| m.online)
    })
    .await;
    phone
        .call(Request::RevokeMachine {
            id: desktop_id.clone(),
        })
        .await
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), machine.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Revoked);
    machine.client.close();
    let c = NodeConfig::load(&node_paths).unwrap();
    assert!(!c.enabled && c.revoked);
    assert!(machines(&engine).await.is_empty());
    // A node that comes back without being turned on again is refused.
    set(&node_paths, |c| {
        c.enabled = true;
        c.revoked = false;
    });
    let again = start_machine(&node_paths, &login(desktop_token.clone()));
    let exit = tokio::time::timeout(Duration::from_secs(15), again.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Revoked);
    again.client.close();
    assert!(machines(&engine).await.is_empty());
    // No machines: the tool isn't offered.
    let sid = session(&engine, &chat_dir).await;
    mock.push(Reply::Text("ok".into()));
    send(&engine, &sid, "hi").await;
    settle(&engine, &sid).await;
    let tools: Vec<String> = mock.agent_requests().last().unwrap()["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(!tools.contains(&"machines".to_string()), "{tools:?}");
    // Only the machine's own connection may speak for it.
    let err = phone
        .call(Request::NodeReply {
            call_id: "x".into(),
            result: NodeResult {
                output: String::new(),
                ok: true,
                refused: false,
                exit_code: None,
                image: None,
            },
        })
        .await
        .unwrap_err();
    assert!(err.message.contains("isn't a machine"), "{}", err.message);
    approving.abort();
    phone.close();
    server.kill();
}
