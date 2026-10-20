//! A machine (`tessera node`) on its own, driven by a fake server: it must
//! enforce its scope itself whatever the server sends (who may use it, when
//! it asks first, desktop control), stop running work at once when turned
//! off, paused, revoked, narrowed or disconnected, and log every action.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::node::{self, Exit, Link, LinkEvent, Node, NodeConfig};
use tessera_proto::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Answers the node like a server would and records what it sends.
struct FakeServer {
    replies: mpsc::UnboundedSender<(String, NodeResult)>,
    progress: Mutex<Vec<(String, String)>>,
    hellos: Mutex<Vec<Request>>,
    scopes: Mutex<Vec<MachineScope>>,
    /// Answer hellos as if this machine was revoked.
    revoked: std::sync::atomic::AtomicBool,
    /// Answer like a server older than "only risky ones" (it can't read them).
    old: std::sync::atomic::AtomicBool,
    answered: Mutex<Vec<String>>,
}

impl FakeServer {
    fn replies_sent_for(&self, id: &str) -> Option<()> {
        self.answered.lock().iter().any(|a| a == id).then_some(())
    }
}

impl Link for FakeServer {
    fn call(&self, req: Request) -> BoxFuture<'static, Result<Value, ApiError>> {
        let answer = match req {
            Request::NodeHello { ref scope, .. }
                if self.old.load(std::sync::atomic::Ordering::SeqCst)
                    && scope.ask == MachineAsk::Risky =>
            {
                self.hellos.lock().push(req);
                Err(ApiError::new(
                    ErrorCode::BadRequest,
                    "unknown variant `risky`, expected `always` or `chat`",
                ))
            }
            Request::NodeHello { .. } => {
                self.hellos.lock().push(req);
                if self.revoked.load(std::sync::atomic::Ordering::SeqCst) {
                    Err(ApiError::new(ErrorCode::Unauthorized, "revoked"))
                } else {
                    Ok(json!({"machine_id": "dev1", "paused": false, "server": "Home"}))
                }
            }
            Request::NodeReply { call_id, result } => {
                self.answered.lock().push(call_id.clone());
                let _ = self.replies.send((call_id, result));
                Ok(Value::Null)
            }
            Request::NodeProgress { call_id, chunk, .. } => {
                self.progress.lock().push((call_id, chunk));
                Ok(Value::Null)
            }
            Request::NodeScope { scope, .. } => {
                self.scopes.lock().push(scope);
                Ok(Value::Null)
            }
            _ => Err(ApiError::new(ErrorCode::BadRequest, "unexpected")),
        };
        Box::pin(async move { answer })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    paths: Paths,
    work: std::path::PathBuf,
    docs: std::path::PathBuf,
    outside: std::path::PathBuf,
    server: Arc<FakeServer>,
    replies: mpsc::UnboundedReceiver<(String, NodeResult)>,
    events: mpsc::UnboundedSender<LinkEvent>,
    task: tokio::task::JoinHandle<Exit>,
    stop: CancellationToken,
    n: u32,
    /// The bot the next calls come from.
    bot: Option<NodeBot>,
    /// The Stop count the next calls were asked for at (as a server says).
    stops: Option<u32>,
    /// The next calls come from a bot that may only read files here.
    files_only: bool,
    #[cfg(unix)]
    root: PathBuf,
}

fn config(paths: &Paths, f: impl FnOnce(&mut NodeConfig)) {
    let mut c = NodeConfig::load(paths).unwrap();
    f(&mut c);
    c.save(paths).unwrap();
}

async fn rig() -> Rig {
    rig_with(|_| None, None).await
}

/// A rig whose node has a stand-in home folder (`home` of its root) and
/// desktop session (never the user's own).
async fn rig_with(
    home: impl FnOnce(&std::path::Path) -> Option<PathBuf>,
    session: Option<HashMap<String, String>>,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths = Paths::at(root.join("node-home"));
    paths.ensure().unwrap();
    let (work, docs, outside) = (root.join("work"), root.join("docs"), root.join("outside"));
    for d in [&work, &docs, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(docs.join("readme.txt"), "hello from docs\n").unwrap();
    std::fs::write(outside.join("private.txt"), "not for agents").unwrap();
    std::fs::write(paths.data_dir.join("secret-marker"), "node-home-secret").unwrap();
    config(&paths, |c| {
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
    let home = home(&root);
    let (rtx, replies) = mpsc::unbounded_channel();
    let server = Arc::new(FakeServer {
        replies: rtx,
        progress: Mutex::new(Vec::new()),
        hellos: Mutex::new(Vec::new()),
        scopes: Mutex::new(Vec::new()),
        revoked: Default::default(),
        old: Default::default(),
        answered: Mutex::new(Vec::new()),
    });
    let node = Node::open_with(paths.clone(), home, session).unwrap();
    let (events, rx) = mpsc::unbounded_channel();
    let stop = CancellationToken::new();
    let task = tokio::spawn(node.serve(server.clone() as Arc<dyn Link>, rx, stop.clone()));
    events
        .send(LinkEvent::Up(Some("http://server".into())))
        .unwrap();
    for _ in 0..100 {
        if !server.hellos.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Rig {
        _dir: dir,
        paths,
        work,
        docs,
        outside,
        server,
        replies,
        events,
        task,
        stop,
        n: 0,
        bot: None,
        stops: None,
        files_only: false,
        #[cfg(unix)]
        root,
    }
}

impl Rig {
    fn send(&mut self, action: NodeAction, mode: PermissionMode, approved: bool) -> String {
        self.n += 1;
        let call_id = format!("c{}", self.n);
        self.events
            .send(LinkEvent::Event(Event::NodeCall {
                call: NodeCall {
                    call_id: call_id.clone(),
                    action,
                    session_id: "s1".into(),
                    chat: "Build the app".into(),
                    mode,
                    approved,
                    bot: self.bot.clone(),
                    env: Default::default(),
                    stops: self.stops,
                    files_only: self.files_only,
                },
            }))
            .unwrap();
        call_id
    }

    async fn reply(&mut self, call_id: &str) -> NodeResult {
        loop {
            let (id, r) = tokio::time::timeout(Duration::from_secs(20), self.replies.recv())
                .await
                .expect("the machine answers")
                .unwrap();
            if id == call_id {
                return r;
            }
        }
    }

    async fn call(&mut self, action: NodeAction) -> NodeResult {
        let id = self.send(action, PermissionMode::Default, true);
        self.reply(&id).await
    }

    fn run(&self, command: &str) -> NodeAction {
        NodeAction::Run {
            command: command.into(),
            cwd: self.work.display().to_string(),
            timeout_secs: None,
            admin: false,
        }
    }
}

fn sandbox_ok() -> bool {
    if !tessera_core::sandbox::confines_writes() {
        eprintln!(
            "commands on machines need Landlock; sandbox here: {}",
            tessera_core::sandbox::describe()
        );
        return false;
    }
    true
}

/// A process with this exact command line is still running.
fn running(marker: &str) -> bool {
    std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .map(|c| String::from_utf8_lossy(&c).replace('\0', " ").trim() == marker)
            .unwrap_or(false)
    })
}

async fn gone(marker: &str) -> bool {
    for _ in 0..50 {
        if !running(marker) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn the_machine_enforces_its_own_scope() {
    let mut r = rig().await;
    let hello = r.server.hellos.lock()[0].clone();
    let Request::NodeHello {
        name,
        generation,
        scope,
        ..
    } = hello
    else {
        unreachable!()
    };
    assert_eq!((name.as_str(), generation), ("Desktop", 1));
    assert_eq!(scope.folders.len(), 2, "it tells the server what it allows");

    // Refused before it was sent (e.g. outside the reported scope): only
    // recorded here, nothing runs and nothing is answered.
    r.events
        .send(LinkEvent::Event(Event::NodeRefused {
            call: NodeCall {
                call_id: "pre".into(),
                action: NodeAction::Read {
                    path: r.outside.join("private.txt").display().to_string(),
                    offset: None,
                    limit: None,
                },
                session_id: "s1".into(),
                chat: "Build the app".into(),
                mode: PermissionMode::Default,
                approved: false,
                bot: None,
                env: Default::default(),
                stops: None,
                files_only: false,
            },
            reason: "outside the folders it allows".into(),
        }))
        .unwrap();

    // Reads inside the scope work; everything around it is refused.
    let ok = r
        .call(NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        })
        .await;
    assert!(
        ok.ok && ok.output.contains("hello from docs"),
        "{}",
        ok.output
    );
    for path in [
        r.outside.join("private.txt").display().to_string(),
        format!("{}/../outside/private.txt", r.work.display()),
        r.paths.config_dir.join("node.json").display().to_string(),
    ] {
        let res = r
            .call(NodeAction::Read {
                path,
                offset: None,
                limit: None,
            })
            .await;
        assert!(
            res.refused && !res.output.contains("not for agents"),
            "{}",
            res.output
        );
    }
    let res = r
        .call(NodeAction::Write {
            path: r.docs.join("x.txt").display().to_string(),
            content: "x".into(),
        })
        .await;
    assert!(res.refused, "read-only folder: {}", res.output);

    // Whatever the server says, nothing runs unapproved while the machine
    // asks for everything, and a read-only chat changes nothing.
    let target = r.work.join("a.txt").display().to_string();
    let id = r.send(
        NodeAction::Write {
            path: target.clone(),
            content: "a".into(),
        },
        PermissionMode::Auto,
        false,
    );
    let res = r.reply(&id).await;
    assert!(
        res.refused && res.output.contains("asks before every action"),
        "{}",
        res.output
    );
    let id = r.send(
        NodeAction::Write {
            path: target.clone(),
            content: "a".into(),
        },
        PermissionMode::ReadOnly,
        true,
    );
    let res = r.reply(&id).await;
    assert!(
        res.refused && res.output.contains("read-only"),
        "{}",
        res.output
    );
    assert!(!r.work.join("a.txt").exists());
    let res = r
        .call(NodeAction::Write {
            path: target.clone(),
            content: "a\n".into(),
        })
        .await;
    assert!(res.ok, "{}", res.output);

    // "chat" mode: the chat's mode decides, the approval flag isn't needed.
    config(&r.paths, |c| c.scope.ask = MachineAsk::Chat);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        r.server
            .scopes
            .lock()
            .last()
            .is_some_and(|s| s.ask == MachineAsk::Chat),
        "local changes reach the server"
    );
    let id = r.send(
        NodeAction::Edit {
            path: target.clone(),
            old: "a".into(),
            new: "b".into(),
            all: false,
        },
        PermissionMode::Auto,
        false,
    );
    let res = r.reply(&id).await;
    assert!(res.ok, "{}", res.output);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "b\n");

    if sandbox_ok() {
        // Commands run, change only read-write folders, and never see
        // Tessera's own data on this computer.
        let res = r
            .call(r.run(&format!(
                "echo built > out.txt; cat out.txt; cat '{home}/secret-marker' 2>&1; ls -a '{home}'; echo x > '{docs}/y.txt' 2>&1 || echo docs-read-only",
                home = r.paths.data_dir.display(),
                docs = r.docs.display()
            )))
            .await;
        assert!(res.ok || res.exit_code.is_some(), "{}", res.output);
        assert!(res.output.contains("built"), "{}", res.output);
        assert!(!res.output.contains("node-home-secret"), "{}", res.output);
        assert!(!res.output.contains("node.json"), "{}", res.output);
        assert!(res.output.contains("docs-read-only"), "{}", res.output);
        assert!(r.work.join("out.txt").exists() && !r.docs.join("y.txt").exists());
        // Live output went out before the reply.
        assert!(
            r.server
                .progress
                .lock()
                .iter()
                .any(|(_, c)| c.contains("built"))
        );
        // A working folder outside the scope.
        let res = r
            .call(NodeAction::Run {
                command: "pwd".into(),
                cwd: format!("{}/../outside", r.work.display()),
                timeout_secs: None,
                admin: false,
            })
            .await;
        assert!(res.refused, "{}", res.output);

        // The time limit.
        let res = r
            .call(NodeAction::Run {
                command: "sleep 31.25".into(),
                cwd: r.work.display().to_string(),
                timeout_secs: Some(1),
                admin: false,
            })
            .await;
        assert!(
            !res.ok && res.output.contains("time limit"),
            "{}",
            res.output
        );
        assert!(gone("sleep 31.25").await);

        // Stopped from the chat.
        let id = r.send(r.run("sleep 32.25"), PermissionMode::Default, true);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(running("sleep 32.25"));
        r.events
            .send(LinkEvent::Event(Event::NodeCancel {
                call_id: id.clone(),
            }))
            .unwrap();
        let res = r.reply(&id).await;
        assert!(
            res.output.contains("stopped from the chat"),
            "{}",
            res.output
        );
        assert!(gone("sleep 32.25").await, "the command is killed");

        // Paused from another device: what runs stops, nothing new starts.
        let id = r.send(r.run("sleep 33.25"), PermissionMode::Default, true);
        tokio::time::sleep(Duration::from_millis(600)).await;
        r.events
            .send(LinkEvent::Event(Event::NodeControl {
                paused: true,
                revoked: false,
            }))
            .unwrap();
        let res = r.reply(&id).await;
        assert!(res.output.contains("paused"), "{}", res.output);
        assert!(gone("sleep 33.25").await);
        let res = r.call(r.run("true")).await;
        assert!(
            res.refused && res.output.contains("paused"),
            "{}",
            res.output
        );
        r.events
            .send(LinkEvent::Event(Event::NodeControl {
                paused: false,
                revoked: false,
            }))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(r.call(r.run("true")).await.ok);

        // Narrowed here while running: commands off stops the command.
        let id = r.send(r.run("sleep 34.25"), PermissionMode::Default, true);
        tokio::time::sleep(Duration::from_millis(600)).await;
        config(&r.paths, |c| c.scope.commands = false);
        let res = r.reply(&id).await;
        assert!(res.output.contains("no longer allowed"), "{}", res.output);
        assert!(gone("sleep 34.25").await);
        let res = r.call(r.run("true")).await;
        assert!(
            res.refused && res.output.contains("commands are off"),
            "{}",
            res.output
        );
        config(&r.paths, |c| c.scope.commands = true);
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // A lost connection stops what nobody waits for any more.
        let id = r.send(r.run("sleep 35.25"), PermissionMode::Default, true);
        tokio::time::sleep(Duration::from_millis(600)).await;
        r.events.send(LinkEvent::Down(Some("gone".into()))).unwrap();
        let res = r.reply(&id).await;
        assert!(res.output.contains("connection"), "{}", res.output);
        assert!(gone("sleep 35.25").await);
        // And it says hello again when the connection is back.
        let before = r.server.hellos.lock().len();
        r.events.send(LinkEvent::Up(None)).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(r.server.hellos.lock().len(), before + 1);

        // Turned off here: the running command stops at once and the node exits.
        let id = r.send(r.run("sleep 36.25"), PermissionMode::Default, true);
        tokio::time::sleep(Duration::from_millis(600)).await;
        config(&r.paths, |c| c.enabled = false);
        let res = r.reply(&id).await;
        assert!(
            res.output.contains("turned off on this computer"),
            "{}",
            res.output
        );
        assert!(gone("sleep 36.25").await);
        let exit = tokio::time::timeout(Duration::from_secs(10), &mut r.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::Disabled);
    } else {
        // Without the sandbox commands are refused, never run unconfined.
        let res = r.call(r.run("echo should-not-run")).await;
        assert!(
            res.refused && !res.output.contains("should-not-run"),
            "{}",
            res.output
        );
        r.stop.cancel();
        let _ = (&mut r.task).await;
    }

    // Every action is in the local log.
    let log = node::log::NodeLog::read(&node::log_file(&r.paths), 500);
    assert!(
        log.iter().any(|l| l.call_id.as_deref() == Some("pre")
            && l.event == NodeLogEvent::Refused
            && l.detail.contains("before it reached this machine")),
        "{log:?}"
    );
    assert!(r.server.replies_sent_for("pre").is_none());
    let has = |e: NodeLogEvent| log.iter().any(|l| l.event == e);
    assert!(has(NodeLogEvent::Connected) && has(NodeLogEvent::Started) && has(NodeLogEvent::Done));
    assert!(has(NodeLogEvent::Refused));
    let refused = log
        .iter()
        .find(|l| l.event == NodeLogEvent::Refused)
        .unwrap();
    assert_eq!(refused.chat.as_deref(), Some("Build the app"));
    if sandbox_ok() {
        assert!(
            has(NodeLogEvent::Cancelled)
                && has(NodeLogEvent::TimedOut)
                && has(NodeLogEvent::Paused)
        );
    }
}

#[tokio::test]
async fn a_revoked_machine_turns_itself_off_until_turned_on_here() {
    let mut r = rig().await;
    r.events
        .send(LinkEvent::Event(Event::NodeControl {
            paused: false,
            revoked: true,
        }))
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), &mut r.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Revoked);
    let c = NodeConfig::load(&r.paths).unwrap();
    assert!(!c.enabled && c.revoked);
    assert!(Node::open(r.paths.clone()).is_err(), "it stays off");

    // Turned on again without a new generation (not by `tessera node
    // enable`): the server still refuses it, and it turns itself off.
    config(&r.paths, |c| {
        c.enabled = true;
        c.revoked = false;
    });
    r.server
        .revoked
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let node = Node::open(r.paths.clone()).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(node.serve(
        r.server.clone() as Arc<dyn Link>,
        rx,
        CancellationToken::new(),
    ));
    tx.send(LinkEvent::Up(None)).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Revoked);
    assert!(!NodeConfig::load(&r.paths).unwrap().enabled);
    let log = node::log::NodeLog::read(&node::log_file(&r.paths), 50);
    assert!(log.iter().any(|l| l.event == NodeLogEvent::Revoked));
}

fn bot(id: &str, handle: &str, name: &str) -> Option<NodeBot> {
    Some(NodeBot {
        id: id.into(),
        handle: handle.into(),
        name: name.into(),
    })
}

#[tokio::test]
async fn only_the_bots_it_allows_may_use_it() {
    let mut r = rig().await;
    config(&r.paths, |c| c.scope.bots = Some(vec!["sweeper".into()]));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let read = || NodeAction::Read {
        path: "/nonexistent-but-checked-later".into(),
        offset: None,
        limit: None,
    };
    // A plain chat (no bot), and another bot: refused before anything else.
    let id = r.send(read(), PermissionMode::Auto, true);
    let res = r.reply(&id).await;
    assert!(
        res.refused && res.output.contains("not a plain chat"),
        "{}",
        res.output
    );
    r.bot = bot("b2", "writer", "Writer");
    let id = r.send(read(), PermissionMode::Auto, true);
    let res = r.reply(&id).await;
    assert!(
        res.refused && res.output.contains("isn't among them"),
        "{}",
        res.output
    );
    // The sweeper (named here by its handle; its subagents and duties carry it too).
    r.bot = bot("b1", "sweeper", "Homelab sweeper");
    let res = r
        .call(NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        })
        .await;
    assert!(res.ok, "{}", res.output);
    let log = node::log::NodeLog::read(&node::log_file(&r.paths), 20);
    assert!(
        log.iter()
            .any(|l| l.bot.as_deref() == Some("Homelab sweeper")),
        "the log names the bot"
    );
}

#[tokio::test]
async fn only_risky_actions_need_an_approval_when_it_asks_for_those() {
    let mut r = rig().await;
    config(&r.paths, |c| c.scope.ask = MachineAsk::Risky);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let id = r.send(
        NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        },
        PermissionMode::Auto,
        false,
    );
    let res = r.reply(&id).await;
    assert!(res.ok, "an ordinary read runs unasked: {}", res.output);
    let push = r.run("git push origin main");
    let id = r.send(push, PermissionMode::Auto, false);
    let res = r.reply(&id).await;
    assert!(
        res.refused && res.output.contains("risky"),
        "a risky command needs the user's approval: {}",
        res.output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_control_changes_startup_files_with_a_copy_to_put_back() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().canonicalize().unwrap();
    // A stand-in desktop session: its Wayland socket and Hyprland folder.
    let _wl = std::os::unix::net::UnixListener::bind(rt.join("wayland-test")).unwrap();
    std::fs::create_dir_all(rt.join("hypr/sigtest")).unwrap();
    std::fs::write(rt.join("keyring-control"), "not for commands").unwrap();
    let session: HashMap<String, String> = [
        ("XDG_RUNTIME_DIR", rt.display().to_string()),
        ("WAYLAND_DISPLAY", "wayland-test".to_string()),
        ("HYPRLAND_INSTANCE_SIGNATURE", "sigtest".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let mut r = rig_with(
        |root| {
            let home = root.join("me");
            std::fs::create_dir_all(home.join(".config/hypr")).unwrap();
            std::fs::write(home.join(".config/hypr/hyprland.conf"), "gaps_in = 5\n").unwrap();
            std::fs::write(home.join(".bashrc"), "# mine\n").unwrap();
            Some(home)
        },
        Some(session),
    )
    .await;
    let home = r.root.join("me");
    let conf = home.join(".config/hypr/hyprland.conf");
    let full = |desktop: bool| MachineScope {
        folders: vec![FolderAccess {
            path: home.display().to_string(),
            write: true,
        }],
        commands: true,
        desktop,
        ..Default::default()
    };
    // Full access: startup files can't change, by file action or command.
    config(&r.paths, |c| c.scope = full(false));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let res = r
        .call(NodeAction::Edit {
            path: "~/.config/hypr/hyprland.conf".into(),
            old: "5".into(),
            new: "9".into(),
            all: false,
        })
        .await;
    assert!(
        res.refused && res.output.contains("desktop control"),
        "{}",
        res.output
    );
    if sandbox_ok() {
        let cmd = NodeAction::Run {
            command: "echo x >> .config/hypr/hyprland.conf; echo y > .bash_login; env".into(),
            cwd: home.display().to_string(),
            timeout_secs: None,
            admin: false,
        };
        let res = r.call(cmd).await;
        assert!(
            !res.output.contains("WAYLAND_DISPLAY"),
            "no desktop session without desktop control: {}",
            res.output
        );
        assert!(
            !home.join(".bash_login").exists(),
            "no new startup file either"
        );
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), "gaps_in = 5\n");
        // …while ordinary files in the home folder can: a new folder there
        // comes from a file action, and commands then work inside it.
        let res = r
            .call(NodeAction::Write {
                path: "~/code/a.txt".into(),
                content: "hi\n".into(),
            })
            .await;
        assert!(res.ok, "{}", res.output);
        let res = r
            .call(NodeAction::Run {
                command: "echo more >> code/a.txt && mkdir -p code/sub && cat code/a.txt".into(),
                cwd: home.display().to_string(),
                timeout_secs: None,
                admin: false,
            })
            .await;
        assert!(res.ok && res.output.contains("more"), "{}", res.output);
    }

    // Desktop control: they can, each kept first, and put back.
    config(&r.paths, |c| c.scope = full(true));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let res = r
        .call(NodeAction::Edit {
            path: "~/.config/hypr/hyprland.conf".into(),
            old: "5".into(),
            new: "9".into(),
            all: false,
        })
        .await;
    assert!(res.ok, "{}", res.output);
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), "gaps_in = 9\n");
    if sandbox_ok() {
        let res = r
            .call(NodeAction::Run {
                command: "echo 'bind = SUPER, T, exec, kitty' >> .config/hypr/hyprland.conf; env | grep -E '^(WAYLAND_DISPLAY|HYPRLAND_INSTANCE_SIGNATURE|XDG_RUNTIME_DIR)=' | sort; ls \"$XDG_RUNTIME_DIR\"".into(),
                cwd: home.display().to_string(),
                timeout_secs: None,
                admin: false,
            })
            .await;
        assert!(res.ok, "{}", res.output);
        assert!(
            res.output.contains("WAYLAND_DISPLAY=wayland-test")
                && res.output.contains("HYPRLAND_INSTANCE_SIGNATURE=sigtest"),
            "the session's variables: {}",
            res.output
        );
        assert!(
            res.output.contains("wayland-test") && res.output.contains("hypr"),
            "its sockets are visible: {}",
            res.output
        );
        assert!(
            !res.output.contains("keyring-control"),
            "nothing else in its runtime folder is: {}",
            res.output
        );
        assert!(std::fs::read_to_string(&conf).unwrap().contains("kitty"));
    }
    let backups = node::backups::Backups::new(node::backups::dir(&r.paths));
    let changes = backups.list(10);
    assert!(
        !changes.is_empty() && changes.iter().all(|c| c.path.ends_with("hyprland.conf")),
        "{changes:?}"
    );
    let first = changes.last().unwrap();
    assert!(first.by.contains("Build the app"), "{first:?}");
    backups
        .restore(first.id, &node::startup_paths(Some(&home)))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&conf).unwrap(),
        "gaps_in = 5\n",
        "put back as it was"
    );
    let log = node::log::NodeLog::read(&node::log_file(&r.paths), 50);
    assert!(log.iter().any(|l| l.event == NodeLogEvent::StartupChanged));
    // Tessera's own files stay out of reach even so.
    let res = r
        .call(NodeAction::Read {
            path: node::config_file(&r.paths).display().to_string(),
            offset: None,
            limit: None,
        })
        .await;
    assert!(res.refused, "{}", res.output);
}

#[tokio::test]
async fn an_older_server_is_told_every_action_instead_of_risky_ones() {
    let mut r = rig().await;
    config(&r.paths, |c| c.scope.ask = MachineAsk::Risky);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    r.server
        .old
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // It reconnects: the first hello is refused, the second gets through.
    r.events.send(LinkEvent::Down(None)).unwrap();
    r.events.send(LinkEvent::Up(None)).unwrap();
    for _ in 0..100 {
        if r.server.hellos.lock().len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let hellos = r.server.hellos.lock().clone();
    let asks: Vec<MachineAsk> = hellos
        .iter()
        .map(|h| match h {
            Request::NodeHello { scope, .. } => scope.ask,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        &asks[1..],
        &[MachineAsk::Risky, MachineAsk::Always],
        "the stricter word the older server knows"
    );
    // Still its own setting here: an unapproved everyday read runs.
    let id = r.send(
        NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        },
        PermissionMode::Auto,
        false,
    );
    assert!(r.reply(&id).await.ok);
}

/// Administrator (batch 17): off by default and set only on this computer.
/// On, an administrator command runs outside the sandbox; by default only
/// what the user approved runs (whatever the chat's mode), and switching it
/// off refuses the next one at once. The log says "run as administrator".
#[tokio::test]
async fn administrator_commands_run_only_as_this_computer_allows() {
    let mut r = rig().await;
    let admin = |r: &Rig, cmd: &str| NodeAction::Run {
        command: cmd.into(),
        cwd: r.work.display().to_string(),
        timeout_secs: None,
        admin: true,
    };
    let outside = r.outside.join("made-by-admin.txt");
    let write_outside = format!("echo root-work > '{}' && echo wrote", outside.display());
    // Off (the default): refused, nothing ran.
    let res = r.call(admin(&r, &write_outside)).await;
    assert!(
        res.refused && res.output.contains("administrator commands are off"),
        "{}",
        res.output
    );
    assert!(!outside.exists());

    // On, asking each time: an unapproved one is refused even in YOLO.
    config(&r.paths, |c| c.scope.admin = AdminAccess::Ask);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let id = r.send(admin(&r, &write_outside), PermissionMode::Auto, false);
    let res = r.reply(&id).await;
    assert!(
        res.refused
            && res
                .output
                .contains("asks before every administrator command"),
        "{}",
        res.output
    );
    assert!(!outside.exists());

    // Approved: it runs, outside the sandbox (it writes where commands can't).
    let res = r.call(admin(&r, &write_outside)).await;
    assert!(res.ok && res.output.contains("wrote"), "{}", res.output);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "root-work\n");
    // An ordinary command still can't (where the sandbox works here).
    if sandbox_ok() {
        let res = r
            .call(r.run(&format!(
                "echo x > '{}'",
                r.outside.join("plain.txt").display()
            )))
            .await;
        assert!(!res.ok, "{}", res.output);
        assert!(!r.outside.join("plain.txt").exists());
    }
    let log = node::log::NodeLog::read(&node::log_file(&r.paths), 50);
    assert!(
        log.iter()
            .any(|e| e.action.as_deref() == Some("run as administrator")
                && e.event == NodeLogEvent::Done),
        "{log:?}"
    );

    // Switched off here while one runs: it stops at once, and the next is refused.
    let id = r.send(admin(&r, "sleep 36.75"), PermissionMode::Default, true);
    tokio::time::sleep(Duration::from_millis(600)).await;
    config(&r.paths, |c| c.scope.admin = AdminAccess::Off);
    let res = r.reply(&id).await;
    assert!(res.output.contains("no longer allowed"), "{}", res.output);
    assert!(gone("sleep 36.75").await);
    let res = r.call(admin(&r, "echo again")).await;
    assert!(
        res.refused && res.output.contains("administrator commands are off"),
        "{}",
        res.output
    );
}

#[tokio::test]
async fn a_stop_here_refuses_everything_until_resumed_and_older_calls_never_run() {
    let mut r = rig().await;
    let read = NodeAction::Read {
        path: r.docs.join("readme.txt").display().to_string(),
        offset: None,
        limit: None,
    };
    assert!(r.call(read.clone()).await.ok);
    // A copy of the settings from before the Stop.
    let stale = NodeConfig::load(&r.paths).unwrap();
    // Stopped here (what the Stop bar and `tessera node stop` write).
    tessera_core::node::StopLatch::set(&r.paths, true, 0).unwrap();
    // Saving that older copy (a settings change) doesn't undo it.
    stale.save(&r.paths).unwrap();
    assert!(NodeConfig::load(&r.paths).unwrap().scope.stopped);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let res = r.call(read.clone()).await;
    assert!(
        res.refused && res.output.contains("stopped Tessera on this computer"),
        "{}",
        res.output
    );
    // The server was told.
    assert!(
        r.server
            .scopes
            .lock()
            .last()
            .is_some_and(|s| s.stopped && s.stops == 1)
    );
    // Resumed here: what was asked for before the Stop never runs; what is
    // asked for after it does.
    tessera_core::node::StopLatch::set(&r.paths, false, 0).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    r.stops = Some(0);
    let res = r.call(read.clone()).await;
    assert!(
        res.refused && res.output.contains("before the user's latest Stop"),
        "{}",
        res.output
    );
    r.stops = Some(1);
    let res = r.call(read).await;
    assert!(res.ok, "{}", res.output);
    let latch = tessera_core::node::StopLatch::load(&r.paths);
    assert!(!latch.stopped && latch.stops == 1);
    // Stopped and resumed again before the machine looked: what ran then
    // was ended, and it isn't left stopped.
    tessera_core::node::StopLatch::set(&r.paths, true, 0).unwrap();
    tessera_core::node::StopLatch::set(&r.paths, false, 0).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    r.stops = Some(2);
    let res = r
        .call(NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        })
        .await;
    assert!(res.ok, "{}", res.output);
    // Stopped and resumed from the app or `tessera node stop`/`resume`, and
    // a call from a turn before that arrives before the machine looked:
    // refused all the same (the saved count is newer than its own).
    tessera_core::node::StopLatch::set(&r.paths, true, 0).unwrap();
    tessera_core::node::StopLatch::set(&r.paths, false, 0).unwrap();
    let res = r
        .call(NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        })
        .await;
    assert!(
        res.refused && res.output.contains("before the user's latest Stop"),
        "{}",
        res.output
    );
    // A Stop file nobody can read stops it (a Stop is never lost).
    std::fs::write(tessera_core::node::stop_file(&r.paths), "{not json").unwrap();
    assert!(tessera_core::node::StopLatch::load(&r.paths).stopped);
}

/// A bot that may only read files here (its access is read-only) reads and
/// lists, and nothing else reaches the computer for it: not a command, not
/// a write, not the screen.
#[tokio::test]
async fn a_bot_that_may_only_read_files_only_reads_files() {
    let mut r = rig().await;
    r.files_only = true;
    let read = r
        .call(NodeAction::Read {
            path: r.docs.join("readme.txt").display().to_string(),
            offset: None,
            limit: None,
        })
        .await;
    assert!(read.ok, "{}", read.output);
    let run = r.call(r.run("echo hi")).await;
    assert!(
        run.refused && run.output.contains("may only read files and list folders"),
        "{}",
        run.output
    );
    let look = r
        .call(NodeAction::Desktop {
            desktop: DesktopOp::Screenshot { screen: None },
            look: false,
        })
        .await;
    assert!(
        look.refused && look.output.contains("may only read files and list folders"),
        "{}",
        look.output
    );
}
