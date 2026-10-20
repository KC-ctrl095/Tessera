//! Machines, server side: the computers that run `tessera node` and offer
//! themselves to this server (see `crate::node` for their side).
//!
//! A machine is a paired device whose connection said `node_hello`. Its
//! actions go to that connection alone (`node_call`, like web view frames
//! go only to the connection that attached), and only its answers on that
//! same connection count. The server keeps what each machine reported
//! (shown in Settings → Devices and to the `machines` tool) but never
//! decides what a machine allows: the machine checks every action itself.
//! Devices can pause, resume and revoke machines; nothing here can turn a
//! machine on or widen it.
//!
//! Who may use a machine ("any chat", or only certain bots) is the
//! machine's own setting too; the server checks it before anything is sent
//! (the acting bot is the session's: a subagent's root bot, a scheduled
//! duty's bot) and tells the machine, which checks again. Each bot also has
//! its own access per machine (`Bot::machines`: none, read only, or as the
//! machine allows), set on the server and checked here: it can only narrow
//! what the machine allows, never widen it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::*;
use crate::tools::machines::MachineCall;

const KV_MACHINES: &str = "machines";
const KV_REVOKED: &str = "machines_revoked";
/// Actions waiting to be sent to one machine before it counts as stuck.
const QUEUE: usize = 64;
/// Recent actions remembered per machine (Settings → Devices).
const RECENT: usize = 50;
/// How long past its own time limit a machine may take to answer.
const GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// This computer's key (`NodeHello::computer` of a machine on it).
fn this_computer() -> Option<&'static String> {
    static MINE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    MINE.get_or_init(tessera_secure::computer_key).as_ref()
}

/// What the server remembers of a machine (`kv` "machines").
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    name: String,
    os: String,
    version: String,
    scope: MachineScope,
    paused: bool,
    last_seen: u64,
    generation: u64,
    #[serde(default)]
    sandbox: String,
    #[serde(default)]
    sandbox_ok: bool,
    #[serde(default)]
    home: Option<String>,
    /// Newest first.
    #[serde(default)]
    recent: Vec<MachineActivity>,
    /// Built backends; absent on older nodes, empty means unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    features: Option<Vec<String>>,
    /// Which computer it is (`NodeHello::computer`): one that is this
    /// server's own computer stops this server's agents too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    computer: Option<String>,
    /// Tessera Guard's public key on that computer (hex), pinned when first
    /// reported: every action there waits for its signed word that the
    /// computer isn't stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guard_key: Option<String>,
    /// It reported another key (or none) since: nothing runs there until
    /// the user adds the machine again.
    #[serde(default)]
    guard_changed: bool,
    /// The newest Stop number the guard signed for: an older one is never
    /// accepted (a replayed word).
    #[serde(default)]
    guard_generation: u64,
    /// It said it has Tessera Guard but the guard couldn't sign (no key
    /// yet): nothing runs there until it can.
    #[serde(default)]
    guard_required: bool,
}

/// The guard's word checked for one action (see `guard_says_go`): the
/// connection it came on, the key that signed it and its Stop number.
struct Word {
    conn: u64,
    key: String,
    generation: u64,
}

impl Record {
    /// Whether this record still says what `word` was checked against
    /// (without a word: that the machine needs none), for a call sent on
    /// `conn`. Any newer word of its guard (a Stop seen for another call
    /// among them), another key or another connection: no.
    fn still_as_checked(&self, word: Option<&Word>, conn: u64) -> bool {
        match word {
            None => self.guard_key.is_none() && !self.guard_required && !self.guard_changed,
            Some(w) => {
                w.conn == conn
                    && !self.guard_changed
                    && self.guard_key.as_deref() == Some(w.key.as_str())
                    && self.guard_generation == w.generation
            }
        }
    }
}

/// A guard's signed word asked of a machine (`node_attest`), by nonce.
struct Attesting {
    device: String,
    conn: u64,
    tx: oneshot::Sender<(crate::guard::State, String)>,
}

struct Live {
    conn: u64,
    tx: mpsc::Sender<Event>,
    /// Ends the connection (see [`must_reach`]).
    kill: CancellationToken,
}

/// A control the machine must get (pause, revoke, stopping an action): its
/// queue being full can't drop it, so the connection is cut instead (the
/// machine ends everything it runs when its link goes, and what was queued
/// for it is never delivered).
fn must_reach(tx: &mpsc::Sender<Event>, kill: &CancellationToken, event: Event) {
    if tx.try_send(event).is_err() {
        tracing::warn!("a machine's queue is full: cutting its connection so a control isn't lost");
        kill.cancel();
    }
}

/// Live output of a running call: (text, stream).
type Progress = Arc<dyn Fn(&str, &str) + Send + Sync>;

struct Waiting {
    device: String,
    conn: u64,
    tx: oneshot::Sender<NodeResult>,
    progress: Progress,
}

pub(crate) struct Machines {
    records: Mutex<Vec<Record>>,
    /// Device id → the generation that was revoked.
    revoked: Mutex<HashMap<String, u64>>,
    live: Mutex<HashMap<String, Live>>,
    calls: Mutex<HashMap<String, Waiting>>,
    attests: Mutex<HashMap<String, Attesting>>,
    conns: AtomicU64,
    /// Device id → its open connections (Settings → Devices: online).
    devices: Mutex<HashMap<String, usize>>,
}

/// The `NodeHello::features` this server knows.
const KNOWN_FEATURES: &[&str] = &[
    "desktop",
    "desktop_session",
    "terminal",
    "admin",
    "sandbox",
    "webviews",
];

impl Machines {
    pub(crate) fn load(store: &Store) -> Self {
        let get = |k: &str| store.kv_get(k).ok().flatten().unwrap_or_default();
        Self {
            records: Mutex::new(serde_json::from_str(&get(KV_MACHINES)).unwrap_or_default()),
            revoked: Mutex::new(serde_json::from_str(&get(KV_REVOKED)).unwrap_or_default()),
            live: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
            attests: Mutex::new(HashMap::new()),
            conns: AtomicU64::new(1),
            devices: Mutex::new(HashMap::new()),
        }
    }

    /// A machine can use its desktop and has desktop control on (the
    /// `desktop` tool is offered).
    /// A machine that is this very computer, connected now, says its user
    /// pressed Stop there (its Stop bar, even when `stop.json` couldn't be
    /// written). Only connected ones: a machine gone since never keeps this
    /// server stopped. It can only stop this server, never resume or allow
    /// anything.
    pub(crate) fn this_computer_stopped(&self) -> bool {
        let Some(mine) = this_computer() else {
            return false;
        };
        let live: Vec<String> = self.live.lock().keys().cloned().collect();
        self.records
            .lock()
            .iter()
            .any(|r| r.scope.stopped && r.computer.as_ref() == Some(mine) && live.contains(&r.id))
    }

    /// Every machine's Stop count, by id (`Inner::stops_here`).
    pub(crate) fn stop_counts(&self) -> std::collections::BTreeMap<String, u32> {
        self.records
            .lock()
            .iter()
            .map(|r| (r.id.clone(), r.scope.stops))
            .collect()
    }

    /// This very computer's machines and their Stop counts: a Stop there
    /// stops this server's agents too.
    pub(crate) fn this_computer_counts(&self) -> Vec<(String, u32)> {
        let Some(mine) = this_computer() else {
            return Vec::new();
        };
        self.records
            .lock()
            .iter()
            .filter(|r| r.computer.as_ref() == Some(mine))
            .map(|r| (r.id.clone(), r.scope.stops))
            .collect()
    }

    /// Device `id` serves as one of the machines.
    pub(crate) fn has(&self, id: &str) -> bool {
        self.records.lock().iter().any(|r| r.id == id)
    }
}

/// A connection's standing as a machine; made for every connection (see
/// [`Engine::node_conn`]), it counts once the connection said `node_hello`.
/// Dropping it takes the machine offline and fails what it was running.
pub struct NodeConn {
    id: u64,
    device: String,
    tx: mpsc::Sender<Event>,
    hello: AtomicBool,
    engine: Weak<Inner>,
    kill: CancellationToken,
}

impl NodeConn {
    /// This connection is a machine: it gets only its own `node_*` events,
    /// not the chats' broadcast.
    pub fn is_node(&self) -> bool {
        self.hello.load(Ordering::SeqCst)
    }

    /// Done when a control couldn't be queued for this machine: the
    /// connection must end now (see `must_reach`).
    pub async fn cut(&self) {
        self.kill.cancelled().await
    }
}

impl Drop for NodeConn {
    fn drop(&mut self) {
        let Some(inner) = self.engine.upgrade() else {
            return;
        };
        if self.is_node() {
            inner.node_gone(&self.device, self.id);
        }
        let gone = {
            let mut d = inner.machines.devices.lock();
            match d.get_mut(&self.device) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    false
                }
                Some(_) => {
                    d.remove(&self.device);
                    true
                }
                None => false,
            }
        };
        if gone {
            inner.emit_machines();
        }
    }
}

/// Requests only a machine's own connection may send ([`Engine::node_request`]).
pub fn is_node_request(req: &Request) -> bool {
    matches!(
        req,
        Request::NodeHello { .. }
            | Request::NodeScope { .. }
            | Request::NodeProgress { .. }
            | Request::NodeReply { .. }
            | Request::NodeAttestation { .. }
            | Request::NodeBrowser { .. }
            | Request::NodeTerminal { .. }
    )
}

impl Engine {
    /// A device connection's machine side: `node_*` events for it arrive on
    /// the receiver. Pass it to [`Engine::node_request`].
    pub fn node_conn(&self, device: &str) -> (NodeConn, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(QUEUE);
        let id = self.0.machines.conns.fetch_add(1, Ordering::Relaxed);
        let first = {
            let mut d = self.0.machines.devices.lock();
            let n = d.entry(device.to_string()).or_insert(0);
            *n += 1;
            *n == 1
        };
        if first {
            self.0.emit_machines();
        }
        (
            NodeConn {
                id,
                device: device.to_string(),
                tx,
                hello: AtomicBool::new(false),
                engine: Arc::downgrade(&self.0),
                kill: CancellationToken::new(),
            },
            rx,
        )
    }

    /// A `node_*` request from the machine on `conn`.
    pub async fn node_request(&self, req: Request, conn: &NodeConn) -> ApiResult {
        self.0.node_request(req, conn)
    }
}

fn clean(s: &str, max: usize) -> String {
    crate::util::one_line(&crate::util::visible_controls(s), max)
}

/// A scope as reported, kept within bounds (it is shown, never trusted).
fn bounded(mut scope: MachineScope) -> MachineScope {
    scope.folders.truncate(crate::node::MAX_FOLDERS);
    for f in &mut scope.folders {
        f.path = crate::util::truncate(&crate::util::visible_controls(&f.path), 1024);
    }
    scope.timeout_secs = crate::node::clamp_timeout(scope.timeout_secs);
    scope
}

impl Inner {
    pub(super) fn emit_machines(&self) {
        self.invalidate_prompts();
        let _ = self.events.send(Event::MachinesChanged);
    }

    fn save_machines(&self) {
        let records = serde_json::to_string(&*self.machines.records.lock()).unwrap_or_default();
        let revoked = serde_json::to_string(&*self.machines.revoked.lock()).unwrap_or_default();
        let _ = self.store.kv_set(KV_MACHINES, &records);
        let _ = self.store.kv_set(KV_REVOKED, &revoked);
    }

    fn info_of(&self, r: &Record) -> MachineInfo {
        let online = self.machines.live.lock().contains_key(&r.id);
        let running = self
            .machines
            .calls
            .lock()
            .values()
            .filter(|c| c.device == r.id)
            .count() as u32;
        MachineInfo {
            id: r.id.clone(),
            name: r.name.clone(),
            os: r.os.clone(),
            version: r.version.clone(),
            online,
            last_seen: if online { now_ms() } else { r.last_seen },
            paused: r.paused,
            scope: r.scope.clone(),
            sandbox: r.sandbox.clone(),
            sandbox_ok: r.sandbox_ok,
            running,
            preset: crate::node::preset_of(&r.scope, r.home.as_deref().map(Path::new)),
            home: r.home.clone(),
            recent: r.recent.clone(),
            features: r.features.clone(),
        }
    }

    /// Mark the devices connected right now.
    pub(super) fn mark_online(&self, list: &mut [DeviceInfo]) {
        let d = self.machines.devices.lock();
        for x in list {
            x.online = d.get(&x.id).is_some_and(|n| *n > 0);
        }
    }

    pub(super) fn machine_list(&self) -> Vec<MachineInfo> {
        let records = self.machines.records.lock().clone();
        records.iter().map(|r| self.info_of(r)).collect()
    }

    pub(super) fn machines_request(&self, req: Request) -> ApiResult {
        match req {
            Request::ListMachines => to_value(self.machine_list()),
            Request::SetMachinePaused { id, paused } => {
                let rec = {
                    let mut records = self.machines.records.lock();
                    let r = records
                        .iter_mut()
                        .find(|r| r.id == id)
                        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such machine"))?;
                    r.paused = paused;
                    r.clone()
                };
                self.save_machines();
                // Sent outside the lock (a cut is logged).
                let live = self
                    .machines
                    .live
                    .lock()
                    .get(&id)
                    .map(|l| (l.tx.clone(), l.kill.clone()));
                if let Some((tx, kill)) = live {
                    must_reach(
                        &tx,
                        &kill,
                        Event::NodeControl {
                            paused,
                            revoked: false,
                        },
                    );
                }
                if paused {
                    self.fail_calls(&id, None, &format!("{} was paused from a device", rec.name));
                    self.browser
                        .machine_gone(&id, &format!("{} was paused from a device", rec.name));
                    self.terminals_machine_gone(
                        &id,
                        &format!("{} was paused from a device", rec.name),
                    );
                }
                self.emit_machines();
                to_value(self.info_of(&rec))
            }
            Request::RevokeMachine { id } => {
                if !self.revoke_machine(&id) {
                    return Err(ApiError::new(ErrorCode::NotFound, "no such machine"));
                }
                Ok(Value::Null)
            }
            _ => Err(bad("not a machines request")),
        }
    }

    /// Turn a machine off from here: it stops, forgets it was on, and only
    /// that computer can turn it on again. False if unknown.
    pub(super) fn revoke_machine(&self, id: &str) -> bool {
        let rec = {
            let mut records = self.machines.records.lock();
            let Some(i) = records.iter().position(|r| r.id == id) else {
                return false;
            };
            records.remove(i)
        };
        self.machines
            .revoked
            .lock()
            .insert(id.to_string(), rec.generation);
        self.save_machines();
        let gone = self.machines.live.lock().remove(id);
        if let Some(l) = gone {
            must_reach(
                &l.tx,
                &l.kill,
                Event::NodeControl {
                    paused: false,
                    revoked: true,
                },
            );
        }
        self.fail_calls(
            id,
            None,
            &format!("{} was turned off from a device", rec.name),
        );
        self.browser
            .machine_gone(id, &format!("{} was turned off from a device", rec.name));
        self.terminals_machine_gone(id, &format!("{} was turned off from a device", rec.name));
        // Credentials allowed on it aren't any more (turned on again, it starts without).
        self.auth.forget(id);
        self.forget_secret_machine(id);
        tracing::info!("machine `{}` ({id}) revoked", rec.name);
        self.emit_machines();
        true
    }

    /// Fail the actions waiting on `device` (on connection `conn`, or any).
    fn fail_calls(&self, device: &str, conn: Option<u64>, why: &str) {
        let gone: Vec<Waiting> = {
            let mut calls = self.machines.calls.lock();
            let ids: Vec<String> = calls
                .iter()
                .filter(|(_, c)| c.device == device && conn.is_none_or(|n| n == c.conn))
                .map(|(k, _)| k.clone())
                .collect();
            ids.iter().filter_map(|k| calls.remove(k)).collect()
        };
        for w in gone {
            let _ = w.tx.send(NodeResult {
                output: why.to_string(),
                ok: false,
                refused: false,
                exit_code: None,
                image: None,
            });
        }
    }

    fn node_gone(&self, device: &str, conn: u64) {
        let was = {
            let mut live = self.machines.live.lock();
            match live.get(device) {
                Some(l) if l.conn == conn => {
                    live.remove(device);
                    true
                }
                _ => false,
            }
        };
        if !was {
            return;
        }
        let name = {
            let mut records = self.machines.records.lock();
            match records.iter_mut().find(|r| r.id == device) {
                Some(r) => {
                    r.last_seen = now_ms();
                    r.name.clone()
                }
                None => "the machine".into(),
            }
        };
        self.save_machines();
        self.fail_calls(
            device,
            Some(conn),
            &format!(
                "Lost the connection to {name} while the action ran; it may or may not have finished, so check before repeating it."
            ),
        );
        self.browser
            .machine_gone(device, &format!("{name} went offline"));
        self.terminals_machine_gone(device, &format!("{name} went offline"));
        self.emit_machines();
    }

    /// The live connection of machine `id` (its `node_*` events go there).
    pub(super) fn machine_tx(&self, id: &str) -> Option<mpsc::Sender<Event>> {
        self.machines.live.lock().get(id).map(|l| l.tx.clone())
    }

    /// Pairing again changes the login id, not the computer. Replace only
    /// machines of the same client kind with the same reported key. An
    /// app and a separately paired terminal on one computer stay separate.
    /// Names are never evidence.
    pub(super) fn replace_machine_login(&self, device: &str) -> Result<Option<String>, ApiError> {
        let rows = self.store.device_rows(None).map_err(internal)?;
        let Some((me, Some(key))) = rows.iter().find(|(d, _)| d.id == device) else {
            return Ok(None);
        };
        let ids: Vec<String> = rows
            .iter()
            .filter(|(d, k)| d.id != device && d.client == me.client && k.as_ref() == Some(key))
            .map(|(d, _)| d.id.clone())
            .collect();
        let replaced = {
            let mut records = self.machines.records.lock();
            let mut replaced = Vec::new();
            records.retain(|r| {
                if ids.contains(&r.id) {
                    replaced.push(r.clone());
                    false
                } else {
                    true
                }
            });
            // The earliest entry keeps its name, pause choice and history.
            // Scope is still reported afresh by the computer itself.
            if let Some(old) = replaced.first() {
                if let Some(current) = records.iter_mut().find(|r| r.id == device) {
                    current.name = old.name.clone();
                    current.paused = old.paused;
                    current.recent = old.recent.clone();
                } else {
                    records.push(Record {
                        id: device.into(),
                        ..old.clone()
                    });
                }
            }
            replaced
        };
        if let Some(previous) = replaced
            .first()
            .and_then(|old| rows.iter().find(|(d, _)| d.id == old.id))
            .map(|(d, _)| d)
        {
            if previous.named {
                self.store
                    .rename_device(device, &previous.name)
                    .map_err(internal)?;
            }
            if !previous.notify {
                self.store
                    .set_device_notify(device, false)
                    .map_err(internal)?;
            }
        }
        for old in &replaced {
            let why = format!(
                "{} paired again while the action ran; check before repeating it.",
                old.name
            );
            self.machines.live.lock().remove(&old.id);
            self.fail_calls(&old.id, None, &why);
            self.browser.machine_gone(&old.id, &why);
            self.terminals_machine_gone(&old.id, &why);
            // Close the replaced connection without NodeControl::revoked:
            // that would switch off the new login's shared local settings.
            for id in self.store.private_of(&old.id) {
                self.store.set_private_owner(&id, Some(device));
            }
            self.revoke_device(&old.id)?;
            self.auth.forget(&old.id);
            self.forget_secret_machine(&old.id);
        }
        if let Some(old) = replaced.first() {
            if let Some(tx) = self.machine_tx(device) {
                let _ = tx.try_send(Event::NodeControl {
                    paused: old.paused,
                    revoked: false,
                });
            }
            self.save_machines();
            self.emit_machines();
        }
        Ok(replaced.first().map(|r| r.name.clone()))
    }
    fn node_request(&self, req: Request, conn: &NodeConn) -> ApiResult {
        if let Request::NodeHello {
            name,
            os,
            version,
            scope,
            generation,
            sandbox,
            sandbox_ok,
            home,
            computer,
            host,
            features,
            guard_key,
        } = req
        {
            let home = home
                .map(|h| clean(&h, 400))
                .filter(|h| h.starts_with('/') && h.len() > 1);
            // Which computer it is (Settings → Devices shows it with that
            // computer's app and terminal), kept before the hello announces
            // it; never its name or what it allows.
            let computer = computer.filter(|k| {
                (16..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_hexdigit())
            });
            let host = host.map(|h| clean(&h, 64)).filter(|h| !h.is_empty());
            if computer.is_some() || host.is_some() {
                let _ = self.store.report_device(
                    &conn.device,
                    None,
                    computer.as_deref(),
                    host.as_deref(),
                    None,
                );
            }
            let name = self.replace_machine_login(&conn.device)?.unwrap_or(name);
            // Known words only, a few.
            let features = features.map(|fs| {
                fs.into_iter()
                    .filter(|f| KNOWN_FEATURES.contains(&f.as_str()))
                    .take(8)
                    .collect()
            });
            // A key (64 hex digits), or "" for a guard that can't sign yet.
            let guard_key = guard_key
                .filter(|k| k.is_empty() || (k.len() == 64 && crate::guard::unhex(k).is_some()));
            return self.node_hello(
                conn, name, os, version, scope, generation, sandbox, sandbox_ok, home, features,
                computer, guard_key,
            );
        }
        if !conn.is_node()
            || self.machines.live.lock().get(&conn.device).map(|l| l.conn) != Some(conn.id)
        {
            return Err(bad(
                "this connection isn't a machine (send node_hello first)",
            ));
        }
        match req {
            Request::NodeScope { scope, name } => {
                let name = name
                    .map(|n| clean(&n, 60))
                    .filter(|n| !n.is_empty())
                    .map(|n| self.unique_machine_name(&conn.device, n));
                let (mut widened, mut renamed) = (None, None);
                if let Some(r) = self
                    .machines
                    .records
                    .lock()
                    .iter_mut()
                    .find(|r| r.id == conn.device)
                {
                    let was = r.scope.terminal;
                    // Its count of Stops never goes down here: a report
                    // overtaken by a newer one can't undo a Stop.
                    let stops = r.scope.stops;
                    r.scope = bounded(scope.clone());
                    r.scope.stops = r.scope.stops.max(stops);
                    if let Some(n) = name.filter(|n| *n != r.name) {
                        renamed = Some(format!(
                            "machine `{}` ({}) renamed itself to `{n}`",
                            r.name, r.id
                        ));
                        r.name = n;
                    }
                    if super::terminals::widens(was, scope.terminal) {
                        widened = Some(r.name.clone());
                    }
                }
                // Logged outside the record's lock (writing a log can wait).
                if let Some(line) = renamed {
                    tracing::info!("{line}");
                }
                // Turned on there: the user's devices are told all the same.
                if let Some(n) = widened {
                    self.tell_terminals_widened(&n, scope.terminal, "on that computer");
                }
                if !scope.webviews {
                    self.browser
                        .machine_gone(&conn.device, "web views were switched off on it");
                }
                if scope.terminal == TerminalAccess::Off {
                    self.terminals_machine_gone(&conn.device, "terminals were switched off on it");
                }
                self.save_machines();
                self.emit_machines();
                Ok(Value::Null)
            }
            Request::NodeProgress {
                call_id,
                chunk,
                stream,
            } => {
                let progress = self
                    .machines
                    .calls
                    .lock()
                    .get(&call_id)
                    .filter(|c| c.device == conn.device && c.conn == conn.id)
                    .map(|c| c.progress.clone());
                if let Some(p) = progress {
                    p(&chunk, stream.as_deref().unwrap_or("legacy"));
                }
                Ok(Value::Null)
            }
            Request::NodeAttestation {
                nonce,
                stopped,
                stops,
                generation,
                signature,
            } => {
                let waiting = {
                    let mut a = self.machines.attests.lock();
                    match a.get(&nonce) {
                        Some(w) if w.device == conn.device && w.conn == conn.id => a.remove(&nonce),
                        _ => None,
                    }
                };
                if let Some(w) = waiting {
                    let _ = w.tx.send((
                        crate::guard::State {
                            stopped,
                            stops,
                            generation,
                        },
                        signature,
                    ));
                }
                Ok(Value::Null)
            }
            Request::NodeReply { call_id, result } => {
                let waiting = {
                    let mut calls = self.machines.calls.lock();
                    match calls.get(&call_id) {
                        Some(c) if c.device == conn.device && c.conn == conn.id => {
                            calls.remove(&call_id)
                        }
                        _ => None,
                    }
                };
                if let Some(w) = waiting {
                    let _ = w.tx.send(result);
                }
                Ok(Value::Null)
            }
            // Its web view browser's news (engine/webview.rs).
            Request::NodeBrowser { seq, msgs } => {
                self.node_browser(&conn.device, seq, msgs);
                Ok(Value::Null)
            }
            // Its terminals' news (engine/terminals.rs).
            Request::NodeTerminal { msgs } => {
                self.node_terminal(&conn.device, msgs);
                Ok(Value::Null)
            }
            _ => Err(bad("not a machine request")),
        }
    }

    /// `name`, or "name (2)"…: names are how approvals say where something
    /// runs, so no two machines share one.
    fn unique_machine_name(&self, device: &str, name: String) -> String {
        let records = self.machines.records.lock();
        let taken = |n: &str| {
            records
                .iter()
                .any(|r| r.id != device && r.name.eq_ignore_ascii_case(n))
        };
        let mut unique = name.clone();
        let mut i = 2;
        while taken(&unique) {
            unique = format!("{name} ({i})");
            i += 1;
        }
        unique
    }

    #[allow(clippy::too_many_arguments)]
    fn node_hello(
        &self,
        conn: &NodeConn,
        name: String,
        os: String,
        version: String,
        scope: MachineScope,
        generation: u64,
        sandbox: String,
        sandbox_ok: bool,
        home: Option<String>,
        features: Option<Vec<String>>,
        computer: Option<String>,
        guard_key: Option<String>,
    ) -> ApiResult {
        let device = conn.device.clone();
        if let Some(&g) = self.machines.revoked.lock().get(&device)
            && generation <= g
        {
            return Err(ApiError::new(
                ErrorCode::Unauthorized,
                "this machine was turned off from another device; turn it on again on that computer",
            ));
        }
        let name = Some(clean(&name, 60))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Machine".into());
        let name = self.unique_machine_name(&device, name);
        let record = Record {
            id: device.clone(),
            name: name.clone(),
            os: clean(&os, 60),
            version: clean(&version, 30),
            scope: bounded(scope),
            paused: false,
            last_seen: now_ms(),
            generation,
            sandbox: clean(&sandbox, 200),
            sandbox_ok,
            home,
            recent: Vec::new(),
            features,
            computer,
            guard_key: guard_key.clone().filter(|k| !k.is_empty()),
            guard_changed: false,
            guard_generation: 0,
            guard_required: guard_key.is_some(),
        };
        let terminal = record.scope.terminal;
        let mut other_key = None;
        let (paused, was) = {
            let mut records = self.machines.records.lock();
            match records.iter_mut().find(|r| r.id == device) {
                Some(r) => {
                    let (paused, was) = (r.paused, r.scope.terminal);
                    let recent = std::mem::take(&mut r.recent);
                    // The guard's key, pinned when first reported: another
                    // one (or none) since stops everything there until the
                    // user adds the machine again.
                    // A guard said before ("" or a key) and none now: changed.
                    let was_required = r.guard_required || r.guard_key.is_some();
                    let reported = guard_key.clone().filter(|k| !k.is_empty());
                    let (key, changed) = match (r.guard_key.take(), reported) {
                        (Some(k), Some(h)) if k == h => (Some(k), r.guard_changed),
                        (Some(k), _) => {
                            other_key = Some(format!("machine `{}` ({})", r.name, r.id));
                            (Some(k), true)
                        }
                        (None, Some(h)) => (Some(h), r.guard_changed),
                        (None, None) => (
                            None,
                            r.guard_changed || (was_required && guard_key.is_none()),
                        ),
                    };
                    let guard_generation = r.guard_generation;
                    *r = Record {
                        paused,
                        recent,
                        guard_key: key,
                        guard_changed: changed,
                        guard_generation,
                        guard_required: was_required || guard_key.is_some(),
                        ..record
                    };
                    (paused, was)
                }
                None => {
                    records.push(record);
                    (false, TerminalAccess::Off)
                }
            }
        };
        // Logged outside the record's lock (writing a log can wait).
        if let Some(which) = other_key {
            tracing::warn!(
                "{which} reported another Tessera Guard key than the one pinned; its actions are refused until it's added again"
            );
        }
        // Changed there while it was away: the user's devices are told.
        if super::terminals::widens(was, terminal) {
            self.tell_terminals_widened(&name, terminal, "on that computer");
        }
        self.machines.revoked.lock().remove(&device);
        let old = self.machines.live.lock().insert(
            device.clone(),
            Live {
                conn: conn.id,
                tx: conn.tx.clone(),
                kill: conn.kill.clone(),
            },
        );
        conn.hello.store(true, Ordering::SeqCst);
        // Its web view browser's news counts afresh on this connection.
        self.browser.machine_hello(&device);
        if let Some(old) = old.filter(|o| o.conn != conn.id) {
            self.fail_calls(
                &device,
                Some(old.conn),
                &format!("{name} reconnected while the action ran; it may or may not have finished, so check before repeating it."),
            );
        }
        self.save_machines();
        tracing::info!("machine `{name}` ({device}) is online");
        self.emit_machines();
        to_value(NodeWelcome {
            machine_id: device,
            paused,
            server: self.config.read().server.name.clone(),
        })
    }

    /// A machine with Tessera Guard (a pinned key): ask its guard's signed
    /// word on the Stop for a fresh nonce, check it with the pinned key, and
    /// go ahead only if it says not stopped (and isn't older than one seen).
    /// A machine without a guard key: nothing to ask.
    async fn guard_says_go(
        &self,
        machine: &str,
        name: &str,
        conn: u64,
        tx: &(mpsc::Sender<Event>, CancellationToken),
    ) -> Result<Option<Word>, String> {
        let (key, changed, required) = {
            let records = self.machines.records.lock();
            let Some(r) = records.iter().find(|r| r.id == machine) else {
                return Err("there is no such machine (any more)".into());
            };
            (r.guard_key.clone(), r.guard_changed, r.guard_required)
        };
        let Some(key) = key else {
            if changed || required {
                return Err(
                    "Tessera Guard on that computer can't sign its word on the Stop, so nothing runs there until it can (it says why on that computer: `tessera guard status`)"
                        .into(),
                );
            }
            return Ok(None);
        };
        if changed {
            return Err(format!(
                "Tessera Guard on that computer isn't the one it had (reinstalled, or something else speaking for {name}), so nothing runs there: remove the machine in Settings → Devices and add it again if this is expected"
            ));
        }
        let nonce = new_id();
        let (atx, arx) = oneshot::channel();
        self.machines.attests.lock().insert(
            nonce.clone(),
            Attesting {
                device: machine.to_string(),
                conn,
                tx: atx,
            },
        );
        if tx
            .0
            .try_send(Event::NodeAttest {
                nonce: nonce.clone(),
            })
            .is_err()
        {
            self.machines.attests.lock().remove(&nonce);
            return Err("it isn't keeping up right now; try again shortly".into());
        }
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), arx).await;
        self.machines.attests.lock().remove(&nonce);
        let (state, signature) = match got {
            Ok(Ok(a)) => a,
            _ => {
                return Err(
                    "Tessera Guard there didn't confirm that the computer isn't stopped, so nothing was done"
                        .into(),
                );
            }
        };
        if !crate::guard::verify_attestation(&key, &nonce, state, &signature) {
            return Err(
                "the answer about the computer's Stop wasn't Tessera Guard's own, so nothing was done"
                    .into(),
            );
        }
        // Checked against the record as it is now (another answer, a Stop
        // or a changed key may have come while this one was awaited), and
        // counted: every word of the guard's, a Stop's too, raises the
        // number older words are refused below.
        {
            let mut records = self.machines.records.lock();
            let Some(r) = records.iter_mut().find(|r| r.id == machine) else {
                return Err("there is no such machine (any more)".into());
            };
            if r.guard_changed || r.guard_key.as_deref() != Some(key.as_str()) {
                return Err(
                    "Tessera Guard on that computer changed while it was asked, so nothing was done"
                        .into(),
                );
            }
            if state.generation < r.guard_generation {
                return Err(
                    "Tessera Guard's word was older than one already seen, so nothing was done"
                        .into(),
                );
            }
            r.guard_generation = state.generation;
        }
        if state.stopped {
            return Err(
                "Tessera was stopped on that computer, so this was not done. Don't retry; tell the user (they resume it on that computer)."
                    .into(),
            );
        }
        Ok(Some(Word {
            conn,
            key,
            generation: state.generation,
        }))
    }

    /// The chat's title and mode, and the bot acting in it, as a machine is
    /// told them.
    pub(super) fn chat_of(&self, session_id: &str) -> (String, PermissionMode, Option<NodeBot>) {
        self.rt(session_id)
            .map(|rt| {
                let st = rt.st.lock();
                let bot = self.speaker_bot(&st).map(|b| NodeBot {
                    id: b.id.clone(),
                    handle: b.handle.clone(),
                    name: b.name.clone(),
                });
                (st.info.title.clone(), self.effective_permission(&st), bot)
            })
            .unwrap_or_else(|_| (String::new(), PermissionMode::ReadOnly, None))
    }
    /// Tool discovery uses this state's actor directly, never locks it again.
    pub(super) fn usable_machines(&self, st: &RtState, bot: Option<&Bot>) -> (bool, bool) {
        if st.info.private || st.info.internal || (self.speaker_id(st).is_some() && bot.is_none()) {
            return (false, false);
        }
        let read_only = self.effective_permission(st) == PermissionMode::ReadOnly;
        self.machines
            .records
            .lock()
            .iter()
            .fold((false, false), |(files, desktop), r| {
                let access = bot.map_or(MachineAccess::Allowed, |b| b.machine_access(&r.id));
                if access == MachineAccess::NoAccess
                    || crate::node::may_use(
                        &r.scope,
                        bot.map(|b| (b.id.as_str(), b.handle.as_str())),
                    )
                    .is_err()
                {
                    return (files, desktop);
                }
                let exec = access == MachineAccess::Allowed && !read_only && !r.scope.stopped;
                let screen = access == MachineAccess::Allowed
                    && !r.scope.stopped
                    && r.scope.desktop
                    && r.features.iter().flatten().any(|f| f == "desktop");
                (
                    files
                        || (!r.scope.stopped && !r.scope.folders.is_empty())
                        || (exec && r.scope.commands)
                        || screen,
                    desktop || screen,
                )
            })
    }

    /// May the session use machine `machine`, and how far? The machine's
    /// "who can use it" against the session's acting bot (a subagent's root
    /// bot, a scheduled duty's bot; none for a plain chat), then that bot's
    /// own access to it (`Bot::machines`), which only ever narrows what the
    /// machine allows. `Err` says why not, in plain words, without the
    /// machine's name.
    pub(super) fn machine_access(
        &self,
        session_id: &str,
        machine: &str,
        scope: &MachineScope,
    ) -> Result<MachineAccess, String> {
        let Ok(rt) = self.rt(session_id) else {
            return Err("this chat is gone".into());
        };
        let bot = {
            let st = rt.st.lock();
            if st.info.private || st.info.internal {
                return Err("private chats and helper sessions don't use machines".into());
            }
            let id = self.speaker_id(&st);
            let bot = id.as_deref().and_then(|id| self.find_bot(id));
            // A removed bot's chats don't fall back to a plain chat's reach.
            if id.is_some() && bot.is_none() {
                return Err("this chat's bot was removed, so it can't use machines".into());
            }
            bot
        };
        crate::node::may_use(
            scope,
            bot.as_ref().map(|b| (b.id.as_str(), b.handle.as_str())),
        )
        .map_err(|why| format!("{why}. Who may use it is set on that computer"))?;
        match bot {
            None => Ok(MachineAccess::Allowed),
            Some(b) => match b.machine_access(machine) {
                MachineAccess::NoAccess => Err(crate::bots::no_machine_access(&b)),
                level => Ok(level),
            },
        }
    }

    /// A machine `bot` may do more on than this session, by name: what
    /// each may really do there, the machine's own "who can use it" first
    /// (a plain chat too: a machine only for certain bots is closed to it),
    /// then the bot's own setting for it.
    pub(super) fn bot_more_machines(&self, session_id: &str, bot: &str) -> Option<String> {
        let to = self.find_bot(bot)?;
        let from = {
            let rt = self.rt(session_id).ok()?;
            let st = rt.st.lock();
            (!st.info.private && !st.info.internal).then(|| self.speaker_bot(&st))
        };
        let rank = |a: MachineAccess| match a {
            MachineAccess::NoAccess => 0,
            MachineAccess::ReadOnly => 1,
            MachineAccess::Allowed => 2,
        };
        // `None`: a session that uses no machines (private, helper).
        let reach = |b: Option<Option<&Bot>>, r: &Record| match b {
            None => 0,
            Some(b) => {
                if crate::node::may_use(&r.scope, b.map(|b| (b.id.as_str(), b.handle.as_str())))
                    .is_err()
                {
                    0
                } else {
                    b.map_or(2, |b| rank(b.machine_access(&r.id)))
                }
            }
        };
        let from = from.as_ref().map(|b| b.as_ref());
        self.machines
            .records
            .lock()
            .iter()
            .find(|r| reach(Some(Some(&to)), r) > reach(from, r))
            .map(|r| r.name.clone())
    }

    /// Refuse what a read-only bot may not do there (anything but reading
    /// and listing).
    pub(super) fn machine_action_ok(
        &self,
        session_id: &str,
        level: MachineAccess,
        action: &NodeAction,
    ) -> Result<(), String> {
        if level != MachineAccess::ReadOnly || action.only_reads_files() {
            return Ok(());
        }
        let bot = self
            .rt(session_id)
            .ok()
            .and_then(|rt| self.speaker_bot(&rt.st.lock()));
        Err(match bot {
            Some(b) => crate::bots::read_only_machine(&b, "its screen, commands and changes"),
            None => "only reading is allowed there".into(),
        })
    }

    /// Remember an action for the machine's recent activity.
    fn note_activity(&self, machine: &str, session_id: &str, action: &NodeAction, outcome: &str) {
        let (chat, _, bot) = self.chat_of(session_id);
        let a = MachineActivity {
            ts: now_ms(),
            action: if matches!(action, NodeAction::Run { admin: true, .. }) {
                "run as administrator".into()
            } else {
                action.name().into()
            },
            target: crate::node::log::target_of(action)
                .chars()
                .take(160)
                .collect(),
            chat: crate::util::one_line(&chat, 80),
            bot: bot.map(|b| crate::util::one_line(&b.name, 60)),
            outcome: outcome.into(),
        };
        {
            let mut records = self.machines.records.lock();
            let Some(r) = records.iter_mut().find(|r| r.id == machine) else {
                return;
            };
            r.recent.insert(0, a);
            r.recent.truncate(RECENT);
        }
        self.save_machines();
    }

    /// An action refused here (or declined by the user) never reaches the
    /// machine; it is still told, for its own log, when it is connected.
    pub(super) fn machine_refused(
        &self,
        session_id: &str,
        machine: &str,
        action: NodeAction,
        reason: &str,
    ) {
        let outcome = if reason == "declined in the chat" {
            "declined"
        } else {
            "refused"
        };
        self.note_activity(machine, session_id, &action, outcome);
        self.emit_machines();
        let Some(tx) = self.machines.live.lock().get(machine).map(|l| l.tx.clone()) else {
            return;
        };
        let (chat, mode, bot) = self.chat_of(session_id);
        let _ = tx.try_send(Event::NodeRefused {
            call: NodeCall {
                call_id: new_id(),
                action,
                session_id: session_id.to_string(),
                chat,
                mode,
                approved: false,
                bot,
                env: Default::default(),
                stops: None,
                files_only: false,
            },
            reason: crate::util::truncate(reason, 500),
        });
    }

    /// Run one action on a machine for the `machines` tool and wait for its
    /// answer (live output goes to `call.progress`).
    pub(super) async fn machine_call(
        self: Arc<Self>,
        call: MachineCall,
    ) -> Result<NodeResult, String> {
        // Tessera Guard there: its signed word that the computer isn't
        // stopped, fresh for this action, first; everything below (access,
        // the turn's Stops, the connection) is then looked at as it is after
        // that wait.
        let guarded = {
            let records = self.machines.records.lock();
            records
                .iter()
                .find(|r| r.id == call.machine)
                .filter(|r| r.guard_key.is_some() || r.guard_required || r.guard_changed)
                .map(|r| r.name.clone())
        };
        let mut attested = None;
        if let Some(name) = guarded {
            let live = self
                .machines
                .live
                .lock()
                .get(&call.machine)
                .map(|l| ((l.tx.clone(), l.kill.clone()), l.conn));
            let Some((ltx, lconn)) = live else {
                return Err(format!("{name} is offline right now"));
            };
            match self.guard_says_go(&call.machine, &name, lconn, &ltx).await {
                Ok(word) => attested = word,
                Err(why) => {
                    let why = format!("{name}: {why}");
                    self.machine_refused(
                        &call.session_id,
                        &call.machine,
                        call.action.clone(),
                        &why,
                    );
                    return Err(why);
                }
            }
        }
        let (tx, conn, name, limit, scope, as_checked) = {
            let records = self.machines.records.lock();
            let r = records
                .iter()
                .find(|r| r.id == call.machine)
                .ok_or("there is no such machine (any more)")?;
            if r.paused {
                return Err(format!(
                    "{} is paused; the user can resume it in Settings → Devices",
                    r.name
                ));
            }
            let live = self.machines.live.lock();
            let l = live
                .get(&r.id)
                .ok_or_else(|| format!("{} is offline right now", r.name))?;
            (
                (l.tx.clone(), l.kill.clone()),
                l.conn,
                r.name.clone(),
                r.scope.timeout_secs,
                r.scope.clone(),
                // Guarded as it is now (it may have said hello again, with
                // a guard, since the first look): only with its guard's word
                // asked on this very connection, as the record still says.
                r.still_as_checked(attested.as_ref(), l.conn),
            )
        };
        let changed = format!(
            "{name}: it reconnected, or its Tessera Guard changed or said something newer, while this was asked, so nothing was done; ask again (if the user stopped it, it will say so)"
        );
        if !as_checked {
            self.machine_refused(&call.session_id, &call.machine, call.action, &changed);
            return Err(changed);
        }
        // Its count of the user's Stops when this turn was accepted (0 for a
        // machine the turn didn't know): after a Stop there, nothing from
        // that turn (its queued calls, its helpers) acts there again. The
        // machine checks it too, against its own count.
        let turn = self.rt(&call.session_id).ok().map(|rt| {
            rt.st
                .lock()
                .turn_stops
                .machines
                .get(&call.machine)
                .copied()
                .unwrap_or(0)
        });
        if turn.is_some_and(|t| t < scope.stops) {
            let why = format!(
                "{name}: Tessera was stopped on that computer after this turn began, so this was not done. Don't retry; tell the user (a new message after they resume it there works as usual)."
            );
            self.machine_refused(&call.session_id, &call.machine, call.action, &why);
            return Err(why);
        }
        // Who may use it and how far (a bot's own access), checked here
        // whatever the caller checked.
        let level = match self
            .machine_access(&call.session_id, &call.machine, &scope)
            .and_then(|level| {
                self.machine_action_ok(&call.session_id, level, &call.action)
                    .map(|()| level)
            }) {
            Ok(level) => level,
            Err(why) => {
                let why = format!("{name}: {why}.");
                self.machine_refused(&call.session_id, &call.machine, call.action, &why);
                return Err(why);
            }
        };
        let (chat, mut mode, bot) = self.chat_of(&call.session_id);
        // The machine checks a read-only bot's calls too, as a read-only chat's.
        if level == MachineAccess::ReadOnly {
            mode = PermissionMode::ReadOnly;
        }
        let action = call.action.clone();
        let call_id = new_id();
        let (rtx, mut rrx) = oneshot::channel();
        let secrets = self.redaction_secrets();
        let protect = !secrets.is_empty();
        let redactor = Arc::new(Mutex::new(std::collections::BTreeMap::from([
            (
                "stdout",
                (
                    crate::tools::redaction::Stream::new(secrets.clone()),
                    String::new(),
                    false,
                ),
            ),
            (
                "stderr",
                (
                    crate::tools::redaction::Stream::new(secrets.clone()),
                    String::new(),
                    false,
                ),
            ),
            (
                "legacy",
                (
                    crate::tools::redaction::Stream::new(secrets.clone()),
                    String::new(),
                    false,
                ),
            ),
        ])));
        let progress = {
            let redactor = redactor.clone();
            let publish = call.progress.clone();
            Arc::new(move |chunk: &str, stream: &str| {
                let mut redactors = redactor.lock();
                let Some((redactor, output, seen)) = redactors.get_mut(stream) else {
                    return;
                };
                *seen = true;
                // Old nodes merge pipes before sending. No suffix redactor can
                // safely recover two interleaved streams, so withhold their
                // command output until that node is updated.
                if protect && stream == "legacy" {
                    return;
                }
                let clean = redactor.push(chunk.as_bytes());
                if !clean.is_empty() {
                    output.push_str(&clean);
                    if output.len() > 120_000 {
                        let cut = crate::util::ceil_char(output, output.len() - 60_000);
                        *output = format!("[… earlier output dropped …]\n{}", &output[cut..]);
                    }
                    publish(&clean);
                }
            })
        };
        self.machines.calls.lock().insert(
            call_id.clone(),
            Waiting {
                device: call.machine.clone(),
                conn,
                tx: rtx,
                progress,
            },
        );
        let event = Event::NodeCall {
            call: NodeCall {
                call_id: call_id.clone(),
                action: call.action,
                session_id: call.session_id.clone(),
                chat,
                mode,
                approved: call.approved,
                bot,
                env: call.env,
                stops: turn.or(call.stops),
                files_only: level == MachineAccess::ReadOnly,
            },
        };
        // Sent while the record still says what the guard's word was checked
        // against, under its lock: no newer word (a Stop seen for another
        // call), changed key or new connection comes in between.
        let sent = {
            let records = self.machines.records.lock();
            let live = self.machines.live.lock();
            let holds = records
                .iter()
                .find(|r| r.id == call.machine)
                .is_some_and(|r| r.still_as_checked(attested.as_ref(), conn))
                && live.get(&call.machine).is_some_and(|l| l.conn == conn);
            holds.then(|| tx.0.try_send(event).is_ok())
        };
        let Some(sent) = sent else {
            self.machines.calls.lock().remove(&call_id);
            self.machine_refused(&call.session_id, &call.machine, action, &changed);
            return Err(changed);
        };
        if !sent {
            self.machines.calls.lock().remove(&call_id);
            return Err(format!(
                "{name} isn't keeping up right now; try again shortly"
            ));
        }
        self.emit_machines();
        let deadline = std::time::Duration::from_secs(limit) + GRACE;
        let cancel = |(tx, kill): &(mpsc::Sender<Event>, CancellationToken)| {
            must_reach(
                tx,
                kill,
                Event::NodeCancel {
                    call_id: call_id.clone(),
                },
            );
        };
        // A desktop action answers a stop only once it let go of the mouse
        // and keys (the machine waits up to 10 s for that); without that
        // answer nobody knows it stopped.
        let desktop = matches!(action, NodeAction::Desktop { .. });
        let unsure = |what: &str| {
            format!(
                "{what} {name} didn't confirm the desktop action stopped; it may still be using the screen, so take a screenshot before going on."
            )
        };
        let result = tokio::select! {
            r = &mut rrx => r.map_err(|_| format!("Lost the connection to {name} while the action ran; it may or may not have finished, so check before repeating it.")),
            _ = tokio::time::sleep(deadline) => {
                cancel(&tx);
                let late = format!("{name} didn't answer within {};", crate::node::human_secs(deadline.as_secs()));
                Err(if desktop { unsure(&late) } else { format!("{late} the action was stopped") })
            }
            _ = call.cancel.cancelled() => {
                cancel(&tx);
                // What it printed before stopping.
                let wait = std::time::Duration::from_secs(if desktop { 15 } else { 5 });
                match tokio::time::timeout(wait, &mut rrx).await {
                    Ok(Ok(r)) => Ok(r),
                    _ if desktop => Err(unsure("Interrupted by the user.")),
                    _ => Err("Interrupted by the user.".into()),
                }
            }
        };
        self.machines.calls.lock().remove(&call_id);
        let mut streams = redactor.lock();
        for (redactor, output, _) in streams.values_mut() {
            let tail = redactor.finish();
            if !tail.is_empty() {
                output.push_str(&tail);
                (call.progress)(&tail);
            }
        }
        let result = result.map(|mut result| {
            if protect && matches!(action, NodeAction::Run { .. }) && !result.refused {
                if streams["legacy"].2 {
                    result.output = "Command output withheld. Update Tessera on that computer to support independently redacted stdout and stderr.".into();
                } else if streams["stdout"].2 || streams["stderr"].2 {
                    // The node's final buffer may have truncated an unknown
                    // stored value. Rebuild from server-redacted progress,
                    // which was sanitized before its bounded buffer advanced.
                    let body = format!("{}{}", streams["stdout"].1, streams["stderr"].1);
                    let status = result.exit_code.map_or_else(|| "Command ended.".into(), |c| format!("[exit {c}]"));
                    result.output = format!("{}\n{status}", body.trim_end());
                }
            }
            result.output = crate::tools::redact_known(&result.output, &self.redaction_secrets());
            result
        });
        drop(streams);
        let outcome = match &result {
            Ok(r) if r.refused => "refused",
            Ok(r) if r.ok => "done",
            Ok(_) => "failed",
            Err(e) if e.starts_with("Interrupted") => "stopped",
            Err(_) => "failed",
        };
        self.note_activity(&call.machine, &call.session_id, &action, outcome);
        self.emit_machines();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control that can't be queued (the machine's queue is full) cuts
    /// the connection instead of being lost.
    #[test]
    fn a_control_that_cant_be_queued_cuts_the_connection() {
        let (tx, _rx) = mpsc::channel(1);
        let kill = CancellationToken::new();
        must_reach(&tx, &kill, Event::Resync);
        assert!(!kill.is_cancelled(), "there was room");
        must_reach(
            &tx,
            &kill,
            Event::NodeControl {
                paused: true,
                revoked: false,
            },
        );
        assert!(kill.is_cancelled(), "full: cut");
    }

    fn guarded_record() -> Record {
        Record {
            id: "m".into(),
            name: "M".into(),
            os: "windows x86_64".into(),
            version: "0.1.0".into(),
            scope: MachineScope::default(),
            paused: false,
            last_seen: 0,
            generation: 1,
            sandbox: String::new(),
            sandbox_ok: false,
            home: None,
            recent: Vec::new(),
            features: None,
            computer: None,
            guard_key: Some("k".into()),
            guard_changed: false,
            guard_generation: 5,
            guard_required: false,
        }
    }

    /// An action is sent only while the record says what its guard's word
    /// was checked against: a newer word seen meanwhile (a Stop signed for
    /// another call), another key, a changed guard or another connection
    /// keep it from going.
    #[test]
    fn a_guards_word_counts_only_as_it_was_checked() {
        let r = guarded_record();
        let word = Word {
            conn: 7,
            key: "k".into(),
            generation: 5,
        };
        assert!(r.still_as_checked(Some(&word), 7));
        assert!(!r.still_as_checked(Some(&word), 8), "another connection");
        let mut newer = r.clone();
        newer.guard_generation = 6;
        assert!(
            !newer.still_as_checked(Some(&word), 7),
            "a newer word was seen"
        );
        let mut other = r.clone();
        other.guard_key = Some("other".into());
        assert!(!other.still_as_checked(Some(&word), 7), "another key");
        let mut changed = r.clone();
        changed.guard_changed = true;
        assert!(!changed.still_as_checked(Some(&word), 7), "a changed guard");
        // Without a word: only a machine that needs none.
        assert!(!r.still_as_checked(None, 7), "guarded since the first look");
        let mut plain = r.clone();
        plain.guard_key = None;
        assert!(plain.still_as_checked(None, 7));
        plain.guard_required = true;
        assert!(!plain.still_as_checked(None, 7), "a guard that can't sign");
    }
}
