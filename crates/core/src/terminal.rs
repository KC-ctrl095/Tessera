//! The user's own terminals: a login shell on a pseudo-terminal, shown on
//! the one device that opened it. Used by the server for its own terminals
//! (`engine/terminals.rs`) and by a machine for its (`node/terminal.rs`);
//! who may open one, and how it is confined, is decided there.
//!
//! Here: the open terminals and their limits. At most `max_open` at once
//! for each device, `max_total` in all; only their owner (a device id) can
//! type in, resize, show or close one. What is typed waits for the program
//! in its own queue (a program that doesn't read holds up only its own
//! terminal), up to [`MAX_PENDING_INPUT`]; past that more is refused, and
//! the terminal says so.
//! Output goes to the one attached sink, in order, under a flow-control
//! window: past `window` bytes the device hasn't acknowledged, the terminal
//! isn't read, so a noisy program waits instead of filling memory. The last
//! `keep` bytes are kept to redraw it when the device attaches again. While
//! no device shows it, nothing is held beyond those. A terminal closes when
//! its program ends, when it is closed, when no device showed it for
//! `grace`, after `idle` without typing while the shell waits at its prompt
//! (`idle_running` while a program runs in it), and when its device is
//! signed out. What was typed or shown is never logged.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::sync::Weak;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// What a terminal sends its device.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// Bytes from the program; `seq` is the offset of the first one.
    Output { id: String, seq: u64, data: Vec<u8> },
    /// It ended, and how.
    Exit {
        id: String,
        code: Option<i32>,
        reason: String,
    },
}

impl Out {
    pub fn id(&self) -> &str {
        match self {
            Out::Output { id, .. } | Out::Exit { id, .. } => id,
        }
    }

    /// As the device gets it.
    pub fn event(self) -> tessera_proto::Event {
        use base64::Engine;
        match self {
            Out::Output { id, seq, data } => tessera_proto::Event::TerminalOutput {
                id,
                seq,
                data: base64::engine::general_purpose::STANDARD.encode(data),
            },
            Out::Exit { id, code, reason } => {
                tessera_proto::Event::TerminalExit { id, code, reason }
            }
        }
    }
}

/// Where a terminal's output goes: a connection (`conn` tells them apart)
/// and its queue.
#[derive(Clone)]
pub struct Sink {
    pub conn: u64,
    pub tx: mpsc::Sender<Out>,
}

#[derive(Debug, Clone)]
pub struct Limits {
    /// For each device.
    pub max_open: usize,
    /// For all devices together.
    pub max_total: usize,
    pub idle: Duration,
    pub idle_running: Duration,
    pub grace: Duration,
    /// Bytes sent but not yet acknowledged before reading pauses.
    pub window: u64,
    /// Bytes kept to redraw a terminal shown again.
    pub keep: usize,
    /// How often the time limits are looked at.
    pub tick: Duration,
}

/// Half an hour without typing while nothing runs.
pub const IDLE: Duration = Duration::from_secs(30 * 60);
/// Eight hours without typing while a program runs.
pub const IDLE_RUNNING: Duration = Duration::from_secs(8 * 3600);
/// Five minutes for a device to come back.
pub const GRACE: Duration = Duration::from_secs(5 * 60);
pub const MAX_OPEN: usize = 4;
pub const MAX_TOTAL: usize = 16;
pub const WINDOW: u64 = 256 << 10;
pub const KEEP: usize = 256 << 10;
/// Largest piece of input taken at once (a long paste).
pub const MAX_INPUT: usize = 64 << 10;
/// Typed but not yet read by the program, at most, for each terminal.
pub const MAX_PENDING_INPUT: usize = 1 << 20;
/// Why more typing was refused (and, in the terminal, what it shows).
pub const HELD_UP: &str = "the program in this terminal isn't reading what you type, and over 1 MB is waiting for it; the rest wasn't typed";

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_open: MAX_OPEN,
            max_total: MAX_TOTAL,
            idle: IDLE,
            idle_running: IDLE_RUNNING,
            grace: GRACE,
            window: WINDOW,
            keep: KEEP,
            tick: Duration::from_secs(5),
        }
    }
}

/// A terminal to open: whose, what it shows as, and what to start.
pub struct Opening {
    pub id: String,
    pub owner: String,
    pub owner_name: String,
    pub place: String,
    pub sandboxed: bool,
    #[cfg(any(unix, windows))]
    pub start: crate::pty::Start,
}

/// A terminal as listed.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub id: String,
    pub owner: String,
    pub place: String,
    pub sandboxed: bool,
    pub shell: String,
    pub started_at: u64,
}

/// How a terminal ended, for the record (never its contents).
#[derive(Debug, Clone)]
pub struct Closed {
    pub id: String,
    pub owner: String,
    pub owner_name: String,
    pub place: String,
    pub sandboxed: bool,
    pub started_at: u64,
    pub code: Option<i32>,
    pub reason: String,
}

type OnClose = Arc<dyn Fn(&Closed) + Send + Sync>;

struct State {
    sink: Option<Sink>,
    /// The last place it was shown: told when it ends even while nobody
    /// shows it (a machine's link to the server must always hear of it).
    last_sink: Option<Sink>,
    /// Counts attachments: a send to an older one is given up.
    generation: u64,
    /// The kept output must go out (again) before anything new.
    replay: bool,
    produced: u64,
    acked: u64,
    kept: VecDeque<u8>,
    last_input: Instant,
    detached_at: Option<Instant>,
    /// Tessera's own words for the device, shown between the program's
    /// output (typing was refused).
    notes: Vec<u8>,
}

struct Term {
    info: Info,
    owner_name: String,
    #[cfg(unix)]
    pid: u32,
    #[cfg(any(unix, windows))]
    pty: crate::pty::Pty,
    st: Mutex<State>,
    /// What was typed, waiting for the program in order, and how much.
    typing: mpsc::UnboundedSender<Vec<u8>>,
    pending: std::sync::atomic::AtomicUsize,
    /// The device was told that typing is held up (until it catches up).
    told: std::sync::atomic::AtomicBool,
    /// The reader looks again: an acknowledgement, or it was shown again.
    wake: Notify,
    /// It was shown elsewhere or nowhere: a send waiting for room gives up.
    moved: Notify,
    /// Its program was reaped (its process id may be reused).
    reaped: std::sync::atomic::AtomicBool,
    closed: CancellationToken,
    /// The reader got to the end of the output.
    #[cfg(any(unix, windows))]
    drained: CancellationToken,
}

struct Ended {
    owner: String,
    code: Option<i32>,
    reason: String,
    at: Instant,
}

struct Shared {
    limits: Mutex<Limits>,
    terms: Mutex<HashMap<String, Arc<Term>>>,
    /// Recently ended terminals, so a device coming back learns how.
    ended: Mutex<HashMap<String, Ended>>,
    on_close: OnClose,
    #[cfg(any(unix, windows))]
    sweeping: Mutex<bool>,
}

/// The open terminals of this server, or of this machine.
#[derive(Clone)]
pub struct Host(Arc<Shared>);

/// Why a request about a terminal didn't happen, in plain words.
pub type Result<T> = std::result::Result<T, String>;

const GONE: &str = "that terminal isn't open any more";

impl Host {
    pub fn new(limits: Limits, on_close: OnClose) -> Self {
        Self(Arc::new(Shared {
            limits: Mutex::new(limits),
            terms: Mutex::new(HashMap::new()),
            ended: Mutex::new(HashMap::new()),
            on_close,
            #[cfg(any(unix, windows))]
            sweeping: Mutex::new(false),
        }))
    }

    pub fn set_limits(&self, limits: Limits) {
        *self.0.limits.lock() = limits;
    }

    pub fn limits(&self) -> Limits {
        self.0.limits.lock().clone()
    }

    pub fn count(&self) -> usize {
        self.0.terms.lock().len()
    }

    /// The terminals of `owner` (every one with `None`).
    pub fn list(&self, owner: Option<&str>) -> Vec<Info> {
        let mut v: Vec<Info> = self
            .0
            .terms
            .lock()
            .values()
            .filter(|t| owner.is_none_or(|o| t.info.owner == o))
            .map(|t| t.info.clone())
            .collect();
        v.sort_by_key(|i| i.started_at);
        v
    }

    fn get(&self, id: &str, owner: &str) -> Result<Arc<Term>> {
        match self.0.terms.lock().get(id) {
            Some(t) if t.info.owner == owner => Ok(t.clone()),
            // Another device's terminal looks the same as none at all.
            _ => Err(self.why_gone(id, owner)),
        }
    }

    fn why_gone(&self, id: &str, owner: &str) -> String {
        match self.0.ended.lock().get(id) {
            Some(e) if e.owner == owner => format!("that terminal ended: {}", e.reason),
            _ => GONE.into(),
        }
    }

    /// Start a terminal and show it on `sink`.
    #[cfg(any(unix, windows))]
    pub fn open(&self, o: Opening, sink: Sink) -> Result<Info> {
        let limits = self.limits();
        let shell = std::path::Path::new(&o.start.program)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "shell".into());
        let shell = if o.start.sandbox.is_some() {
            // Behind `nsenter` the program is its last word before the shell's own.
            o.start
                .args
                .iter()
                .rev()
                .find(|a| a.to_string_lossy().starts_with('/'))
                .map(|a| {
                    a.to_string_lossy()
                        .rsplit('/')
                        .next()
                        .unwrap_or("shell")
                        .to_string()
                })
                .unwrap_or(shell)
        } else {
            shell
        };
        // Counted and claimed in one step, so two at once can't both pass.
        let mut terms = self.0.terms.lock();
        if terms.contains_key(&o.id) {
            return Err("that terminal is already open".into());
        }
        let mine = terms.values().filter(|t| t.info.owner == o.owner).count();
        if mine >= limits.max_open {
            return Err(format!(
                "{} terminals of this device are open there already, the most allowed at once; close one first",
                limits.max_open
            ));
        }
        if terms.len() >= limits.max_total.max(limits.max_open) {
            return Err(format!(
                "{} terminals are open there already (from all your devices), the most allowed at once; close one first",
                terms.len()
            ));
        }
        let spawned =
            crate::pty::spawn(o.start).map_err(|e| format!("the terminal couldn't start: {e}"))?;
        let now = Instant::now();
        let (typing, typed) = mpsc::unbounded_channel();
        let term = Arc::new(Term {
            info: Info {
                id: o.id.clone(),
                owner: o.owner,
                place: o.place,
                sandboxed: o.sandboxed,
                shell,
                started_at: crate::util::now_ms(),
            },
            owner_name: o.owner_name,
            #[cfg(unix)]
            pid: spawned.pid,
            pty: spawned.pty,
            st: Mutex::new(State {
                last_sink: Some(sink.clone()),
                sink: Some(sink),
                generation: 1,
                replay: false,
                produced: 0,
                acked: 0,
                kept: VecDeque::new(),
                last_input: now,
                detached_at: None,
                notes: Vec::new(),
            }),
            typing,
            pending: Default::default(),
            told: Default::default(),
            wake: Notify::new(),
            moved: Notify::new(),
            reaped: Default::default(),
            closed: CancellationToken::new(),
            drained: CancellationToken::new(),
        });
        terms.insert(o.id.clone(), term.clone());
        drop(terms);
        tracing::info!(
            "terminal {} opened in {} for device {}",
            term.info.id,
            term.info.place,
            term.info.owner
        );
        tokio::spawn(read_loop(self.clone(), term.clone()));
        tokio::spawn(type_loop(term.clone(), typed));
        tokio::spawn(wait_loop(self.clone(), term.clone(), spawned.child));
        self.sweep_later();
        Ok(term.info.clone())
    }

    /// Show one of `owner`'s terminals on `sink` (again), at `size`
    /// (columns, rows): what was kept comes first, then the program is
    /// asked to draw itself again.
    pub fn attach(
        &self,
        id: &str,
        owner: &str,
        sink: Sink,
        size: Option<(u16, u16)>,
    ) -> Result<Info> {
        let t = self.get(id, owner)?;
        {
            let mut st = t.st.lock();
            st.last_sink = Some(sink.clone());
            st.sink = Some(sink);
            st.generation += 1;
            st.detached_at = None;
            st.replay = true;
            // What is kept goes out again, and counts as unacknowledged.
            st.acked = st.produced - st.kept.len() as u64;
        }
        #[cfg(any(unix, windows))]
        {
            if let Some((cols, rows)) = size {
                let _ = t.pty.resize(cols, rows);
            }
            t.pty.redraw();
        }
        #[cfg(not(any(unix, windows)))]
        let _ = size;
        t.moved.notify_one();
        t.wake.notify_one();
        Ok(t.info.clone())
    }

    /// Nobody shows it now: it waits for its device for `grace`.
    pub fn detach(&self, id: &str, owner: &str) -> Result<()> {
        let t = self.get(id, owner)?;
        detach(&t);
        Ok(())
    }

    /// A connection went away: whatever it showed waits for its device.
    pub fn detach_conn(&self, conn: u64) {
        let terms: Vec<Arc<Term>> = self.0.terms.lock().values().cloned().collect();
        for t in terms {
            if t.st.lock().sink.as_ref().is_some_and(|s| s.conn == conn) {
                detach(&t);
            }
        }
    }

    /// Type `data` into one of `owner`'s terminals, after what was typed
    /// before ([`Host::type_in`]).
    pub async fn input(&self, id: &str, owner: &str, data: &[u8]) -> Result<()> {
        self.type_in(id, owner, data)
    }

    /// Type `data` into one of `owner`'s terminals, after what was typed
    /// before. It doesn't wait for the program to read it: that happens in
    /// the terminal's own queue. Past [`MAX_PENDING_INPUT`] waiting, more is
    /// refused, and the terminal itself says so (once, until the program
    /// reads again).
    pub fn type_in(&self, id: &str, owner: &str, data: &[u8]) -> Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        if data.len() > MAX_INPUT {
            return Err(format!(
                "that is more than {} KB at once; paste it in parts",
                MAX_INPUT >> 10
            ));
        }
        let t = self.get(id, owner)?;
        t.st.lock().last_input = Instant::now();
        if t.pending.load(SeqCst) + data.len() > MAX_PENDING_INPUT {
            if !t.told.swap(true, SeqCst) {
                t.st.lock()
                    .notes
                    .extend_from_slice(format!("\r\n[Tessera: {HELD_UP}.]\r\n").as_bytes());
                t.wake.notify_one();
            }
            return Err(HELD_UP.into());
        }
        t.pending.fetch_add(data.len(), SeqCst);
        t.typing
            .send(data.to_vec())
            .map_err(|_| self.why_gone(id, owner))
    }

    pub fn resize(&self, id: &str, owner: &str, cols: u16, rows: u16) -> Result<()> {
        let t = self.get(id, owner)?;
        #[cfg(any(unix, windows))]
        t.pty
            .resize(cols, rows)
            .map_err(|e| format!("couldn't resize the terminal: {e}"))?;
        #[cfg(not(any(unix, windows)))]
        let _ = (t, cols, rows);
        Ok(())
    }

    /// The device drew everything before byte `upto`.
    pub fn ack(&self, id: &str, owner: &str, upto: u64) -> Result<()> {
        let t = self.get(id, owner)?;
        {
            let mut st = t.st.lock();
            let upto = upto.min(st.produced);
            if upto > st.acked {
                st.acked = upto;
            }
        }
        t.wake.notify_one();
        Ok(())
    }

    pub fn close(&self, id: &str, owner: &str, reason: &str) -> Result<()> {
        let t = self.get(id, owner)?;
        self.finish(&t, None, reason);
        Ok(())
    }

    /// Close every terminal of `owner` (signed out).
    pub fn close_owner(&self, owner: &str, reason: &str) {
        let terms: Vec<Arc<Term>> = self
            .0
            .terms
            .lock()
            .values()
            .filter(|t| t.info.owner == owner)
            .cloned()
            .collect();
        for t in terms {
            self.finish(&t, None, reason);
        }
    }

    pub fn close_all(&self, reason: &str) {
        let terms: Vec<Arc<Term>> = self.0.terms.lock().values().cloned().collect();
        for t in terms {
            self.finish(&t, None, reason);
        }
    }

    /// End `t` once: stop its programs, tell its device, keep the record.
    fn finish(&self, t: &Arc<Term>, code: Option<i32>, reason: &str) {
        if self.0.terms.lock().remove(&t.info.id).is_none() {
            return;
        }
        t.closed.cancel();
        // Still running: the whole session (the shell leads it) and all it
        // started. Once reaped, its process id may belong to another
        // program; the terminal closing hangs up what is left.
        if !t.reaped.load(std::sync::atomic::Ordering::SeqCst) {
            #[cfg(unix)]
            // SAFETY: a signal to the terminal's own process group.
            unsafe {
                libc::killpg(t.pid as i32, libc::SIGHUP);
            }
            #[cfg(unix)]
            crate::node::exec::stop_tree(t.pid);
        }
        #[cfg(windows)]
        t.pty.close();
        t.moved.notify_one();
        let sink = {
            let mut st = t.st.lock();
            st.generation += 1;
            let last = st.last_sink.take();
            st.sink.take().or(last)
        };
        if let Some(s) = sink {
            let out = Out::Exit {
                id: t.info.id.clone(),
                code,
                reason: reason.to_string(),
            };
            if let Err(mpsc::error::TrySendError::Full(out)) = s.tx.try_send(out) {
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(10), s.tx.send(out)).await;
                });
            }
        }
        let now = Instant::now();
        {
            let grace = self.limits().grace;
            let mut ended = self.0.ended.lock();
            ended.retain(|_, e| now.duration_since(e.at) < grace);
            ended.insert(
                t.info.id.clone(),
                Ended {
                    owner: t.info.owner.clone(),
                    code,
                    reason: reason.to_string(),
                    at: now,
                },
            );
        }
        tracing::info!("terminal {} closed ({reason})", t.info.id);
        (self.0.on_close)(&Closed {
            id: t.info.id.clone(),
            owner: t.info.owner.clone(),
            owner_name: t.owner_name.clone(),
            place: t.info.place.clone(),
            sandboxed: t.info.sandboxed,
            started_at: t.info.started_at,
            code,
            reason: reason.to_string(),
        });
    }

    /// How a terminal of `owner` ended, if it did lately.
    pub fn ended(&self, id: &str, owner: &str) -> Option<(Option<i32>, String)> {
        self.0
            .ended
            .lock()
            .get(id)
            .filter(|e| e.owner == owner)
            .map(|e| (e.code, e.reason.clone()))
    }

    /// Look at the time limits every `tick` while terminals are open.
    #[cfg(any(unix, windows))]
    fn sweep_later(&self) {
        {
            let mut s = self.0.sweeping.lock();
            if *s {
                return;
            }
            *s = true;
        }
        let weak: Weak<Shared> = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            loop {
                let Some(shared) = weak.upgrade() else { return };
                let host = Host(shared);
                let tick = host.limits().tick;
                {
                    // Looked at under the flag's lock: a terminal opened
                    // meanwhile starts a new sweep after this one stops.
                    let mut s = host.0.sweeping.lock();
                    if host.count() == 0 {
                        *s = false;
                        return;
                    }
                }
                drop(host);
                tokio::time::sleep(tick).await;
                let Some(shared) = weak.upgrade() else { return };
                Host(shared).sweep();
            }
        });
    }

    /// Close what ran out of time.
    pub fn sweep(&self) {
        let limits = self.limits();
        let now = Instant::now();
        let terms: Vec<Arc<Term>> = self.0.terms.lock().values().cloned().collect();
        for t in terms {
            let (detached, last_input) = {
                let st = t.st.lock();
                (st.detached_at, st.last_input)
            };
            if detached.is_some_and(|d| now.duration_since(d) >= limits.grace) {
                self.finish(
                    &t,
                    None,
                    &format!(
                        "closed because no device showed it for {}",
                        crate::node::human_secs(limits.grace.as_secs())
                    ),
                );
                continue;
            }
            let quiet = now.duration_since(last_input);
            let limit = idle_limit(&limits, at_prompt(&t));
            if quiet >= limit {
                self.finish(
                    &t,
                    None,
                    &format!(
                        "closed after {} without typing",
                        crate::node::human_secs(limit.as_secs())
                    ),
                );
            }
        }
    }
}

/// Only a known prompt can use the shorter idle bound. ConPTY cannot tell
/// an in-process cmdlet from the shell prompt, so unknown uses the running bound.
fn idle_limit(limits: &Limits, prompt: Option<bool>) -> Duration {
    if prompt == Some(true) {
        limits.idle.min(limits.idle_running)
    } else {
        limits.idle_running
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    #[test]
    fn unknown_prompt_uses_the_running_limit() {
        let limits = Limits {
            idle: Duration::from_secs(30 * 60),
            idle_running: Duration::from_secs(8 * 60 * 60),
            ..Limits::default()
        };
        assert_eq!(idle_limit(&limits, None), limits.idle_running);
        assert_eq!(idle_limit(&limits, Some(false)), limits.idle_running);
        assert_eq!(idle_limit(&limits, Some(true)), limits.idle);
        // A one-hour in-process PowerShell cmdlet is not expired at 30 minutes.
        assert!(Duration::from_secs(3600) < idle_limit(&limits, None));
    }
}

fn at_prompt(t: &Term) -> Option<bool> {
    #[cfg(unix)]
    {
        let shell = crate::sandbox::program_pid(t.pid) as i32;
        t.pty.foreground().map(|foreground| foreground == shell)
    }
    #[cfg(windows)]
    {
        t.pty.at_prompt()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = t;
        None
    }
}

fn detach(t: &Term) {
    {
        let mut st = t.st.lock();
        if st.sink.is_none() {
            return;
        }
        st.sink = None;
        st.generation += 1;
        st.replay = false;
        st.detached_at = Some(Instant::now());
        // Nobody acknowledges now: only what is kept is held.
        st.acked = st.produced;
    }
    t.moved.notify_one();
    t.wake.notify_one();
}

/// Send `out` on `sink` unless the terminal is shown elsewhere (or nowhere)
/// meanwhile. False: given up.
#[cfg(any(unix, windows))]
async fn deliver(t: &Term, sink: &Sink, generation: u64, out: Out) -> bool {
    loop {
        tokio::select! {
            permit = sink.tx.reserve() => match permit {
                Ok(p) => {
                    p.send(out);
                    return true;
                }
                Err(_) => {
                    // Its connection is gone.
                    let same = t.st.lock().generation == generation;
                    if same {
                        detach(t);
                    }
                    return false;
                }
            },
            _ = t.moved.notified() => {
                if t.st.lock().generation != generation {
                    return false;
                }
            }
            _ = t.closed.cancelled() => return false,
        }
    }
}

#[cfg(any(unix, windows))]
async fn read_loop(host: Host, t: Arc<Term>) {
    let mut buf = vec![0u8; 16 << 10];
    loop {
        // Kept output first, to a device that (re)attached.
        let replay = {
            let mut st = t.st.lock();
            if st.replay {
                st.replay = false;
                st.sink.clone().map(|s| {
                    let from = st.produced - st.kept.len() as u64;
                    let data: Vec<u8> = st.kept.iter().copied().collect();
                    (s, st.generation, from, data)
                })
            } else {
                None
            }
        };
        if let Some((sink, generation, from, data)) = replay {
            let mut seq = from;
            for piece in data.chunks(64 << 10) {
                let out = Out::Output {
                    id: t.info.id.clone(),
                    seq,
                    data: piece.to_vec(),
                };
                if !deliver(&t, &sink, generation, out).await {
                    break;
                }
                seq += piece.len() as u64;
            }
            continue;
        }
        let window = host.limits().window;
        let room = {
            let st = t.st.lock();
            st.produced - st.acked < window
        };
        if !room {
            tokio::select! {
                _ = t.wake.notified() => continue,
                _ = t.closed.cancelled() => break,
            }
        }
        // Tessera's own words first, then what the program wrote.
        let noted = std::mem::take(&mut t.st.lock().notes);
        let data = if noted.is_empty() {
            let n = tokio::select! {
                r = t.pty.read(&mut buf) => match r {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::debug!("terminal {}: read failed: {e}", t.info.id);
                        0
                    }
                },
                _ = t.wake.notified() => continue,
                _ = t.closed.cancelled() => break,
            };
            if n == 0 {
                break;
            }
            buf[..n].to_vec()
        } else {
            noted
        };
        let keep = host.limits().keep;
        let (seq, sink, generation) = {
            let mut st = t.st.lock();
            let seq = st.produced;
            st.kept.extend(&data);
            let over = st.kept.len().saturating_sub(keep);
            st.kept.drain(..over);
            st.produced += data.len() as u64;
            if st.sink.is_none() {
                st.acked = st.produced;
            }
            (seq, st.sink.clone(), st.generation)
        };
        if let Some(sink) = sink {
            let out = Out::Output {
                id: t.info.id.clone(),
                seq,
                data,
            };
            deliver(&t, &sink, generation, out).await;
        }
    }
    t.drained.cancel();
}

/// Type what the device sent into the terminal, in order, as the program
/// reads it, until the terminal closes.
#[cfg(any(unix, windows))]
async fn type_loop(t: Arc<Term>, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    use std::sync::atomic::Ordering::SeqCst;
    loop {
        let data = tokio::select! {
            d = rx.recv() => match d {
                Some(d) => d,
                None => return,
            },
            _ = t.closed.cancelled() => return,
        };
        let r = tokio::select! {
            r = t.pty.write_all(&data) => r,
            _ = t.closed.cancelled() => return,
        };
        if let Err(e) = r {
            tracing::debug!("terminal {}: typing failed: {e}", t.info.id);
        }
        // Caught up: should it be held up again, the device hears of it again.
        if t.pending.fetch_sub(data.len(), SeqCst) == data.len() {
            t.told.store(false, SeqCst);
        }
    }
}

#[cfg(any(unix, windows))]
async fn wait_loop(host: Host, t: Arc<Term>, mut child: crate::pty::Child) {
    let status = tokio::select! {
        s = child.wait() => s.ok(),
        _ = t.closed.cancelled() => {
            // Closed from here: reap it once it is gone.
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            return;
        }
    };
    t.reaped.store(true, std::sync::atomic::Ordering::SeqCst);
    // What it printed last still goes out, while the device keeps taking
    // it (unless something it started keeps the terminal open).
    let progress = || {
        let st = t.st.lock();
        (st.produced, st.acked)
    };
    for _ in 0..20 {
        let before = progress();
        if tokio::time::timeout(Duration::from_millis(500), t.drained.cancelled())
            .await
            .is_ok()
            || progress() == before
        {
            break;
        }
    }
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    let (code, reason) = match status {
        Some(s) => {
            #[cfg(unix)]
            let signal = s.signal();
            #[cfg(not(unix))]
            let signal: Option<i32> = None;
            match (s.code(), signal) {
                (Some(c), _) => (Some(c), format!("exited with code {c}")),
                (None, Some(sig)) => (None, format!("ended by signal {sig}")),
                _ => (None, "ended".to_string()),
            }
        }
        None => (None, "ended".to_string()),
    };
    host.finish(&t, code, &reason);
}

/// Tests that start programs on terminals end within `secs` whatever
/// happens: past it the whole test process exits (a deadlock can't be
/// timed out from inside), which hangs up every terminal it opened, so
/// neither the test nor its programs can hold a machine's build lock.
#[cfg(test)]
pub(crate) struct Watchdog(Arc<std::sync::atomic::AtomicBool>);

#[cfg(test)]
pub(crate) fn watchdog(what: &'static str, secs: u64) -> Watchdog {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = done.clone();
    std::thread::spawn(move || {
        let end = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < end {
            if d.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("{what}: still running after {secs} s; stopping the test process");
        std::process::exit(101);
    });
    Watchdog(done)
}

#[cfg(test)]
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Rig {
        host: Host,
        closed: Arc<Mutex<Vec<Closed>>>,
        dir: tempfile::TempDir,
        _dog: Watchdog,
    }

    /// Whatever happened, nothing a test started outlives it.
    impl Drop for Rig {
        fn drop(&mut self) {
            self.host.close_all("the test is over");
        }
    }

    fn rig(limits: Limits) -> Rig {
        let closed: Arc<Mutex<Vec<Closed>>> = Default::default();
        let c = closed.clone();
        Rig {
            host: Host::new(limits, Arc::new(move |x: &Closed| c.lock().push(x.clone()))),
            closed,
            dir: tempfile::tempdir().unwrap(),
            _dog: watchdog("a terminal test", 60),
        }
    }

    fn quick() -> Limits {
        Limits {
            tick: Duration::from_millis(50),
            ..Limits::default()
        }
    }

    /// A plain interactive `sh` in a throwaway home (never the user's own).
    fn opening(r: &Rig, id: &str, owner: &str, args: &[&str]) -> Opening {
        let home = r.dir.path().to_path_buf();
        Opening {
            id: id.into(),
            owner: owner.into(),
            owner_name: format!("{owner}'s phone"),
            place: "this server".into(),
            sandboxed: false,
            start: crate::pty::Start {
                program: "/bin/sh".into(),
                args: args.iter().map(|a| a.into()).collect(),
                cwd: home.clone(),
                env: vec![
                    ("PATH".into(), "/usr/bin:/bin".into()),
                    ("HOME".into(), home.into_os_string()),
                    ("PS1".into(), "$ ".into()),
                    ("TERM".into(), "xterm-256color".into()),
                ],
                cols: 80,
                rows: 24,
                sandbox: None,
            },
        }
    }

    fn sink(conn: u64) -> (Sink, mpsc::Receiver<Out>) {
        let (tx, rx) = mpsc::channel(1024);
        (Sink { conn, tx }, rx)
    }

    /// Output (acknowledged as it comes, unless `ack` is off) until `want`
    /// shows up; then the text, and the exit if it came.
    async fn until(
        host: &Host,
        rx: &mut mpsc::Receiver<Out>,
        owner: &str,
        want: &str,
        ack: bool,
    ) -> (String, Option<(Option<i32>, String)>) {
        let mut text = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let Ok(Some(out)) = tokio::time::timeout_at(deadline, rx.recv()).await else {
                panic!("no {want:?} in {text:?}");
            };
            match out {
                Out::Output { id, seq, data } => {
                    text.push_str(&String::from_utf8_lossy(&data));
                    if ack {
                        // It may have ended meanwhile (its last output still queued).
                        let _ = host.ack(&id, owner, seq + data.len() as u64);
                    }
                }
                Out::Exit { code, reason, .. } => return (text, Some((code, reason))),
            }
            if text.contains(want) {
                return (text, None);
            }
        }
    }

    #[tokio::test]
    async fn a_shell_runs_types_resizes_and_tells_how_it_ended() {
        let r = rig(quick());
        let (s, mut rx) = sink(1);
        let info = r.host.open(opening(&r, "t1", "phone", &["-i"]), s).unwrap();
        assert_eq!(info.shell, "sh");
        r.host
            .input("t1", "phone", b"echo $((6*7))\r")
            .await
            .unwrap();
        let (out, _) = until(&r.host, &mut rx, "phone", "42", true).await;
        assert!(out.contains("42"), "{out:?}");
        r.host.resize("t1", "phone", 100, 30).unwrap();
        r.host.input("t1", "phone", b"stty size\r").await.unwrap();
        let (out, _) = until(&r.host, &mut rx, "phone", "30 100", true).await;
        assert!(out.contains("30 100"), "{out:?}");
        r.host.input("t1", "phone", b"exit 5\r").await.unwrap();
        let (_, end) = until(&r.host, &mut rx, "phone", "\u{0}never", true).await;
        assert_eq!(end, Some((Some(5), "exited with code 5".into())));
        assert_eq!(r.host.count(), 0);
        let c = r.closed.lock();
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].owner.as_str(), c[0].code), ("phone", Some(5)));
        // Asked about later, it says how it ended.
        assert!(
            r.host
                .attach("t1", "phone", sink(2).0, None)
                .unwrap_err()
                .contains("exited with code 5")
        );
    }

    #[tokio::test]
    async fn only_the_device_that_opened_it_can_use_it() {
        let r = rig(quick());
        let (s, mut rx) = sink(1);
        r.host.open(opening(&r, "t1", "phone", &["-i"]), s).unwrap();
        let other = |e: String| assert!(e.contains("isn't open"), "{e}");
        other(r.host.attach("t1", "laptop", sink(2).0, None).unwrap_err());
        other(
            r.host
                .input("t1", "laptop", b"echo hi\r")
                .await
                .unwrap_err(),
        );
        other(r.host.resize("t1", "laptop", 10, 10).unwrap_err());
        other(r.host.ack("t1", "laptop", 10).unwrap_err());
        other(r.host.close("t1", "laptop", "closed").unwrap_err());
        assert!(r.host.list(Some("laptop")).is_empty());
        assert_eq!(r.host.list(Some("phone")).len(), 1);
        // Signed out: its terminals end at once, others' stay.
        let (s2, _rx2) = sink(3);
        r.host
            .open(opening(&r, "t2", "laptop", &["-i"]), s2)
            .unwrap();
        r.host.close_owner("phone", "its device was signed out");
        let (_, end) = until(&r.host, &mut rx, "phone", "\u{0}never", true).await;
        assert_eq!(end.unwrap().1, "its device was signed out");
        assert_eq!(r.host.list(None).len(), 1);
        r.host.close_all("done");
    }

    #[tokio::test]
    async fn at_most_so_many_at_once() {
        let r = rig(Limits {
            max_open: 2,
            max_total: 3,
            ..quick()
        });
        for id in ["a", "b"] {
            r.host
                .open(opening(&r, id, "phone", &["-i"]), sink(1).0)
                .unwrap();
        }
        let e = r
            .host
            .open(opening(&r, "c", "phone", &["-i"]), sink(1).0)
            .unwrap_err();
        assert!(e.contains("2 terminals of this device are open"), "{e}");
        // Each device counts for itself: the phone's don't keep the laptop out…
        r.host
            .open(opening(&r, "d", "laptop", &["-i"]), sink(2).0)
            .unwrap();
        // …up to the overall limit.
        let e = r
            .host
            .open(opening(&r, "e", "laptop", &["-i"]), sink(2).0)
            .unwrap_err();
        assert!(e.contains("from all your devices"), "{e}");
        r.host.close("a", "phone", "closed").unwrap();
        r.host
            .open(opening(&r, "c", "phone", &["-i"]), sink(1).0)
            .unwrap();
        r.host.close_all("done");
    }

    /// Typing doesn't wait for the program: a program that doesn't read
    /// holds up only its own terminal, and past 1 MB waiting more is
    /// refused, with the terminal saying why.
    #[tokio::test]
    async fn typing_into_a_program_that_doesnt_read_is_capped() {
        let r = rig(quick());
        let (s, mut rx) = sink(1);
        r.host
            .open(opening(&r, "stuck", "phone", &["-i"]), s)
            .unwrap();
        // Raw mode (in line mode Linux drops what doesn't fit instead), no
        // echo, and a program that never reads.
        r.host
            .input(
                "stuck",
                "phone",
                b"stty raw -echo; echo READ$((1+1))Y; sleep 30\r",
            )
            .await
            .unwrap();
        until(&r.host, &mut rx, "phone", "READ2Y", true).await;
        let piece = vec![b'x'; MAX_INPUT];
        let t0 = std::time::Instant::now();
        let mut refused = None;
        for i in 0..20 {
            if let Err(e) = r.host.type_in("stuck", "phone", &piece) {
                refused = Some((i, e));
                break;
            }
        }
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "typing waited: {:?}",
            t0.elapsed()
        );
        let (i, e) = refused.expect("refused past 1 MB");
        // 16 pieces are 1 MB; the terminal itself holds a little more.
        assert!((16..=19).contains(&i), "refused at piece {i}");
        assert!(e.contains("isn't reading what you type"), "{e}");
        // The terminal itself says so (once).
        let (out, _) = until(&r.host, &mut rx, "phone", "the rest wasn't typed", true).await;
        assert!(
            out.contains("[Tessera: the program in this terminal"),
            "{out:?}"
        );
        assert!(r.host.type_in("stuck", "phone", b"y").is_err());
        // Another terminal answers at once all the same.
        let (s2, mut rx2) = sink(2);
        r.host
            .open(opening(&r, "fine", "phone", &["-i"]), s2)
            .unwrap();
        r.host
            .type_in("fine", "phone", b"echo OK-$((3+4))\r")
            .unwrap();
        until(&r.host, &mut rx2, "phone", "OK-7", true).await;
        // Closing the stuck one ends its typing too.
        r.host.close("stuck", "phone", "closed").unwrap();
        assert_eq!(r.host.count(), 1);
        r.host.close_all("done");
    }

    #[tokio::test]
    async fn a_noisy_program_waits_for_the_device() {
        let r = rig(Limits {
            window: 8 << 10,
            ..quick()
        });
        let (s, mut rx) = sink(1);
        r.host
            .open(
                opening(
                    &r,
                    "t",
                    "phone",
                    &[
                        "-c",
                        "yes 0123456789abcdef | head -c 400000; echo; echo DONE",
                    ],
                ),
                s,
            )
            .unwrap();
        // Nothing acknowledged: it stops a window (and one read) ahead.
        let mut got = 0usize;
        while let Ok(Some(Out::Output { data, .. })) =
            tokio::time::timeout(Duration::from_millis(600), rx.recv()).await
        {
            got += data.len();
        }
        assert!(got > 0 && got <= (8 << 10) + (16 << 10), "{got}");
        // Acknowledged as drawn, the rest comes.
        r.host.ack("t", "phone", got as u64).unwrap();
        let (out, _) = until(&r.host, &mut rx, "phone", "DONE", true).await;
        assert!(out.contains("DONE"));
    }

    #[tokio::test]
    async fn shown_again_it_draws_what_was_kept_and_unshown_it_closes_after_the_grace() {
        let r = rig(Limits {
            grace: Duration::from_millis(600),
            ..quick()
        });
        let (s, mut rx) = sink(1);
        r.host.open(opening(&r, "t", "phone", &["-i"]), s).unwrap();
        r.host
            .input("t", "phone", b"echo kept-$((2+3))\r")
            .await
            .unwrap();
        until(&r.host, &mut rx, "phone", "kept-5", true).await;
        r.host.detach_conn(1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Back on another connection within the grace: the kept output first.
        let (s2, mut rx2) = sink(2);
        r.host.attach("t", "phone", s2, Some((90, 20))).unwrap();
        let Ok(Some(Out::Output { seq, data, .. })) =
            tokio::time::timeout(Duration::from_secs(10), rx2.recv()).await
        else {
            panic!("no replay")
        };
        assert_eq!(seq, 0, "from the start of what was kept");
        assert!(String::from_utf8_lossy(&data).contains("kept-5"));
        // Gone again, past the grace: closed.
        r.host.detach("t", "phone").unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(r.host.count(), 0);
        assert!(r.closed.lock()[0].reason.contains("no device showed it"));
    }

    #[tokio::test]
    async fn an_untouched_shell_closes_but_not_while_a_program_runs() {
        let r = rig(Limits {
            idle: Duration::from_millis(400),
            idle_running: Duration::from_millis(2500),
            ..quick()
        });
        r.host
            .open(opening(&r, "idle", "phone", &["-i", "-m"]), sink(1).0)
            .unwrap();
        let (s, mut rx) = sink(2);
        r.host
            .open(opening(&r, "busy", "phone", &["-i", "-m"]), s)
            .unwrap();
        r.host.input("busy", "phone", b"sleep 30\r").await.unwrap();
        until(&r.host, &mut rx, "phone", "sleep 30", true).await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let open: Vec<String> = r.host.list(None).into_iter().map(|i| i.id).collect();
        assert_eq!(open, ["busy"], "the quiet one closed, the busy one didn't");
        assert!(r.closed.lock()[0].reason.contains("without typing"));
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert_eq!(
            r.host.count(),
            0,
            "a program running doesn't keep it open forever"
        );
    }
}

#[cfg(all(test, windows))]
#[path = "terminal/windows_tests.rs"]
mod windows_tests;
