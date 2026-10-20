//! Terminals, server side: the user's own terminals on this server (see
//! `crate::terminal`) and on their machines (relayed to `node/terminal.rs`).
//!
//! Only a paired device's own connection opens, shows, types in or closes
//! one ([`TermConn`]); chats, tools, bots, jobs and machine logins have no
//! way in, and terminal output never goes to the event bus that chats and
//! logs read. A terminal belongs to the device that opened it; another
//! device is told it doesn't exist. Terminals open only over an end-to-end
//! encrypted connection or on the server itself: a plain connection from
//! the network passes through something (a tunnel, a proxy) that would see
//! every keystroke. Never from a web browser either ("Use Tessera in a web
//! browser"): its page comes from whoever serves the address (a tunnel's
//! edge, a proxy), who could change it. Terminals are off on this server
//! until the user turns them on, from the app or the server itself (never
//! a plain connection or a browser), and the user's devices are told each
//! time they are turned on or given more; a machine decides for itself, on
//! that computer.
//!
//! Kept for each terminal: who (the device's name), when and where, and how
//! it ended (`terminal-audit.jsonl`, owner-only) — never what was typed or
//! shown.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::*;
use crate::terminal::{self, Host, Limits, Out, Sink};

/// Terminals remembered in the record.
const AUDIT_KEEP: usize = 100;
/// How long a machine may take to open one.
const OPEN_WAIT: Duration = Duration::from_secs(20);
/// Output from a machine waiting for a slow device, past which that
/// terminal is closed (a machine sends at most one window ahead).
const RELAY_LIMIT: usize = 4 * terminal::WINDOW as usize;

const NOT_PRIVATE: &str = "terminals open only over an end-to-end encrypted connection (the Tessera app) or on the server itself; this connection passes through something that could read it";
const FROM_BROWSER: &str = "Terminals open only from the Tessera app, not from a web browser.";
const WIDEN_NOT_PRIVATE: &str = "terminals can be turned on, or given more, only from the Tessera app or on the server itself; this connection passes through something that could change it";
/// What may wait for a machine's link for one of its terminals, at most
/// (keystrokes' bytes, and a little for each other request).
const MAX_PENDING_INPUT: usize = terminal::MAX_PENDING_INPUT;
/// What a request other than typing counts for while it waits.
const REQUEST_WEIGHT: usize = 64;
const LINK_HELD_UP: &str = "over 1 MB of typing is still on its way to that computer (its connection is slow or stuck); the rest wasn't typed";
const LINK_STUCK: &str = "that computer's connection is slow or stuck; try again in a moment";

/// New nodes advertise backends; old Unix nodes already had terminals.
/// Never route a terminal to an old Windows node even if its scope says Full.
fn terminal_backend(os: &str, features: Option<&[String]>) -> bool {
    features.map_or_else(
        || {
            [
                "linux", "macos", "darwin", "freebsd", "openbsd", "netbsd", "android", "illumos",
                "solaris",
            ]
            .iter()
            .any(|p| os.to_ascii_lowercase().starts_with(p))
        },
        |c| c.iter().any(|name| name == "terminal"),
    )
}

/// Keep reported support, settings and the open guard on the same platforms.
fn server_terminal_access(
    unix: bool,
    windows: bool,
    access: TerminalAccess,
    require_sandbox: bool,
) -> Result<TerminalAccess, ApiError> {
    if !unix && !windows {
        return Err(bad("terminals aren't available on this server's platform"));
    }
    match access {
        TerminalAccess::Off => Err(refused(
            "terminals are off on this server; they can be turned on in Settings → Privacy & safety → What commands can reach",
        )),
        TerminalAccess::Sandboxed if windows => Err(bad(
            "Windows terminals are not sandboxed; choose full access in the server's terminal settings, or leave them off",
        )),
        TerminalAccess::Full if require_sandbox => Err(refused(
            "this server runs everything in the sandbox (sandbox = \"strict\"); choose sandboxed terminals in Settings → Privacy & safety → What commands can reach",
        )),
        a => Ok(a),
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;

    #[test]
    fn windows_server_guard_allows_full_but_never_off_or_sandboxed() {
        assert_eq!(
            server_terminal_access(false, true, TerminalAccess::Full, false).unwrap(),
            TerminalAccess::Full
        );
        assert!(
            server_terminal_access(false, true, TerminalAccess::Off, false)
                .unwrap_err()
                .message
                .contains("terminals are off")
        );
        assert!(
            server_terminal_access(false, true, TerminalAccess::Sandboxed, false)
                .unwrap_err()
                .message
                .contains("not sandboxed")
        );
        assert!(
            server_terminal_access(false, true, TerminalAccess::Full, true)
                .unwrap_err()
                .message
                .contains("runs everything in the sandbox")
        );
        assert!(server_terminal_access(false, false, TerminalAccess::Full, false).is_err());
        assert_eq!(
            server_terminal_access(true, false, TerminalAccess::Full, false).unwrap(),
            TerminalAccess::Full
        );
        assert_eq!(
            server_terminal_access(true, false, TerminalAccess::Sandboxed, false).unwrap(),
            TerminalAccess::Sandboxed
        );
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn server_open_request_reaches_the_shared_access_guard() {
        let _dog = crate::terminal::watchdog("server terminal access guard", 20);
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::open(
            Paths::at(dir.path().join("home")),
            EngineOptions { scheduler: false },
        )
        .await
        .unwrap();
        let (device, _) = e.store().add_device("Test app").unwrap();
        let (app, _rx) = e.terminal_conn(&device, true);
        let open = || Request::TerminalOpen {
            machine: None,
            cols: 80,
            rows: 24,
        };
        let off = e.terminal_request(open(), &app).await.unwrap_err();
        assert!(off.message.contains("terminals are off"), "{off:?}");
        // Reach the second guard decision without ever launching a shell in
        // the running account's real home or reading its startup profiles.
        {
            let mut config = e.0.config.write();
            config.terminal.access = TerminalAccess::Full;
            config.security.sandbox = SandboxMode::Strict;
        }
        let full = e.terminal_request(open(), &app).await.unwrap_err();
        assert!(
            full.message.contains("runs everything in the sandbox"),
            "{full:?}"
        );
        assert_eq!(e.open_terminals(), 0);
    }
    #[test]
    fn older_windows_is_not_a_terminal_node() {
        assert!(!terminal_backend("windows x86_64", None));
        assert!(!terminal_backend("unknown", None));
        assert!(terminal_backend("linux x86_64", None));
        assert!(terminal_backend("macos aarch64", None));
        assert!(!terminal_backend("linux x86_64", Some(&[])));
        assert!(terminal_backend(
            "windows x86_64",
            Some(&["terminal".into()])
        ));
        assert!(!terminal_backend(
            "windows x86_64",
            Some(&["future".into()])
        ));
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct AuditRec {
    id: String,
    #[serde(flatten)]
    entry: TerminalAudit,
}

/// A terminal on a machine, as this server relays it.
struct Remote {
    info: TerminalInfo,
    owner: String,
    /// The connection showing it, shared with its relay.
    sink: Arc<Mutex<Option<Sink>>>,
    /// Output on its way to the device, in order, and how much.
    relay: mpsc::UnboundedSender<Out>,
    queued: Arc<AtomicUsize>,
    /// What the device asks of it, on its way to the machine in order (a
    /// busy link holds up only this terminal), and how much waits.
    down: mpsc::UnboundedSender<NodeTermDown>,
    waiting: Arc<AtomicUsize>,
}

impl Remote {
    /// Hand `msg` to the machine, after what was asked of it before. Past
    /// [`MAX_PENDING_INPUT`] waiting it is refused (closing always goes), so
    /// nothing piles up for a stuck link.
    fn send_down(&self, msg: NodeTermDown) -> Result<(), ApiError> {
        let w = weight(&msg);
        let close = matches!(msg, NodeTermDown::Close { .. });
        let fits = |n: usize| (close || n + w <= MAX_PENDING_INPUT).then_some(n + w);
        if self
            .waiting
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, fits)
            .is_err()
        {
            let typing = matches!(msg, NodeTermDown::Input { .. });
            return Err(bad(if typing { LINK_HELD_UP } else { LINK_STUCK }));
        }
        self.down.send(msg).map_err(|_| {
            self.waiting.fetch_sub(w, Ordering::SeqCst);
            gone("that terminal isn't open any more")
        })
    }
}

/// What a request waiting for a machine's link counts for: its keystrokes,
/// or a little for anything else.
fn weight(m: &NodeTermDown) -> usize {
    match m {
        NodeTermDown::Input { data, .. } => data.len(),
        _ => REQUEST_WEIGHT,
    }
}

type Opened = std::result::Result<(String, bool), String>;

pub(crate) struct Terminals {
    host: Host,
    remote: Mutex<HashMap<String, Remote>>,
    /// Machines asked to open one, by terminal id: (machine, answer).
    opening: Mutex<HashMap<String, (String, oneshot::Sender<Opened>)>>,
    /// Newest first.
    audit: Mutex<VecDeque<AuditRec>>,
    audit_file: PathBuf,
    conns: AtomicU64,
}

fn limits_of(c: &crate::config::TerminalConfig) -> Limits {
    Limits {
        max_open: c.max_open.clamp(1, 16),
        idle: Duration::from_secs(u64::from(clamp_idle(c.idle_minutes)) * 60),
        grace: Duration::from_secs(c.grace_secs.clamp(10, 3600)),
        ..Limits::default()
    }
}

fn clamp_idle(m: u32) -> u32 {
    m.clamp(15, 240)
}

impl Terminals {
    pub(crate) fn new(
        paths: &Paths,
        config: &crate::config::TerminalConfig,
        me: Weak<Inner>,
    ) -> Self {
        let audit_file = paths.data_dir.join("terminal-audit.jsonl");
        let audit: VecDeque<AuditRec> = std::fs::read_to_string(&audit_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .take(AUDIT_KEEP)
            .collect();
        let on_close = {
            let me = me.clone();
            Arc::new(move |c: &terminal::Closed| {
                if let Some(e) = me.upgrade() {
                    e.terminals.audit_closed(&c.id, &c.reason);
                }
            })
        };
        Self {
            host: Host::new(limits_of(config), on_close),
            remote: Mutex::new(HashMap::new()),
            opening: Mutex::new(HashMap::new()),
            audit: Mutex::new(audit),
            audit_file,
            conns: AtomicU64::new(1),
        }
    }

    fn save_audit(&self) {
        let text: String = self
            .audit
            .lock()
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .map(|l| l + "\n")
            .collect();
        let _ = crate::config::write_atomic(&self.audit_file, text.as_bytes(), true);
    }

    fn audit_opened(&self, id: &str, device: &str, place: &str, sandboxed: bool) {
        {
            let mut a = self.audit.lock();
            a.push_front(AuditRec {
                id: id.to_string(),
                entry: TerminalAudit {
                    opened_at: now_ms(),
                    closed_at: None,
                    device: device.to_string(),
                    place: place.to_string(),
                    sandboxed,
                    ended: String::new(),
                },
            });
            a.truncate(AUDIT_KEEP);
        }
        self.save_audit();
    }

    fn audit_closed(&self, id: &str, reason: &str) {
        {
            let mut a = self.audit.lock();
            let Some(r) = a.iter_mut().find(|r| r.id == id) else {
                return;
            };
            if r.entry.closed_at.is_some() {
                return;
            }
            r.entry.closed_at = Some(now_ms());
            r.entry.ended = one_line(reason, 200);
        }
        self.save_audit();
    }

    /// The server is stopping: every terminal ends.
    pub(crate) fn shutdown(&self) {
        self.host.close_all("the server stopped");
        let ids: Vec<String> = self.remote.lock().keys().cloned().collect();
        for id in ids {
            self.audit_closed(&id, "the server stopped");
        }
        // Those the server opened before a crash end with it too.
        let open: Vec<String> = self
            .audit
            .lock()
            .iter()
            .filter(|r| r.entry.closed_at.is_none())
            .map(|r| r.id.clone())
            .collect();
        for id in open {
            self.audit_closed(&id, "the server stopped");
        }
    }
}

/// A device connection's terminals: their output arrives on the receiver,
/// and only there. Dropping it leaves them waiting for the device to come
/// back (for the grace period).
pub struct TermConn {
    id: u64,
    device: String,
    private: bool,
    /// A web browser's login ("Use Tessera in a web browser").
    browser: bool,
    tx: mpsc::Sender<Out>,
    engine: Weak<Inner>,
}

impl TermConn {
    fn sink(&self) -> Sink {
        Sink {
            conn: self.id,
            tx: self.tx.clone(),
        }
    }
}

impl Drop for TermConn {
    fn drop(&mut self) {
        if let Some(e) = self.engine.upgrade() {
            e.terminals_conn_gone(self.id);
        }
    }
}

/// Requests about terminals ([`Engine::terminal_request`]).
pub fn is_terminal_request(req: &Request) -> bool {
    matches!(
        req,
        Request::TerminalStatus
            | Request::SetTerminalSettings { .. }
            | Request::TerminalOpen { .. }
            | Request::TerminalAttach { .. }
            | Request::TerminalDetach { .. }
            | Request::TerminalInput { .. }
            | Request::TerminalResize { .. }
            | Request::TerminalAck { .. }
            | Request::TerminalClose { .. }
    )
}

/// Those that must happen in the order they were sent: keystrokes, and
/// showing, hiding or closing (a tile shown again right after it was hidden).
/// Acknowledgements only ever move forward, so they don't wait in that line:
/// a long paste into a program that isn't reading can't hold up the output
/// of every terminal on the connection.
pub fn is_ordered_terminal_request(req: &Request) -> bool {
    matches!(
        req,
        Request::TerminalInput { .. }
            | Request::TerminalResize { .. }
            | Request::TerminalAttach { .. }
            | Request::TerminalDetach { .. }
            | Request::TerminalClose { .. }
    )
}

impl Engine {
    /// The terminals of one device connection (`device`, its paired id).
    /// `private`: end-to-end encrypted, or made on this computer.
    pub fn terminal_conn(&self, device: &str, private: bool) -> (TermConn, mpsc::Receiver<Out>) {
        let (tx, rx) = mpsc::channel(256);
        let id = self.0.terminals.conns.fetch_add(1, Ordering::Relaxed);
        // Known by its login, whichever way it connected.
        let browser = self
            .0
            .store
            .browsers()
            .map_or(true, |b| b.iter().any(|d| d == device));
        (
            TermConn {
                id,
                device: device.to_string(),
                private: private && !browser,
                browser,
                tx,
                engine: Arc::downgrade(&self.0),
            },
            rx,
        )
    }

    /// A terminal request from the device connection `conn`.
    pub async fn terminal_request(&self, req: Request, conn: &TermConn) -> ApiResult {
        self.0.clone().terminal_request(req, conn).await
    }

    /// Every terminal open on this server now (tests, diagnostics).
    pub fn open_terminals(&self) -> usize {
        self.0.terminals.host.count() + self.0.terminals.remote.lock().len()
    }
}

fn gone(why: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::NotFound, why)
}

fn refused(why: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Unauthorized, why)
}

impl Inner {
    fn terminal_config(&self) -> crate::config::TerminalConfig {
        self.config.read().terminal.clone()
    }

    /// The command sandbox can confine terminals on this server now.
    fn terminal_sandbox_ok(&self) -> bool {
        cfg!(target_os = "linux")
            && self.config.read().security.sandbox != SandboxMode::Off
            && crate::sandbox::support().complete()
    }

    fn device_name(&self, id: &str) -> String {
        self.store
            .devices(None)
            .ok()
            .and_then(|l| l.into_iter().find(|d| d.id == id))
            .map(|d| d.name)
            .unwrap_or_else(|| "a device".into())
    }

    fn server_name(&self) -> String {
        let n = self.config.read().server.name.clone();
        if n.trim().is_empty() {
            "this server".into()
        } else {
            n
        }
    }

    pub(super) fn terminal_status(&self, device: Option<&str>, private: bool) -> TerminalStatus {
        let c = self.terminal_config();
        let strict = self.config.read().security.sandbox == SandboxMode::Strict;
        let sandbox = if !cfg!(target_os = "linux") {
            "the command sandbox exists only on Linux".to_string()
        } else if self.config.read().security.sandbox == SandboxMode::Off {
            "off (Settings → Privacy & safety)".to_string()
        } else {
            crate::sandbox::describe()
        };
        let mut open: Vec<TerminalInfo> = match device {
            Some(d) => self
                .terminals
                .host
                .list(Some(d))
                .into_iter()
                .map(|i| TerminalInfo {
                    id: i.id,
                    machine: None,
                    place: i.place,
                    sandboxed: i.sandboxed,
                    shell: i.shell,
                    started_at: i.started_at,
                })
                .collect(),
            None => Vec::new(),
        };
        if let Some(d) = device {
            open.extend(
                self.terminals
                    .remote
                    .lock()
                    .values()
                    .filter(|r| r.owner == d)
                    .map(|r| r.info.clone()),
            );
        }
        let limits = limits_of(&c);
        TerminalStatus {
            settings: TerminalSettings {
                access: c.access,
                idle_minutes: clamp_idle(c.idle_minutes),
            },
            supported: cfg!(any(unix, windows)),
            sandbox_ok: self.terminal_sandbox_ok(),
            sandbox,
            strict,
            private,
            max_open: limits.max_open as u32,
            grace_secs: limits.grace.as_secs() as u32,
            open,
            recent: self
                .terminals
                .audit
                .lock()
                .iter()
                .take(30)
                .map(|r| r.entry.clone())
                .collect(),
        }
    }

    /// Change the terminal settings (`by`: the device asking; the server
    /// itself checks what is possible here). Turned on or given more: the
    /// user's devices are told.
    pub(super) fn set_terminal_settings(
        &self,
        s: TerminalSettings,
        by: Option<&str>,
    ) -> Result<(), ApiError> {
        let strict = self.config.read().security.sandbox == SandboxMode::Strict;
        match s.access {
            TerminalAccess::Off => {}
            _ if !cfg!(any(unix, windows)) => {
                return Err(bad("terminals aren't available on this platform"));
            }
            TerminalAccess::Sandboxed if !self.terminal_sandbox_ok() => {
                return Err(bad(
                    "the command sandbox is off or incomplete on this server, so terminals can't be kept in it here",
                ));
            }
            TerminalAccess::Full if strict => {
                return Err(bad(
                    "this server runs everything in the sandbox (sandbox = \"strict\"), so terminals can only be sandboxed",
                ));
            }
            _ => {}
        }
        let (was, new) = {
            let mut c = self.config.write();
            let was = c.terminal.access;
            c.terminal.access = s.access;
            c.terminal.idle_minutes = clamp_idle(s.idle_minutes);
            c.save(&self.paths).map_err(internal)?;
            (was, c.terminal.clone())
        };
        self.terminals.host.set_limits(limits_of(&new));
        tracing::info!("terminal settings changed: {:?}", new.access);
        if widens(was, new.access) {
            let who = by.map_or_else(|| "the server itself".into(), |d| self.device_name(d));
            self.tell_terminals_widened(&self.server_name(), new.access, &format!("from {who}"));
        }
        match new.access {
            // Turned off: none stays open.
            TerminalAccess::Off => self
                .terminals
                .host
                .close_all("terminals were turned off on this server"),
            // Narrowed: those with more than is allowed now end.
            TerminalAccess::Sandboxed => {
                for t in self.terminals.host.list(None) {
                    if !t.sandboxed {
                        let _ = self.terminals.host.close(
                            &t.id,
                            &t.owner,
                            "terminals here are sandboxed now",
                        );
                    }
                }
            }
            TerminalAccess::Full => {}
        }
        Ok(())
    }

    /// The command sandbox's mode changed (Settings → Privacy & safety): with
    /// `strict`, everything here runs sandboxed, so full-access terminals end.
    pub(super) fn terminals_follow_sandbox(&self) {
        if self.config.read().security.sandbox != SandboxMode::Strict {
            return;
        }
        for t in self.terminals.host.list(None) {
            if !t.sandboxed {
                let _ = self.terminals.host.close(
                    &t.id,
                    &t.owner,
                    "this server runs everything in the sandbox now",
                );
            }
        }
    }

    async fn terminal_request(self: Arc<Self>, req: Request, conn: &TermConn) -> ApiResult {
        let device = conn.device.as_str();
        // A web browser may only see that terminals exist here.
        if conn.browser && !matches!(req, Request::TerminalStatus) {
            return Err(refused(FROM_BROWSER));
        }
        // Keystrokes and output only over a private connection: not even
        // typing into a terminal shown elsewhere goes over a plain one.
        if !conn.private
            && !matches!(
                req,
                Request::TerminalStatus | Request::SetTerminalSettings { .. }
            )
        {
            return Err(refused(NOT_PRIVATE));
        }
        match req {
            Request::TerminalStatus => to_value(self.terminal_status(Some(device), conn.private)),
            Request::SetTerminalSettings { settings } => {
                // Narrowing is fine from anywhere; turning on or more only
                // from where nothing on the way could have changed the ask.
                if !conn.private && widens(self.terminal_config().access, settings.access) {
                    return Err(refused(WIDEN_NOT_PRIVATE));
                }
                self.set_terminal_settings(settings, Some(device))?;
                to_value(self.terminal_status(Some(device), conn.private))
            }
            Request::TerminalOpen {
                machine,
                cols,
                rows,
            } => {
                let info = match machine {
                    None => self.open_here(conn, cols, rows)?,
                    Some(m) => self.open_on_machine(conn, &m, cols, rows).await?,
                };
                to_value(info)
            }
            Request::TerminalAttach { id, cols, rows } => {
                if let Some(info) = self.attach_remote(conn, &id, cols, rows).await? {
                    return to_value(info);
                }
                let i = self
                    .terminals
                    .host
                    .attach(&id, device, conn.sink(), Some((cols, rows)))
                    .map_err(gone)?;
                to_value(TerminalInfo {
                    id: i.id,
                    machine: None,
                    place: i.place,
                    sandboxed: i.sandboxed,
                    shell: i.shell,
                    started_at: i.started_at,
                })
            }
            Request::TerminalDetach { id } => {
                // Not shown here any more at once, even if the machine hears
                // of it only later.
                if let Some(r) = self.terminals.remote.lock().get(&id)
                    && r.owner == device
                {
                    *r.sink.lock() = None;
                }
                if !self
                    .remote_down(&id, device, |id| NodeTermDown::Detach { id })
                    .await?
                {
                    self.terminals.host.detach(&id, device).map_err(gone)?;
                }
                Ok(Value::Null)
            }
            Request::TerminalInput { id, data } => {
                if data.len() > terminal::MAX_INPUT {
                    return Err(bad(format!(
                        "that is more than {} KB at once; paste it in parts",
                        terminal::MAX_INPUT >> 10
                    )));
                }
                let remote = self
                    .remote_down(&id, device, |id| NodeTermDown::Input {
                        id,
                        data: data.clone(),
                    })
                    .await?;
                if !remote {
                    self.terminals
                        .host
                        .input(&id, device, data.as_bytes())
                        .await
                        .map_err(bad)?;
                }
                Ok(Value::Null)
            }
            Request::TerminalResize { id, cols, rows } => {
                if !self
                    .remote_down(&id, device, |id| NodeTermDown::Resize { id, cols, rows })
                    .await?
                {
                    self.terminals
                        .host
                        .resize(&id, device, cols, rows)
                        .map_err(gone)?;
                }
                Ok(Value::Null)
            }
            Request::TerminalAck { id, upto } => {
                if !self
                    .remote_down(&id, device, |id| NodeTermDown::Ack { id, upto })
                    .await?
                {
                    self.terminals.host.ack(&id, device, upto).map_err(gone)?;
                }
                Ok(Value::Null)
            }
            Request::TerminalClose { id } => {
                if self
                    .remote_down(&id, device, |id| NodeTermDown::Close { id })
                    .await?
                {
                    self.remote_ended(&id, None, "closed");
                } else {
                    self.terminals
                        .host
                        .close(&id, device, "closed")
                        .map_err(gone)?;
                }
                Ok(Value::Null)
            }
            _ => Err(bad("not a terminal request")),
        }
    }

    /// What may open on this server, and confined how; or why not.
    fn server_terminal_allowed(&self) -> Result<TerminalAccess, ApiError> {
        server_terminal_access(
            cfg!(unix),
            cfg!(windows),
            self.terminal_config().access,
            self.config.read().security.sandbox == SandboxMode::Strict,
        )
    }

    #[cfg(unix)]
    fn open_here(&self, conn: &TermConn, cols: u16, rows: u16) -> Result<TerminalInfo, ApiError> {
        let access = self.server_terminal_allowed()?;
        let id = format!("t{}", crate::util::random_token(16));
        let (home, shell) = crate::pty::account();
        let cwd = home
            .clone()
            .filter(|h| h.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"));
        let sandbox = match access {
            TerminalAccess::Sandboxed => {
                let (mut spec, _) = self.sandbox_spec().ok_or_else(|| {
                    refused("the command sandbox is off on this server (Settings → Privacy & safety), so terminals can't be kept in it; turn it on, or choose full access in Settings → Privacy & safety → What commands can reach")
                })?;
                spec.group = format!("terminal-{id}");
                match crate::sandbox::prepare(Some(&(spec, true)), &cwd) {
                    Ok(Some(p)) => Some(p),
                    Ok(None) => None,
                    Err(e) => {
                        return Err(refused(format!(
                            "the sandbox can't confine terminals here ({e})"
                        )));
                    }
                }
            }
            _ => None,
        };
        let sandboxed = sandbox.is_some();
        // A login shell that reads the user's own settings too (pty::shell_start).
        let (program, args) = crate::pty::shell_start(&shell);
        let owner_name = self.device_name(&conn.device);
        let place = self.server_name();
        let info = self
            .terminals
            .host
            .open(
                terminal::Opening {
                    id: id.clone(),
                    owner: conn.device.clone(),
                    owner_name: owner_name.clone(),
                    place: place.clone(),
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
                conn.sink(),
            )
            .map_err(bad)?;
        self.terminals
            .audit_opened(&id, &owner_name, &place, sandboxed);
        Ok(TerminalInfo {
            id: info.id,
            machine: None,
            place: info.place,
            sandboxed: info.sandboxed,
            shell: info.shell,
            started_at: info.started_at,
        })
    }

    #[cfg(windows)]
    fn open_here(&self, conn: &TermConn, cols: u16, rows: u16) -> Result<TerminalInfo, ApiError> {
        self.server_terminal_allowed()?;
        let c = self.terminal_config();
        if c.access != TerminalAccess::Full {
            return Err(bad(
                "Windows terminals are not sandboxed; choose full access in the server's terminal settings, or leave them off",
            ));
        }
        self.terminals.host.set_limits(limits_of(&c));
        let account = crate::pty::account().map_err(|e| {
            bad(format!(
                "the terminal couldn't find this account's shell: {e}"
            ))
        })?;
        let (program, args) = account.shell_start();
        let id = format!("t{}", crate::util::random_token(16));
        let owner_name = self.device_name(&conn.device);
        let place = self.server_name();
        let info = self
            .terminals
            .host
            .open(
                crate::terminal::Opening {
                    id: id.clone(),
                    owner: conn.device.clone(),
                    owner_name: owner_name.clone(),
                    place: place.clone(),
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
                conn.sink(),
            )
            .map_err(bad)?;
        self.terminals.audit_opened(&id, &owner_name, &place, false);
        Ok(TerminalInfo {
            id: info.id,
            machine: None,
            place: info.place,
            sandboxed: false,
            shell: info.shell,
            started_at: info.started_at,
        })
    }
    #[cfg(not(any(unix, windows)))]
    fn open_here(&self, _: &TermConn, _: u16, _: u16) -> Result<TerminalInfo, ApiError> {
        self.server_terminal_allowed()?;
        Err(bad("terminals aren't available on this server's platform"))
    }

    // ---- terminals on machines ------------------------------------------

    async fn open_on_machine(
        self: &Arc<Self>,
        conn: &TermConn,
        machine: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalInfo, ApiError> {
        let m = self
            .machine_list()
            .into_iter()
            .find(|m| m.id == machine)
            .ok_or_else(|| gone("that machine isn't known here any more"))?;
        if !terminal_backend(&m.os, m.features.as_deref()) {
            return Err(bad(format!(
                "terminals aren't available on {} (update the node on that computer first)",
                m.name
            )));
        }
        if !m.online {
            return Err(bad(format!("{} is offline", m.name)));
        }
        if m.paused {
            return Err(bad(format!(
                "{} is paused; it can be resumed in Settings → Devices",
                m.name
            )));
        }
        if m.scope.terminal == TerminalAccess::Off {
            return Err(refused(format!(
                "terminals are off on {}; only that computer can turn them on (Settings → Devices → This computer)",
                m.name
            )));
        }
        let tx = self
            .machine_tx(&m.id)
            .ok_or_else(|| bad(format!("{} is offline", m.name)))?;
        let id = format!("t{}", crate::util::random_token(16));
        let owner_name = self.device_name(&conn.device);
        let (otx, orx) = oneshot::channel();
        self.terminals
            .opening
            .lock()
            .insert(id.clone(), (m.id.clone(), otx));
        // Its output may come before the answer does: the record is ready.
        let sink = Arc::new(Mutex::new(Some(conn.sink())));
        let (relay, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        tokio::spawn(relay_loop(rx, sink.clone(), queued.clone()));
        let (down, down_rx) = mpsc::unbounded_channel();
        let waiting = Arc::new(AtomicUsize::new(0));
        tokio::spawn(down_loop(
            Arc::downgrade(self),
            m.id.clone(),
            down_rx,
            waiting.clone(),
        ));
        self.terminals.remote.lock().insert(
            id.clone(),
            Remote {
                info: TerminalInfo {
                    id: id.clone(),
                    machine: Some(m.id.clone()),
                    place: m.name.clone(),
                    sandboxed: false,
                    shell: String::new(),
                    started_at: now_ms(),
                },
                owner: conn.device.clone(),
                sink,
                relay,
                queued,
                down,
                waiting,
            },
        );
        let sent = tx
            .send(Event::NodeTerminal {
                msgs: vec![NodeTermDown::Open {
                    id: id.clone(),
                    device: conn.device.clone(),
                    device_name: owner_name.clone(),
                    cols,
                    rows,
                }],
            })
            .await;
        let answer = match sent {
            Ok(()) => tokio::time::timeout(OPEN_WAIT, orx).await,
            Err(_) => Ok(Ok(Err(format!("{} went offline", m.name)))),
        };
        self.terminals.opening.lock().remove(&id);
        let answer = match answer {
            Ok(Ok(a)) => a,
            Ok(Err(_)) => Err(format!("{} went offline", m.name)),
            Err(_) => {
                let _ = tx.try_send(Event::NodeTerminal {
                    msgs: vec![NodeTermDown::Close { id: id.clone() }],
                });
                Err(format!("{} didn't answer", m.name))
            }
        };
        match answer {
            Ok((shell, sandboxed)) => {
                let info = {
                    let mut remote = self.terminals.remote.lock();
                    let Some(r) = remote.get_mut(&id) else {
                        return Err(bad(format!("the terminal on {} ended at once", m.name)));
                    };
                    r.info.shell = one_line(&shell, 40);
                    r.info.sandboxed = sandboxed;
                    r.info.clone()
                };
                self.terminals
                    .audit_opened(&id, &owner_name, &m.name, sandboxed);
                tracing::info!(
                    "terminal {id} opened on machine {} for device {}",
                    m.id,
                    conn.device
                );
                Ok(info)
            }
            Err(e) => {
                self.terminals.remote.lock().remove(&id);
                Err(bad(format!("{}: {}", m.name, one_line(&e, 300))))
            }
        }
    }

    /// Show a machine's terminal again. `None`: it isn't one.
    async fn attach_remote(
        &self,
        conn: &TermConn,
        id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<Option<TerminalInfo>, ApiError> {
        let remote = self.terminals.remote.lock();
        match remote.get(id) {
            Some(r) if r.owner == conn.device => {
                r.send_down(NodeTermDown::Resize {
                    id: id.to_string(),
                    cols,
                    rows,
                })?;
                r.send_down(NodeTermDown::Attach { id: id.to_string() })?;
                *r.sink.lock() = Some(conn.sink());
                Ok(Some(r.info.clone()))
            }
            // Another device's terminal looks the same as none.
            Some(_) => Err(gone("that terminal isn't open any more")),
            None => Ok(None),
        }
    }

    /// Pass a request about one of `owner`'s terminals on to its machine,
    /// after what was asked of it before (it doesn't wait for the machine:
    /// a busy link holds up only this terminal). False: it isn't a
    /// machine's terminal.
    async fn remote_down(
        &self,
        id: &str,
        owner: &str,
        msg: impl FnOnce(String) -> NodeTermDown,
    ) -> Result<bool, ApiError> {
        let machine = {
            let remote = self.terminals.remote.lock();
            match remote.get(id) {
                Some(r) if r.owner == owner => r.info.machine.clone().unwrap_or_default(),
                Some(_) => return Err(gone("that terminal isn't open any more")),
                None => return Ok(false),
            }
        };
        if self.machine_tx(&machine).is_none() {
            return Err(bad("that machine went offline"));
        }
        let remote = self.terminals.remote.lock();
        let r = remote
            .get(id)
            .filter(|r| r.owner == owner)
            .ok_or_else(|| gone("that terminal isn't open any more"))?;
        r.send_down(msg(id.to_string()))?;
        Ok(true)
    }

    /// A machine's terminal ended (or was closed from here).
    fn remote_ended(&self, id: &str, code: Option<i32>, reason: &str) {
        let Some(r) = self.terminals.remote.lock().remove(id) else {
            return;
        };
        let _ = r.relay.send(Out::Exit {
            id: id.to_string(),
            code,
            reason: reason.to_string(),
        });
        self.terminals.audit_closed(id, reason);
        tracing::info!("terminal {id} on a machine closed ({reason})");
    }

    /// News from machine `device`'s terminals (its own connection only).
    pub(super) fn node_terminal(&self, device: &str, msgs: Vec<NodeTermUp>) {
        let ours = |id: &str| {
            self.terminals
                .remote
                .lock()
                .get(id)
                .is_some_and(|r| r.info.machine.as_deref() == Some(device))
        };
        for m in msgs {
            match m {
                NodeTermUp::Opened {
                    id,
                    shell,
                    sandboxed,
                } => {
                    let waiting = {
                        let mut o = self.terminals.opening.lock();
                        match o.get(&id) {
                            Some((mach, _)) if mach == device => o.remove(&id),
                            _ => None,
                        }
                    };
                    if let Some((_, tx)) = waiting {
                        let _ = tx.send(Ok((shell, sandboxed)));
                    }
                }
                NodeTermUp::Failed { id, error } => {
                    let waiting = {
                        let mut o = self.terminals.opening.lock();
                        match o.get(&id) {
                            Some((mach, _)) if mach == device => o.remove(&id),
                            _ => None,
                        }
                    };
                    if let Some((_, tx)) = waiting {
                        let _ = tx.send(Err(error));
                    }
                }
                NodeTermUp::Output { id, seq, data } => {
                    if !ours(&id) {
                        continue;
                    }
                    use base64::Engine as _;
                    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) else {
                        continue;
                    };
                    let over = {
                        let remote = self.terminals.remote.lock();
                        let Some(r) = remote.get(&id) else { continue };
                        // Shown nowhere: the machine keeps it for later.
                        if r.sink.lock().is_none() {
                            continue;
                        }
                        let n = r.queued.fetch_add(bytes.len(), Ordering::SeqCst) + bytes.len();
                        if n <= RELAY_LIMIT {
                            let _ = r.relay.send(Out::Output {
                                id: id.clone(),
                                seq,
                                data: bytes,
                            });
                            false
                        } else {
                            true
                        }
                    };
                    if over {
                        if let Some(tx) = self.machine_tx(device) {
                            let _ = tx.try_send(Event::NodeTerminal {
                                msgs: vec![NodeTermDown::Close { id: id.clone() }],
                            });
                        }
                        self.remote_ended(
                            &id,
                            None,
                            "the machine sent more than the device could take",
                        );
                    }
                }
                NodeTermUp::Exit { id, code, reason } => {
                    if ours(&id) {
                        self.remote_ended(&id, code, &one_line(&reason, 200));
                    }
                }
            }
        }
    }

    /// Machine `machine` went offline, was paused or turned off: its
    /// terminals end.
    pub(super) fn terminals_machine_gone(&self, machine: &str, reason: &str) {
        let ids: Vec<String> = self
            .terminals
            .remote
            .lock()
            .iter()
            .filter(|(_, r)| r.info.machine.as_deref() == Some(machine))
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            self.remote_ended(&id, None, reason);
        }
        let waiting: Vec<oneshot::Sender<Opened>> = {
            let mut o = self.terminals.opening.lock();
            let ids: Vec<String> = o
                .iter()
                .filter(|(_, (m, _))| m == machine)
                .map(|(k, _)| k.clone())
                .collect();
            ids.iter()
                .filter_map(|k| o.remove(k))
                .map(|(_, tx)| tx)
                .collect()
        };
        for tx in waiting {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// Device `device` was signed out: its terminals end at once, here and
    /// on every machine.
    pub(super) fn terminals_signed_out(&self, device: &str) {
        self.end_terminals_of(device, "its device was signed out");
    }

    /// Every terminal of `device` ends (`why`), here and on every machine.
    /// A machine whose queue is full right now is told as soon as there is
    /// room (were its connection to go first, it closes its terminals as
    /// it goes): the message is never dropped.
    pub(super) fn end_terminals_of(&self, device: &str, why: &str) {
        self.terminals.host.close_owner(device, why);
        let ids: Vec<String> = self
            .terminals
            .remote
            .lock()
            .iter()
            .filter(|(_, r)| r.owner == device)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            self.remote_ended(&id, None, why);
        }
        for m in self.machine_list().into_iter().filter(|m| m.online) {
            let Some(tx) = self.machine_tx(&m.id) else {
                continue;
            };
            let msg = Event::NodeTerminal {
                msgs: vec![NodeTermDown::SignedOut {
                    device: device.to_string(),
                }],
            };
            if let Err(mpsc::error::TrySendError::Full(msg)) = tx.try_send(msg) {
                match tokio::runtime::Handle::try_current() {
                    Ok(rt) => {
                        rt.spawn(async move {
                            let _ = tx.send(msg).await;
                        });
                    }
                    Err(_) => {
                        let _ = tx.blocking_send(msg);
                    }
                }
            }
        }
    }

    /// Terminals were turned on, or given more, at `place` (`how`: from
    /// which device, or on that computer): the user's devices are told, in
    /// case it wasn't them.
    pub(super) fn tell_terminals_widened(&self, place: &str, access: TerminalAccess, how: &str) {
        let (title, what) = match access {
            TerminalAccess::Off => return,
            TerminalAccess::Sandboxed => ("Terminals turned on", "turned on (sandboxed)"),
            TerminalAccess::Full => ("Terminals with full access", "given full access"),
        };
        let body = format!(
            "Terminals on {place} were {what} {how}. If that wasn't you, turn them off there and sign out the device you don't know (Settings → Devices)."
        );
        tracing::info!("terminals on {place} were {what} {how}");
        if let Err(e) = self.send_notification(crate::store::NewNotification {
            title: title.into(),
            body,
            from: "Tessera".into(),
            session_id: None,
            due_at: now_ms(),
            kind: "machine".into(),
            ..Default::default()
        }) {
            tracing::warn!("couldn't tell the devices about terminals: {e}");
        }
    }

    /// A device connection closed: what it showed waits for it.
    fn terminals_conn_gone(&self, conn: u64) {
        self.terminals.host.detach_conn(conn);
        let detached: Vec<(String, String)> = {
            let remote = self.terminals.remote.lock();
            remote
                .iter()
                .filter_map(|(id, r)| {
                    let mut s = r.sink.lock();
                    if s.as_ref().is_some_and(|s| s.conn == conn) {
                        *s = None;
                        Some((id.clone(), r.info.machine.clone().unwrap_or_default()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (id, machine) in detached {
            if let Some(tx) = self.machine_tx(&machine) {
                let _ = tx.try_send(Event::NodeTerminal {
                    msgs: vec![NodeTermDown::Detach { id }],
                });
            }
        }
    }
}

/// Terminal access `to` gives more than `from` did.
pub(super) fn widens(from: TerminalAccess, to: TerminalAccess) -> bool {
    let rank = |a: TerminalAccess| match a {
        TerminalAccess::Off => 0,
        TerminalAccess::Sandboxed => 1,
        TerminalAccess::Full => 2,
    };
    rank(to) > rank(from)
}

/// Hand what a device asks of a machine's terminal to that machine, in
/// order, a batch at a time; `waiting` counts what is on its way
/// ([`weight`]).
async fn down_loop(
    me: Weak<Inner>,
    machine: String,
    mut rx: mpsc::UnboundedReceiver<NodeTermDown>,
    waiting: Arc<AtomicUsize>,
) {
    while let Some(first) = rx.recv().await {
        let mut bytes = weight(&first);
        let mut msgs = vec![first];
        while bytes < 256 << 10
            && let Ok(more) = rx.try_recv()
        {
            bytes += weight(&more);
            msgs.push(more);
        }
        let tx = me.upgrade().and_then(|e| e.machine_tx(&machine));
        if let Some(tx) = tx {
            let _ = tx.send(Event::NodeTerminal { msgs }).await;
        }
        waiting.fetch_sub(bytes, Ordering::SeqCst);
    }
}

/// Hand a machine terminal's output to the device showing it, in order.
async fn relay_loop(
    mut rx: mpsc::UnboundedReceiver<Out>,
    sink: Arc<Mutex<Option<Sink>>>,
    queued: Arc<AtomicUsize>,
) {
    while let Some(out) = rx.recv().await {
        let len = match &out {
            Out::Output { data, .. } => data.len(),
            Out::Exit { .. } => 0,
        };
        let s = sink.lock().clone();
        if let Some(s) = s {
            let _ = s.tx.send(out).await;
        }
        queued.fetch_sub(len, Ordering::SeqCst);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::terminal::Opening;

    async fn engine() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::open(
            Paths::at(dir.path().join("home")),
            EngineOptions { scheduler: false },
        )
        .await
        .unwrap();
        (e, dir)
    }

    fn settings(access: TerminalAccess) -> Request {
        Request::SetTerminalSettings {
            settings: TerminalSettings {
                access,
                idle_minutes: 30,
            },
        }
    }

    fn open_here() -> Request {
        Request::TerminalOpen {
            machine: None,
            cols: 80,
            rows: 24,
        }
    }

    /// A shell for `owner` on the server's own host, as if it had been
    /// opened (whatever the checks in front would say now).
    fn shell(e: &Engine, id: &str, owner: &str, dir: &Path, sink: &mpsc::Sender<Out>) {
        e.0.terminals
            .host
            .open(
                Opening {
                    id: id.into(),
                    owner: owner.into(),
                    owner_name: "a device".into(),
                    place: "this server".into(),
                    sandboxed: false,
                    start: crate::pty::Start {
                        program: "/bin/sh".into(),
                        args: vec!["-i".into()],
                        cwd: dir.to_path_buf(),
                        env: vec![("PATH".into(), "/usr/bin:/bin".into())],
                        cols: 80,
                        rows: 24,
                        sandbox: None,
                    },
                },
                Sink {
                    conn: 1,
                    tx: sink.clone(),
                },
            )
            .unwrap();
    }

    /// Machine "Desktop" says hello with terminals at `terminal`.
    async fn hello(e: &Engine, conn: &crate::engine::NodeConn, terminal: TerminalAccess) {
        e.node_request(
            Request::NodeHello {
                name: "Desktop".into(),
                os: "linux".into(),
                version: "0".into(),
                scope: MachineScope {
                    terminal,
                    ..MachineScope::default()
                },
                generation: 1,
                sandbox: String::new(),
                sandbox_ok: terminal == TerminalAccess::Sandboxed,
                home: None,
                computer: None,
                host: None,
                features: None,
                guard_key: None,
            },
            conn,
        )
        .await
        .unwrap();
    }

    /// Turned on or given more only from the app (never a plain connection
    /// or a browser), and then every device hears of it; narrowing is fine
    /// from anywhere. A browser can't open, type in or change terminals.
    #[tokio::test]
    async fn only_the_app_turns_terminals_on_and_the_devices_hear_of_it() {
        let (e, _dir) = engine().await;
        let (phone, _) = e.store().add_device("Phone").unwrap();
        let (plain, _rx) = e.terminal_conn(&phone, false);
        let err = e
            .terminal_request(settings(TerminalAccess::Full), &plain)
            .await
            .unwrap_err();
        assert!(err.message.contains("only from the Tessera app"), "{err:?}");
        assert_eq!(e.0.terminal_config().access, TerminalAccess::Off);

        let (browser, _) = e.store().add_browser("Firefox", &[4u8; 65]).unwrap();
        let (web, _rx) = e.terminal_conn(&browser, true);
        for req in [
            settings(TerminalAccess::Full),
            settings(TerminalAccess::Off),
            open_here(),
            Request::TerminalInput {
                id: "t1".into(),
                data: "ls\r".into(),
            },
        ] {
            let err = e.terminal_request(req, &web).await.unwrap_err();
            assert_eq!(err.message, FROM_BROWSER);
        }
        let st: TerminalStatus = serde_json::from_value(
            e.terminal_request(Request::TerminalStatus, &web)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            !st.private,
            "a browser's connection never counts as private"
        );

        let (app, _rx) = e.terminal_conn(&phone, true);
        e.terminal_request(settings(TerminalAccess::Full), &app)
            .await
            .unwrap();
        let told = e.store().notifications(10).unwrap();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].title.contains("full access") && told[0].body.contains("from Phone"),
            "{told:?}"
        );
        // Narrowed from a plain connection: allowed, and nothing to tell.
        e.terminal_request(settings(TerminalAccess::Off), &plain)
            .await
            .unwrap();
        assert_eq!(e.0.terminal_config().access, TerminalAccess::Off);
        assert_eq!(e.store().notifications(10).unwrap().len(), 1);
    }

    /// Signed out: every machine hears of it, even one whose queue is full
    /// at that moment.
    #[tokio::test]
    async fn signing_out_reaches_a_machine_whose_queue_is_full() {
        let (e, _dir) = engine().await;
        let (m, _) = e.store().add_device_as("Desktop", true).unwrap();
        let (conn, mut rx) = e.node_conn(&m);
        hello(&e, &conn, TerminalAccess::Off).await;
        let tx = e.0.machine_tx(&m).unwrap();
        while tx.try_send(Event::Resync).is_ok() {}
        e.0.terminals_signed_out("phone");
        let mut told = false;
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            if let Event::NodeTerminal { msgs } = ev
                && msgs
                    .iter()
                    .any(|m| matches!(m, NodeTermDown::SignedOut { device } if device == "phone"))
            {
                told = true;
                break;
            }
        }
        assert!(told, "the machine was told");
    }

    /// Terminals turned on, or given more, on a machine (while connected,
    /// or while it was away): the user's devices hear of it each time;
    /// narrowing, or the same again, says nothing.
    #[tokio::test]
    async fn the_devices_hear_when_a_machine_turns_terminals_on() {
        let (e, _dir) = engine().await;
        let (m, _) = e.store().add_device_as("Desktop", true).unwrap();
        let told = || e.store().notifications(10).unwrap();
        let (conn, _rx) = e.node_conn(&m);
        hello(&e, &conn, TerminalAccess::Off).await;
        assert!(told().is_empty(), "{:?}", told());
        let scope = |terminal| Request::NodeScope {
            scope: MachineScope {
                terminal,
                ..MachineScope::default()
            },
            name: None,
        };
        e.node_request(scope(TerminalAccess::Sandboxed), &conn)
            .await
            .unwrap();
        let t = told();
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(t[0].title, "Terminals turned on");
        assert!(
            t[0].body
                .contains("Terminals on Desktop were turned on (sandboxed) on that computer"),
            "{t:?}"
        );
        e.node_request(scope(TerminalAccess::Off), &conn)
            .await
            .unwrap();
        assert_eq!(told().len(), 1);
        drop(conn);
        // Given full access there while it was away.
        let (conn, _rx) = e.node_conn(&m);
        hello(&e, &conn, TerminalAccess::Full).await;
        let t = told();
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(
            t.iter().any(|n| n.title == "Terminals with full access"
                && n.body.contains("given full access on that computer")),
            "{t:?}"
        );
        drop(conn);
        let (conn, _rx) = e.node_conn(&m);
        hello(&e, &conn, TerminalAccess::Full).await;
        assert_eq!(told().len(), 2, "the same again says nothing");
    }

    /// Typing for a machine's terminal goes out in order without holding up
    /// the device's other requests, even while that machine's link is full;
    /// past 1 MB still on its way, more is refused, and once the machine
    /// takes it, typing goes on.
    #[tokio::test]
    async fn typing_for_a_busy_machine_is_capped_and_holds_nothing_else_up() {
        let _dog = crate::terminal::watchdog("a machine typing test", 60);
        let (e, _dir) = engine().await;
        let (m, _) = e.store().add_device_as("Desktop", true).unwrap();
        let (node, mut rx) = e.node_conn(&m);
        hello(&e, &node, TerminalAccess::Sandboxed).await;
        let (phone, _) = e.store().add_device("Phone").unwrap();
        let (app, _out) = e.terminal_conn(&phone, true);
        // The machine opens it.
        let answer = async {
            loop {
                let Some(Event::NodeTerminal { msgs }) = rx.recv().await else {
                    continue;
                };
                for msg in msgs {
                    if let NodeTermDown::Open { id, .. } = msg {
                        e.node_request(
                            Request::NodeTerminal {
                                msgs: vec![NodeTermUp::Opened {
                                    id: id.clone(),
                                    shell: "bash".into(),
                                    sandboxed: true,
                                }],
                            },
                            &node,
                        )
                        .await
                        .unwrap();
                        return id;
                    }
                }
            }
        };
        let (opened, id) = tokio::join!(
            e.terminal_request(
                Request::TerminalOpen {
                    machine: Some(m.clone()),
                    cols: 80,
                    rows: 24,
                },
                &app,
            ),
            answer
        );
        opened.unwrap();
        // Its link is full: nothing reaches the machine for now.
        let tx = e.0.machine_tx(&m).unwrap();
        while tx.try_send(Event::Resync).is_ok() {}
        let piece = "x".repeat(terminal::MAX_INPUT);
        let input = || Request::TerminalInput {
            id: id.clone(),
            data: piece.clone(),
        };
        let t0 = std::time::Instant::now();
        let mut refused = None;
        for i in 0..40 {
            if let Err(err) = e.terminal_request(input(), &app).await {
                refused = Some((i, err));
                break;
            }
        }
        let (i, err) = refused.expect("refused past 1 MB");
        assert_eq!(i, MAX_PENDING_INPUT / terminal::MAX_INPUT, "{err:?}");
        assert_eq!(err.message, LINK_HELD_UP);
        // Other requests about it wait in the same line, so they can't pile
        // up either.
        let stuck = e
            .terminal_request(
                Request::TerminalResize {
                    id: id.clone(),
                    cols: 100,
                    rows: 30,
                },
                &app,
            )
            .await
            .unwrap_err();
        assert_eq!(stuck.message, LINK_STUCK);
        // Other requests go on meanwhile.
        let st = tokio::time::timeout(
            Duration::from_secs(2),
            e.terminal_request(Request::TerminalStatus, &app),
        )
        .await
        .expect("answered at once");
        assert!(st.is_ok(), "{st:?}");
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        // The machine takes it all, in order, and typing goes on.
        let mut got = 0;
        while got < i * terminal::MAX_INPUT {
            let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("the typing reaches the machine")
                .unwrap();
            if let Event::NodeTerminal { msgs } = ev {
                for msg in msgs {
                    if let NodeTermDown::Input { id: to, data } = msg {
                        assert_eq!(to, id);
                        got += data.len();
                    }
                }
            }
        }
        assert_eq!(got, i * terminal::MAX_INPUT);
        let mut again = Err(err);
        for _ in 0..50 {
            again = e.terminal_request(input(), &app).await;
            if again.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(again.is_ok(), "{again:?}");
        // Closing always goes, however much waits.
        while tx.try_send(Event::Resync).is_ok() {}
        while e.terminal_request(input(), &app).await.is_ok() {}
        e.terminal_request(Request::TerminalClose { id: id.clone() }, &app)
            .await
            .unwrap();
        assert_eq!(e.open_terminals(), 0);
    }

    /// Terminals never open from a browser; should one ever have, it ends
    /// when web browser access is switched off (others stay).
    #[tokio::test]
    async fn switching_web_access_off_ends_any_terminal_of_a_browser() {
        let _dog = crate::terminal::watchdog("a web access test", 60);
        let (e, dir) = engine().await;
        let (browser, _) = e.store().add_browser("Firefox", &[4u8; 65]).unwrap();
        let (phone, _) = e.store().add_device("Phone").unwrap();
        let (tx, _rx) = mpsc::channel(1024);
        shell(&e, "tb", &browser, dir.path(), &tx);
        shell(&e, "tp", &phone, dir.path(), &tx);
        e.handle(
            Request::SetWebAccess {
                on: false,
                token: None,
            },
            None,
        )
        .await
        .unwrap();
        let open: Vec<String> =
            e.0.terminals
                .host
                .list(None)
                .into_iter()
                .map(|i| i.id)
                .collect();
        assert_eq!(open, ["tp"]);
        e.0.terminals.host.close_all("the test is over");
    }

    /// The server's sandbox (agent commands and sandboxed terminals): the
    /// Tessera program and its service files are there to read, never to
    /// change, whatever folder rules say.
    #[tokio::test]
    async fn the_server_sandbox_keeps_tesseras_program_and_service_as_they_are() {
        let (e, dir) = engine().await;
        let exe = crate::platform::canonicalize(&std::env::current_exe().unwrap()).unwrap();
        let home = dirs::home_dir().unwrap();
        let systemd = home.join(".config/systemd");
        let kept = super::super::security::with_program_kept(
            vec![(systemd.join("user"), true), (home.join("work"), true)],
            crate::node::server_program_paths(Some(&home)),
        );
        assert!(kept.contains(&(exe.clone(), false)), "{kept:?}");
        assert!(kept.contains(&(systemd.clone(), false)), "{kept:?}");
        assert!(!kept.contains(&(systemd.join("user"), true)), "{kept:?}");
        assert!(kept.contains(&(home.join("work"), true)), "{kept:?}");

        // The agent's file tools can't change them either, whatever the
        // permission mode; other files as usual. With the sandbox off (the
        // user's choice), as before.
        let ctx = crate::tools::ToolCtx {
            session_id: "s".into(),
            call_id: "c".into(),
            seq: 0,
            cwd: dir.path().to_path_buf(),
            cancel: Default::default(),
            host: Arc::new(super::super::Host(e.0.clone(), false)),
            progress: Arc::new(|_: &str| {}),
            depth: 0,
            cleared: None,
            admission: Default::default(),
        };
        let blocked = |p: &Path| {
            matches!(
                crate::tools::files::path_write_risk(p, &ctx),
                crate::tools::Risk::Block { reason } if reason.contains("Tessera's own program")
            )
        };
        let unit = systemd.join("user/tessera.service");
        assert!(blocked(&exe) && blocked(&unit));
        assert!(!blocked(&dir.path().join("work/a.txt")));
        e.0.config.write().security.sandbox = SandboxMode::Off;
        assert!(!blocked(&exe) && !blocked(&unit));
        e.0.config.write().security.sandbox = SandboxMode::Auto;

        let Some((spec, _)) = e.0.sandbox_spec() else {
            eprintln!("the sandbox is off here");
            return;
        };
        assert!(spec.folders.contains(&(exe.clone(), false)), "{spec:?}");
        if !crate::sandbox::support().available {
            eprintln!("sandbox unavailable here: {}", crate::sandbox::describe());
            return;
        }
        // Commands in it can run and read the program, not change it.
        use std::os::unix::process::CommandExt;
        let mut spec = spec;
        spec.group = "program kept test".into();
        let plan = crate::sandbox::Prepared::new(&spec, dir.path()).unwrap();
        let x = exe.display();
        let (program, args) = plan.argv(
            "/bin/sh".into(),
            vec![
                "-c".into(),
                format!(
                    "test -r '{x}' && echo readable; test -w '{x}' && echo WRITABLE || echo kept"
                )
                .into(),
            ],
        );
        let mut cmd = std::process::Command::new(program);
        cmd.args(args).current_dir(dir.path());
        plan.protect_environment(&mut cmd).unwrap();
        // SAFETY: the sandbox's `enter` is written for this place.
        unsafe { cmd.pre_exec(move || plan.enter()) };
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("readable") && text.contains("kept"), "{text}");
        assert!(!text.contains("WRITABLE"), "{text}");
    }
}
