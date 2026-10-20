//! The user's own terminals, end to end: a phone opens one over its end-to-
//! end encrypted connection to a real server, types and reads, resizes it
//! and sees how it ended; another device can't touch it; a machine opens one
//! only once that computer allows it; signing the phone out ends them all.
//! Off until turned on; never from a chat or a plain connection from the
//! network; and nothing typed or shown reaches the logs, the event bus, the
//! chats or the records. The shells run in a throwaway home folder.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::Value;
use tessera_client::{Client, ClientEvent, ConnState, ServerConfig};
use tessera_core::config::Paths;
use tessera_core::node::{Link, LinkEvent, Node, NodeConfig};
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;
use tokio_util::sync::CancellationToken;

/// Everything logged while the test runs, at every level we write.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;
    fn make_writer(&'a self) -> Logs {
        self.clone()
    }
}

struct ClientLink(Client);

impl Link for ClientLink {
    fn call(&self, req: Request) -> BoxFuture<'static, Result<Value, ApiError>> {
        let c = self.0.clone();
        Box::pin(async move { c.call_timeout(req, Duration::from_secs(30)).await })
    }
}

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

async fn wait_for(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..300 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A device's view of its terminals: output collected (and acknowledged,
/// as a tile does once it drew it) per terminal, and how each ended.
#[derive(Default)]
struct Screen {
    text: Mutex<std::collections::HashMap<String, String>>,
    ended: Mutex<std::collections::HashMap<String, (Option<i32>, String)>>,
}

fn watch(client: &Client) -> Arc<Screen> {
    let screen = Arc::new(Screen::default());
    let mut rx = client.subscribe();
    let (s, c) = (screen.clone(), client.clone());
    tokio::spawn(async move {
        use base64::Engine as _;
        while let Ok(ev) = rx.recv().await {
            match ev {
                ClientEvent::Event(Event::TerminalOutput { id, seq, data }) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&data)
                        .unwrap();
                    s.text
                        .lock()
                        .entry(id.clone())
                        .or_default()
                        .push_str(&String::from_utf8_lossy(&bytes));
                    let upto = seq + bytes.len() as u64;
                    let c = c.clone();
                    tokio::spawn(async move {
                        let _ = c.call(Request::TerminalAck { id, upto }).await;
                    });
                }
                ClientEvent::Event(Event::TerminalExit { id, code, reason }) => {
                    s.ended.lock().insert(id, (code, reason));
                }
                _ => {}
            }
        }
    });
    screen
}

impl Screen {
    async fn shows(&self, id: &str, want: &str) {
        wait_for(&format!("{want:?} in terminal {id}"), async || {
            self.text.lock().get(id).is_some_and(|t| t.contains(want))
        })
        .await;
    }
    async fn ended(&self, id: &str) -> (Option<i32>, String) {
        wait_for(&format!("terminal {id} to end"), async || {
            self.ended.lock().contains_key(id)
        })
        .await;
        self.ended.lock()[id].clone()
    }
}

async fn open(client: &Client, machine: Option<&str>) -> Result<TerminalInfo, ApiError> {
    let v = client
        .call(Request::TerminalOpen {
            machine: machine.map(str::to_string),
            cols: 80,
            rows: 24,
        })
        .await?;
    Ok(serde_json::from_value(v).unwrap())
}

async fn typed(client: &Client, id: &str, text: &str) {
    client
        .call(Request::TerminalInput {
            id: id.into(),
            data: text.into(),
        })
        .await
        .unwrap();
}

#[test]
fn a_phone_opens_terminals_on_the_server_and_a_machine() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    // The shells' home (their profile, their history): never the user's own.
    let home = root_path.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: set before any other thread of this test binary starts.
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let logs = Logs::default();
    tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .init();
    // Whatever happens, the test (and with it every shell it opened, which
    // its exit hangs up) ends within four minutes: a hang must never hold
    // a machine's build lock.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(240));
        eprintln!("the terminal scenario still runs after 4 minutes; stopping it");
        std::process::exit(101);
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(scenario(root_path, logs));
}

async fn scenario(root: PathBuf, logs: Logs) {
    let server_dir = root.join("server-home");
    std::fs::create_dir_all(&server_dir).unwrap();
    std::fs::write(
        server_dir.join("config.toml"),
        "[server]\nname = \"Home server\"\n",
    )
    .unwrap();
    let engine = Engine::open(Paths::at(&server_dir), EngineOptions::default())
        .await
        .unwrap();
    std::fs::write(
        engine.paths().data_dir.join("secret-marker"),
        "server-secret-data",
    )
    .unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
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
    let (phone_id, phone_token) = engine.store().add_device("Phone").unwrap();
    let (_, laptop_token) = engine.store().add_device("Laptop").unwrap();
    let (desktop_id, desktop_token) = engine.store().add_device("Desktop").unwrap();
    let mut bus = engine.subscribe();
    let bus_seen: Arc<Mutex<Vec<String>>> = Default::default();
    {
        let seen = bus_seen.clone();
        tokio::spawn(async move {
            while let Ok(ev) = bus.recv().await {
                seen.lock().push(serde_json::to_string(&ev).unwrap());
            }
        });
    }

    let phone = Client::connect(login(phone_token));
    phone.wait_connected(Duration::from_secs(10)).await.unwrap();
    let screen = watch(&phone);
    let laptop = Client::connect(login(laptop_token));
    laptop
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();

    // ── off until turned on ─────────────────────────────────────────────
    let st: TerminalStatus =
        serde_json::from_value(phone.call(Request::TerminalStatus).await.unwrap()).unwrap();
    assert_eq!(st.settings.access, TerminalAccess::Off, "off by default");
    assert!(st.private, "the app's connection is end-to-end encrypted");
    let e = open(&phone, None).await.unwrap_err();
    assert!(
        e.message.contains("terminals are off on this server"),
        "{e:?}"
    );

    // Never from anything but a device's own connection (chats, tools, the
    // in-process backend), whatever the setting.
    for device in [None, Some(phone_id.as_str())] {
        let e = engine
            .handle(
                Request::TerminalOpen {
                    machine: None,
                    cols: 80,
                    rows: 24,
                },
                device,
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unauthorized, "{e:?}");
    }
    // The agent's settings tool can't turn them on.
    assert!(
        tessera_core::tools::settings::refusal(&["terminal".into(), "access".into()]).is_some()
    );

    // Turned on from a device: full access (this machine's sandbox is
    // checked further down where it is available).
    phone
        .call(Request::SetTerminalSettings {
            settings: TerminalSettings {
                access: TerminalAccess::Full,
                idle_minutes: 30,
            },
        })
        .await
        .unwrap();
    // A plain connection from the network: refused even now.
    let (plain, _rx) = engine.terminal_conn(&phone_id, false);
    let e = engine
        .terminal_request(
            Request::TerminalOpen {
                machine: None,
                cols: 80,
                rows: 24,
            },
            &plain,
        )
        .await
        .unwrap_err();
    assert!(e.message.contains("end-to-end encrypted"), "{e:?}");

    // ── a terminal on the server ────────────────────────────────────────
    let t = open(&phone, None).await.unwrap();
    assert_eq!(
        (t.place.as_str(), t.machine.as_deref()),
        ("Home server", None)
    );
    // Not even typing into it over a plain connection of the same device.
    let e = engine
        .terminal_request(
            Request::TerminalInput {
                id: t.id.clone(),
                data: "echo PLAIN\r".into(),
            },
            &plain,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unauthorized, "{e:?}");
    typed(&phone, &t.id, "echo MARK$((40+2))KEY\r").await;
    screen.shows(&t.id, "MARK42KEY").await;
    phone
        .call(Request::TerminalResize {
            id: t.id.clone(),
            cols: 100,
            rows: 30,
        })
        .await
        .unwrap();
    typed(&phone, &t.id, "stty size\r").await;
    screen.shows(&t.id, "30 100").await;
    // In the throwaway home, with Tessera's own variables kept back.
    typed(&phone, &t.id, "echo \"at:$PWD:${TESSERA_HOME:-none}\"\r").await;
    screen
        .shows(&t.id, &format!("at:{}:none", root.join("home").display()))
        .await;

    // Another of the user's devices can't see, type in, show or close it.
    for req in [
        Request::TerminalAttach {
            id: t.id.clone(),
            cols: 80,
            rows: 24,
        },
        Request::TerminalInput {
            id: t.id.clone(),
            data: "echo NOT-HERE\r".into(),
        },
        Request::TerminalClose { id: t.id.clone() },
    ] {
        let e = laptop.call(req).await.unwrap_err();
        assert!(e.message.contains("isn't open"), "{e:?}");
    }
    let lst: TerminalStatus =
        serde_json::from_value(laptop.call(Request::TerminalStatus).await.unwrap()).unwrap();
    assert!(lst.open.is_empty());
    let pst: TerminalStatus =
        serde_json::from_value(phone.call(Request::TerminalStatus).await.unwrap()).unwrap();
    assert_eq!(pst.open.len(), 1);

    typed(&phone, &t.id, "exit 7\r").await;
    let (code, reason) = screen.ended(&t.id).await;
    assert_eq!((code, reason.as_str()), (Some(7), "exited with code 7"));
    let shown = screen.text.lock()[&t.id].clone();
    assert!(!shown.contains("NOT-HERE") && !shown.contains("PLAIN"));

    // Sandboxed, where this computer's sandbox can: Tessera's data is empty.
    let sandboxed = phone
        .call(Request::SetTerminalSettings {
            settings: TerminalSettings {
                access: TerminalAccess::Sandboxed,
                idle_minutes: 30,
            },
        })
        .await;
    match sandboxed {
        Ok(_) => {
            let s = open(&phone, None).await.unwrap();
            assert!(s.sandboxed);
            let marker = engine.paths().data_dir.join("secret-marker");
            typed(
                &phone,
                &s.id,
                &format!("cat {} ; echo END-$((1+1))\r", marker.display()),
            )
            .await;
            screen.shows(&s.id, "END-2").await;
            assert!(
                !screen.text.lock()[&s.id].contains("server-secret-data"),
                "a sandboxed terminal can't read Tessera's data"
            );
            phone
                .call(Request::TerminalClose { id: s.id.clone() })
                .await
                .unwrap();
            assert_eq!(screen.ended(&s.id).await.1, "closed");
        }
        Err(e) => eprintln!(
            "no sandboxed terminals here ({}): {}",
            tessera_core::sandbox::describe(),
            e.message
        ),
    }
    phone
        .call(Request::SetTerminalSettings {
            settings: TerminalSettings {
                access: TerminalAccess::Full,
                idle_minutes: 30,
            },
        })
        .await
        .unwrap();

    // The strict sandbox chosen in Settings → Privacy & safety: full-access
    // terminals end at once, and no new one opens.
    let full = open(&phone, None).await.unwrap();
    assert!(!full.sandboxed);
    screen.shows(&full.id, "").await;
    let security = |mode: &str| -> Request {
        serde_json::from_value(serde_json::json!({
            "method": "set_security",
            "params": { "sandbox": mode },
        }))
        .unwrap()
    };
    phone.call(security("strict")).await.unwrap();
    assert_eq!(
        screen.ended(&full.id).await.1,
        "this server runs everything in the sandbox now"
    );
    let e = open(&phone, None).await.unwrap_err();
    assert!(e.message.contains("strict"), "{e:?}");
    phone.call(security("auto")).await.unwrap();

    // ── a machine decides for itself ────────────────────────────────────
    let node_paths = Paths::at(root.join("desktop-home"));
    node_paths.ensure().unwrap();
    let set = |f: &dyn Fn(&mut NodeConfig)| {
        let mut c = NodeConfig::load(&node_paths).unwrap();
        f(&mut c);
        c.save(&node_paths).unwrap();
    };
    set(&|c| {
        c.enabled = true;
        c.name = "Desktop".into();
        c.generation = 1;
    });
    let node = Node::open_at(node_paths.clone(), Some(root.join("home"))).unwrap();
    let node_client = Client::connect(login(desktop_token));
    let events = pump(&node_client);
    let _node = tokio::spawn(node.serve(
        Arc::new(ClientLink(node_client.clone())),
        events,
        CancellationToken::new(),
    ));
    let machine = |want: TerminalAccess| {
        let engine = engine.clone();
        async move {
            let v = engine.handle(Request::ListMachines, None).await.unwrap();
            let list: Vec<MachineInfo> = serde_json::from_value(v).unwrap();
            list.first()
                .is_some_and(|m| m.online && m.scope.terminal == want)
        }
    };
    wait_for("the machine online", async || {
        machine(TerminalAccess::Off).await
    })
    .await;
    let e = open(&phone, Some(&desktop_id)).await.unwrap_err();
    assert!(e.message.contains("terminals are off on Desktop"), "{e:?}");
    // Turned on there (only that computer can).
    set(&|c| c.scope.terminal = TerminalAccess::Full);
    wait_for("terminals on, on Desktop", async || {
        machine(TerminalAccess::Full).await
    })
    .await;
    let m = open(&phone, Some(&desktop_id)).await.unwrap();
    assert_eq!(
        (m.place.as_str(), m.machine.as_deref()),
        ("Desktop", Some(desktop_id.as_str()))
    );
    typed(&phone, &m.id, "echo HOST$((2+3))\r").await;
    screen.shows(&m.id, "HOST5").await;
    let e = laptop
        .call(Request::TerminalInput {
            id: m.id.clone(),
            data: "echo NOT-HERE\r".into(),
        })
        .await
        .unwrap_err();
    assert!(e.message.contains("isn't open"), "{e:?}");
    typed(&phone, &m.id, "exit 0\r").await;
    assert_eq!(
        screen.ended(&m.id).await,
        (Some(0), "exited with code 0".into())
    );

    // ── signed out: every terminal of the phone ends at once ────────────
    let a = open(&phone, None).await.unwrap();
    let b = open(&phone, Some(&desktop_id)).await.unwrap();
    screen.shows(&b.id, "").await;
    assert_eq!(engine.open_terminals(), 2);
    laptop
        .call(Request::RevokeDevice {
            id: phone_id.clone(),
        })
        .await
        .unwrap();
    wait_for("the phone's terminals to end", async || {
        engine.open_terminals() == 0
    })
    .await;
    let _ = a;
    let node_log = std::fs::read_to_string(node_paths.data_dir.join("node-log.jsonl")).unwrap();
    wait_for("the machine to close it too", async || {
        std::fs::read_to_string(node_paths.data_dir.join("node-log.jsonl"))
            .unwrap()
            .contains("its device was signed out")
    })
    .await;
    assert!(
        node_log.contains("terminal opened from Phone"),
        "{node_log}"
    );

    // ── nothing typed or shown went anywhere else ───────────────────────
    let _ = std::io::stdout().flush();
    let log = String::from_utf8_lossy(&logs.0.lock()).into_owned();
    let audit =
        std::fs::read_to_string(engine.paths().data_dir.join("terminal-audit.jsonl")).unwrap();
    let node_log = std::fs::read_to_string(node_paths.data_dir.join("node-log.jsonl")).unwrap();
    let bus = bus_seen.lock().join("\n");
    for secret in [
        "MARK42KEY",
        "MARK$((40+2))KEY",
        "HOST5",
        "HOST$((2+3))",
        "stty size",
        "30 100",
    ] {
        assert!(!log.contains(secret), "the logs have {secret:?}");
        assert!(!audit.contains(secret), "the record has {secret:?}");
        assert!(
            !node_log.contains(secret),
            "the machine's log has {secret:?}"
        );
        assert!(!bus.contains(secret), "the event bus carried {secret:?}");
    }
    assert!(
        log.contains("terminal") && log.contains("opened"),
        "the opens are logged (ids only)"
    );
    assert!(
        !bus.contains("terminal_output"),
        "terminal output never goes on the event bus"
    );
    assert!(
        audit.contains("\"device\":\"Phone\"") && audit.contains("Desktop"),
        "{audit}"
    );
    assert!(audit.contains("its device was signed out"), "{audit}");
    let sessions = engine
        .handle(Request::ListSessions { archived: false }, None)
        .await
        .unwrap();
    assert_eq!(
        sessions.as_array().map(Vec::len),
        Some(0),
        "no chat was made"
    );
}
