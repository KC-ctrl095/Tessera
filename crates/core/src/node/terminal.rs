//! Terminals on this machine ("Terminal from my devices"): one of the
//! user's paired devices opens a terminal here through the server. This
//! computer decides whether that is allowed (its own setting,
//! `MachineScope::terminal`, off unless turned on here), confines it the way
//! chosen here, and keeps its own record (who and when, in its log; never
//! what was typed or shown). Agents never get one: the server only relays a
//! device's own connection.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tessera_proto::{
    MachineScope, NodeLogEvent, NodeTermDown, NodeTermUp, Request, TerminalAccess,
};
use tokio::sync::mpsc;

use super::Link;
use super::log::{NodeLog, entry};
use crate::config::Paths;
use crate::terminal::{Closed, Host, Limits, Out, Sink};

/// The link's end for terminal output (one per connection to the server).
const CONN: u64 = 0;

pub struct NodeTerminals {
    host: Host,
    #[cfg(unix)]
    paths: Paths,
    #[cfg(unix)]
    home: Option<PathBuf>,
    /// Output on its way to the server while connected.
    sink: Mutex<Option<Sink>>,
    log: Arc<NodeLog>,
}

impl NodeTerminals {
    pub fn new(paths: Paths, home: Option<PathBuf>) -> Self {
        let log = Arc::new(NodeLog::new(super::log_file(&paths)));
        #[cfg(not(unix))]
        let _ = home;
        let on_close = {
            let log = log.clone();
            Arc::new(move |c: &Closed| {
                let mut e = entry(
                    NodeLogEvent::TerminalClosed,
                    format!(
                        "terminal from {} closed: {}",
                        crate::util::one_line(&c.owner_name, 80),
                        crate::util::one_line(&c.reason, 200)
                    ),
                );
                e.duration_ms = Some(crate::util::now_ms().saturating_sub(c.started_at));
                log.add(&e);
            })
        };
        Self {
            host: Host::new(Limits::default(), on_close),
            #[cfg(unix)]
            paths,
            #[cfg(unix)]
            home,
            sink: Mutex::new(None),
            log,
        }
    }

    /// Terminals open here now.
    pub fn count(&self) -> usize {
        self.host.count()
    }

    /// Connected (again): output goes to the server over `link`.
    pub fn connected(&self, link: Arc<dyn Link>) {
        let (tx, rx) = mpsc::channel::<Out>(256);
        tokio::spawn(forward(link, rx));
        *self.sink.lock() = Some(Sink { conn: CONN, tx });
    }

    /// Every terminal here ends (disconnected, paused, turned off…).
    pub fn close_all(&self, why: &str) {
        self.host.close_all(why);
    }

    pub fn disconnected(&self, why: &str) {
        self.close_all(why);
        *self.sink.lock() = None;
    }

    fn owner(&self, id: &str) -> Option<String> {
        self.host
            .list(None)
            .into_iter()
            .find(|i| i.id == id)
            .map(|i| i.owner)
    }

    /// The server's messages about terminals, in order.
    pub fn handle(
        &self,
        link: &Arc<dyn Link>,
        msgs: Vec<NodeTermDown>,
        scope: &MachineScope,
        paused: bool,
    ) {
        for m in msgs {
            match m {
                NodeTermDown::Open {
                    id,
                    device,
                    device_name,
                    cols,
                    rows,
                } => {
                    let up = match self.open(&id, &device, &device_name, cols, rows, scope, paused)
                    {
                        Ok((shell, sandboxed)) => NodeTermUp::Opened {
                            id,
                            shell,
                            sandboxed,
                        },
                        Err(error) => {
                            let e = entry(
                                NodeLogEvent::Refused,
                                format!(
                                    "terminal from {} not opened: {error}",
                                    crate::util::one_line(&device_name, 80)
                                ),
                            );
                            self.log.add(&e);
                            NodeTermUp::Failed { id, error }
                        }
                    };
                    let link = link.clone();
                    tokio::spawn(async move {
                        let _ = link.call(Request::NodeTerminal { msgs: vec![up] }).await;
                    });
                }
                // Typed in order, as the program reads it (a program that
                // doesn't read holds up only its own terminal; past 1 MB
                // waiting, the terminal says so and takes no more).
                NodeTermDown::Input { id, data } => {
                    if let Some(owner) = self.owner(&id) {
                        let _ = self.host.type_in(&id, &owner, data.as_bytes());
                    }
                }
                NodeTermDown::Resize { id, cols, rows } => {
                    if let Some(owner) = self.owner(&id) {
                        let _ = self.host.resize(&id, &owner, cols, rows);
                    }
                }
                NodeTermDown::Ack { id, upto } => {
                    if let Some(owner) = self.owner(&id) {
                        let _ = self.host.ack(&id, &owner, upto);
                    }
                }
                NodeTermDown::Attach { id } => {
                    let sink = self.sink.lock().clone();
                    if let (Some(owner), Some(sink)) = (self.owner(&id), sink) {
                        let _ = self.host.attach(&id, &owner, sink, None);
                    }
                }
                NodeTermDown::Detach { id } => {
                    if let Some(owner) = self.owner(&id) {
                        let _ = self.host.detach(&id, &owner);
                    }
                }
                NodeTermDown::Close { id } => {
                    if let Some(owner) = self.owner(&id) {
                        let _ = self.host.close(&id, &owner, "closed");
                    }
                }
                NodeTermDown::SignedOut { device } => {
                    self.host.close_owner(&device, "its device was signed out");
                }
            }
        }
    }

    /// Open one if this computer allows it: (the shell, sandboxed).
    #[allow(clippy::too_many_arguments)]
    fn open(
        &self,
        id: &str,
        device: &str,
        device_name: &str,
        cols: u16,
        rows: u16,
        scope: &MachineScope,
        paused: bool,
    ) -> Result<(String, bool), String> {
        if paused {
            return Err("this machine is paused from another device".into());
        }
        if scope.terminal == TerminalAccess::Off {
            let access = if cfg!(windows) { "full" } else { "sandboxed" };
            return Err(format!(
                "terminals are off on this machine; only this computer can turn them on (Settings → Devices → This computer, or `tessera node scope --terminal {access}`)"
            ));
        }
        if id.len() > 64 || device.is_empty() {
            return Err("that isn't a terminal".into());
        }
        let sink = self
            .sink
            .lock()
            .clone()
            .ok_or("this machine isn't connected")?;
        #[cfg(unix)]
        {
            let (account_home, shell) = crate::pty::account();
            let home = self.home.clone().or(account_home);
            let cwd = home
                .clone()
                .filter(|h| h.is_dir())
                .unwrap_or_else(|| PathBuf::from("/"));
            let sandbox = match scope.terminal {
                // Tessera's data hidden; its program and the startup files
                // never changed (as for an agent's commands here).
                TerminalAccess::Sandboxed => {
                    let spec = super::exec::terminal_spec(
                        &self.paths,
                        home.as_deref(),
                        format!("terminal-{id}"),
                    );
                    match crate::sandbox::prepare(Some(&(spec, true)), &cwd) {
                        Ok(Some(p)) => Some(p),
                        Ok(None) => None,
                        Err(e) => {
                            return Err(format!(
                                "the sandbox can't confine terminals on this computer ({e}); choose full access here, or leave terminals off"
                            ));
                        }
                    }
                }
                _ => None,
            };
            let sandboxed = sandbox.is_some();
            // A login shell that reads the user's own settings too (pty::shell_start).
            let (program, args) = crate::pty::shell_start(&shell);
            let info = self.host.open(
                crate::terminal::Opening {
                    id: id.to_string(),
                    owner: device.to_string(),
                    owner_name: device_name.to_string(),
                    place: "this machine".into(),
                    sandboxed,
                    start: crate::pty::Start {
                        program,
                        args,
                        cwd,
                        env: crate::pty::environment(home.as_deref(), &shell, sandboxed),
                        cols,
                        rows,
                        sandbox,
                    },
                },
                sink,
            )?;
            let e = entry(
                NodeLogEvent::TerminalOpened,
                format!(
                    "terminal opened from {} ({})",
                    crate::util::one_line(device_name, 80),
                    if sandboxed {
                        "sandboxed"
                    } else {
                        "full access"
                    }
                ),
            );
            self.log.add(&e);
            Ok((info.shell, sandboxed))
        }
        #[cfg(windows)]
        {
            if scope.terminal != TerminalAccess::Full {
                return Err("Windows terminals are not sandboxed; choose full access on this computer, or leave terminals off".into());
            }
            let account = crate::pty::account()
                .map_err(|e| format!("the terminal couldn't find this account's shell: {e}"))?;
            let (program, args) = account.shell_start();
            let info = self.host.open(
                crate::terminal::Opening {
                    id: id.to_string(),
                    owner: device.to_string(),
                    owner_name: device_name.to_string(),
                    place: "this machine".into(),
                    sandboxed: false,
                    start: crate::pty::Start {
                        program,
                        args,
                        cwd: account.home.clone(),
                        env: account.environment(),
                        cols,
                        rows,
                        sandbox: None,
                    },
                },
                sink,
            )?;
            self.log.add(&entry(
                NodeLogEvent::TerminalOpened,
                format!(
                    "terminal opened from {} (full access)",
                    crate::util::one_line(device_name, 80)
                ),
            ));
            Ok((info.shell, false))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (device_name, cols, rows, sink);
            Err("terminals aren't available on Windows machines yet".into())
        }
    }
}

/// Send terminal output to the server, in order, a batch per request.
async fn forward(link: Arc<dyn Link>, mut rx: mpsc::Receiver<Out>) {
    use base64::Engine as _;
    let up = |o: Out| match o {
        Out::Output { id, seq, data } => NodeTermUp::Output {
            id,
            seq,
            data: base64::engine::general_purpose::STANDARD.encode(data),
        },
        Out::Exit { id, code, reason } => NodeTermUp::Exit { id, code, reason },
    };
    while let Some(first) = rx.recv().await {
        let mut size = 0usize;
        let mut msgs = vec![up(first)];
        while size < 128 << 10
            && let Ok(more) = rx.try_recv()
        {
            if let Out::Output { data, .. } = &more {
                size += data.len();
            }
            msgs.push(up(more));
        }
        // An error: the connection is going, and its terminals end with it.
        let _ = link.call(Request::NodeTerminal { msgs }).await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use serde_json::Value;
    use tessera_proto::ApiError;

    /// Records what the machine tells the server about its terminals.
    #[derive(Default)]
    struct Server(Mutex<Vec<NodeTermUp>>);

    impl Link for Server {
        fn call(&self, req: Request) -> BoxFuture<'static, Result<Value, ApiError>> {
            if let Request::NodeTerminal { msgs } = req {
                self.0.lock().extend(msgs);
            }
            Box::pin(async { Ok(Value::Null) })
        }
    }

    impl Server {
        async fn wait(&self, what: &str, f: impl Fn(&[NodeTermUp]) -> bool) {
            for _ in 0..200 {
                // A copy: `f` may look at the server again.
                let seen = self.0.lock().clone();
                if f(&seen) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            panic!("{what}: {:?}", self.0.lock());
        }

        fn output(&self, id: &str) -> String {
            use base64::Engine as _;
            self.0
                .lock()
                .iter()
                .filter_map(|m| match m {
                    NodeTermUp::Output { id: i, data, .. } if i == id => {
                        base64::engine::general_purpose::STANDARD.decode(data).ok()
                    }
                    _ => None,
                })
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .collect()
        }
    }

    fn open(id: &str) -> NodeTermDown {
        NodeTermDown::Open {
            id: id.into(),
            device: "phone".into(),
            device_name: "Phone".into(),
            cols: 80,
            rows: 24,
        }
    }

    /// Whatever happened, nothing the test started outlives it.
    struct Closing<'a>(&'a NodeTerminals);

    impl Drop for Closing<'_> {
        fn drop(&mut self) {
            self.0.close_all("the test is over");
        }
    }

    #[tokio::test]
    async fn this_computer_decides_and_keeps_its_own_record() {
        let _dog = crate::terminal::watchdog("a machine terminal test", 60);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::at(root.join("tessera"));
        paths.ensure().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let terms = NodeTerminals::new(paths.clone(), Some(home));
        let _closing = Closing(&terms);
        let server = Arc::new(Server::default());
        let link: Arc<dyn Link> = server.clone();
        terms.connected(link.clone());
        let mut scope = MachineScope::default();

        // Off here (the default): refused, whatever the server thinks.
        terms.handle(&link, vec![open("t1")], &scope, false);
        server
            .wait("refused while off", |m| {
                m.iter().any(|x| matches!(x, NodeTermUp::Failed { id, error } if id == "t1" && error.contains("off on this machine")))
            })
            .await;
        // On, but paused from a device: refused too.
        scope.terminal = TerminalAccess::Full;
        terms.handle(&link, vec![open("t2")], &scope, true);
        server
            .wait("refused while paused", |m| {
                m.iter().any(|x| matches!(x, NodeTermUp::Failed { id, error } if id == "t2" && error.contains("paused")))
            })
            .await;
        assert_eq!(terms.count(), 0);

        // On: it opens, types, and ends when its device is signed out.
        terms.handle(&link, vec![open("t3")], &scope, false);
        server
            .wait("opened", |m| {
                m.iter()
                    .any(|x| matches!(x, NodeTermUp::Opened { id, .. } if id == "t3"))
            })
            .await;
        terms.handle(
            &link,
            vec![NodeTermDown::Input {
                id: "t3".into(),
                data: "echo node-$((20+1))\r".into(),
            }],
            &scope,
            false,
        );
        let s = server.clone();
        server
            .wait("output", move |_| s.output("t3").contains("node-21"))
            .await;
        terms.handle(
            &link,
            vec![NodeTermDown::SignedOut {
                device: "phone".into(),
            }],
            &scope,
            false,
        );
        server
            .wait("ended", |m| {
                m.iter().any(|x| matches!(x, NodeTermUp::Exit { id, reason, .. } if id == "t3" && reason.contains("signed out")))
            })
            .await;
        assert_eq!(terms.count(), 0);

        // Its own log: who and when, never what was typed or shown.
        let log = std::fs::read_to_string(super::super::log_file(&paths)).unwrap();
        assert!(log.contains("terminal opened from Phone"), "{log}");
        assert!(
            log.contains("terminal from Phone closed: its device was signed out"),
            "{log}"
        );
        assert!(log.contains("not opened: terminals are off"), "{log}");
        assert!(!log.contains("node-21") && !log.contains("20+1"), "{log}");
    }

    /// Sandboxed here: Tessera's data hidden, and its program and the
    /// startup files never changed (nothing new beside them either), as
    /// for an agent's commands; the rest of the account's files as usual.
    #[tokio::test]
    async fn a_sandboxed_terminal_never_changes_startup_files_or_tessera() {
        let _dog = crate::terminal::watchdog("a machine terminal test", 60);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::at(root.join("tessera"));
        paths.ensure().unwrap();
        let home = root.join("home");
        for d in [".config/systemd/user", ".config/other", "Documents"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join(".bashrc"), "# mine\n").unwrap();
        std::fs::write(home.join("notes.txt"), "notes\n").unwrap();

        // What it may change.
        let spec = super::super::exec::terminal_spec(&paths, Some(&home), "t".into());
        let writable = spec.writable.clone().unwrap();
        assert!(writable.contains(&home.join("Documents")), "{writable:?}");
        assert!(
            writable.contains(&home.join(".config/other")),
            "{writable:?}"
        );
        assert!(writable.contains(&home.join("notes.txt")), "{writable:?}");
        // Not the folders holding startup files as a whole (nothing new
        // beside those), nor the startup files and folders themselves.
        for never in [home.clone(), home.join(".config"), home.join(".bashrc")] {
            assert!(!writable.contains(&never), "{never:?} in {writable:?}");
        }
        let systemd = home.join(".config/systemd");
        assert!(
            !writable.iter().any(|w| w.starts_with(&systemd)),
            "{writable:?}"
        );
        let exe = crate::platform::canonicalize(&std::env::current_exe().unwrap()).unwrap();
        assert!(
            !writable.contains(&exe),
            "the Tessera program stays as it is"
        );
        assert!(spec.hide.contains(&paths.data_dir), "{:?}", spec.hide);
        assert!(
            spec.folders
                .contains(&(home.join(".config/systemd"), false))
        );

        // And in a real terminal, where this kernel can keep it so.
        let terms = NodeTerminals::new(paths, Some(home.clone()));
        let _closing = Closing(&terms);
        let server = Arc::new(Server::default());
        let link: Arc<dyn Link> = server.clone();
        terms.connected(link.clone());
        let scope = MachineScope {
            terminal: TerminalAccess::Sandboxed,
            ..MachineScope::default()
        };
        terms.handle(&link, vec![open("s")], &scope, false);
        server
            .wait("answered", |m| {
                m.iter().any(|x| {
                    matches!(x, NodeTermUp::Opened { id, .. } | NodeTermUp::Failed { id, .. } if id == "s")
                })
            })
            .await;
        if !crate::sandbox::confines_writes() {
            let failed = server.0.lock().iter().any(|x| {
                matches!(x, NodeTermUp::Failed { id, error } if id == "s" && error.contains("the sandbox can't confine terminals"))
            });
            assert!(
                failed,
                "refused, never opened unconfined: {:?}",
                server.0.lock()
            );
            return;
        }
        let h = home.display();
        terms.handle(
            &link,
            vec![NodeTermDown::Input {
                id: "s".into(),
                data: format!(
                    "echo x >> '{h}/.bashrc' 2>/dev/null && echo WROTE-RC || echo refused-rc; \
                     touch '{h}/.bash_login' 2>/dev/null && echo MADE-LOGIN || echo refused-login; \
                     touch '{h}/.config/systemd/user/x.service' 2>/dev/null && echo MADE-UNIT || echo refused-unit; \
                     echo ok > '{h}/Documents/a' && echo wrote-docs; echo DONE-$((1+1))\r"
                ),
            }],
            &scope,
            false,
        );
        let s = server.clone();
        server
            .wait("done", move |_| s.output("s").contains("DONE-2"))
            .await;
        let out = server.output("s");
        for want in ["refused-rc", "refused-login", "refused-unit", "wrote-docs"] {
            assert!(out.contains(want), "{want}: {out}");
        }
        assert_eq!(
            std::fs::read_to_string(home.join(".bashrc")).unwrap(),
            "# mine\n"
        );
        assert!(!home.join(".bash_login").exists());
    }

    #[tokio::test]
    async fn a_program_that_doesnt_read_holds_up_only_its_own_terminal() {
        let _dog = crate::terminal::watchdog("a machine terminal test", 60);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::at(root.join("tessera"));
        paths.ensure().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let terms = NodeTerminals::new(paths, Some(home));
        let _closing = Closing(&terms);
        let server = Arc::new(Server::default());
        let link: Arc<dyn Link> = server.clone();
        terms.connected(link.clone());
        let scope = MachineScope {
            terminal: TerminalAccess::Full,
            ..MachineScope::default()
        };
        let input = |id: &str, data: String| NodeTermDown::Input {
            id: id.into(),
            data,
        };
        terms.handle(&link, vec![open("a"), open("b")], &scope, false);
        server
            .wait("both opened", |m| {
                ["a", "b"].iter().all(|w| {
                    m.iter()
                        .any(|x| matches!(x, NodeTermUp::Opened { id, .. } if id == w))
                })
            })
            .await;
        // In `a` a program runs that never reads, with the terminal in raw
        // mode (in line mode Linux drops what doesn't fit instead), and more
        // is typed than the terminal holds: typing into `a` waits.
        terms.handle(
            &link,
            vec![input("a", "stty raw; sleep 30\r".into())],
            &scope,
            false,
        );
        let s = server.clone();
        server
            .wait("sleep started", move |_| s.output("a").contains("sleep 30"))
            .await;
        let lots = "x".repeat(60_000);
        terms.handle(
            &link,
            vec![input("a", lots.clone()), input("a", lots)],
            &scope,
            false,
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let echoed = server.output("a").matches('x').count();
        assert!(echoed < 120_000, "typing into a is held up ({echoed})");
        // `b` answers at once all the same.
        let t0 = std::time::Instant::now();
        terms.handle(
            &link,
            vec![input("b", "echo B-$((1+1))\r".into())],
            &scope,
            false,
        );
        let s = server.clone();
        server
            .wait("b answered", move |_| s.output("b").contains("B-2"))
            .await;
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "b waited for a: {:?}",
            t0.elapsed()
        );
    }
}
