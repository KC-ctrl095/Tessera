//! The running node: says hello on every (re)connection, runs the actions
//! the server routes to it, follows local changes to its settings, and
//! stops running actions the moment it is turned off, paused, revoked,
//! narrowed or loses its connection.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tessera_proto::{
    AdminAccess, ApiError, ErrorCode, Event, MachineAsk, MachineScope, NodeAction, NodeCall,
    NodeLogEntry, NodeLogEvent, NodeResult, NodeWelcome, PermissionMode, Request,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::exec::{self, Outcome};
use super::log::{NodeLog, entry, target_of};
use super::{NodeConfig, NodeState};
use crate::config::Paths;

/// How the node reaches the server: the paired device's end-to-end
/// encrypted connection (`tessera_client::Client` in the CLI).
pub trait Link: Send + Sync + 'static {
    fn call(&self, req: Request) -> BoxFuture<'static, Result<Value, ApiError>>;
}

/// What happens on the connection.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // short-lived; boxing every event isn't worth it
pub enum LinkEvent {
    /// Connected (again), to this address.
    Up(Option<String>),
    /// Disconnected, and why.
    Down(Option<String>),
    Event(Event),
}

/// Why the node stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Turned off on this computer.
    Disabled,
    /// Turned off from another device.
    Revoked,
    /// Asked to stop (signal, or the connection's owner went away).
    Stopped,
}

struct Running {
    action: NodeAction,
    bot: Option<(String, String)>,
    cancel: CancellationToken,
    why: Mutex<Option<String>>,
}

pub struct Node {
    paths: Paths,
    /// This computer's home folder (`~` in actions; the startup files).
    home: Option<std::path::PathBuf>,
    /// The desktop session to hand commands, as variables, instead of
    /// finding the running one (tests use a stand-in).
    session_env: Option<Arc<HashMap<String, String>>>,
    backups: super::backups::Backups,
    config: RwLock<NodeConfig>,
    /// `node.json` and `stop.json` as last read.
    stamp: Mutex<Stamp>,
    /// A Stop pressed here that couldn't be written (when): it holds in
    /// memory until a Resume after it.
    unsaved_stop: Mutex<Option<u64>>,
    calls: Mutex<HashMap<String, Arc<Running>>>,
    paused: AtomicBool,
    welcomed: AtomicBool,
    log: NodeLog,
    state: Mutex<NodeState>,
    /// "Web views here": the browser it runs for web views.
    browser: super::browser::NodeBrowser,
    /// "Terminal from my devices": the terminals open here.
    terminals: super::terminal::NodeTerminals,
    /// Which chat is using the screen (desktop actions, one chat at a time).
    desktop: Mutex<super::screen::Lease>,
    /// Tessera Guard took this machine in (Windows): `None` without a guard
    /// (or not asked yet), else how asking went.
    taken_in: Mutex<Option<Result<(), String>>>,
}

/// A chat's turn at the screen (`Lease`), ended when dropped: its minute
/// counts from then.
struct ScreenTurn {
    node: Arc<Node>,
    chat: String,
}

impl Drop for ScreenTurn {
    fn drop(&mut self) {
        self.node
            .desktop
            .lock()
            .end(&self.chat, std::time::Instant::now());
    }
}

/// An action on the Stop bar, taken off however it ends.
struct Shown(String);

impl Drop for Shown {
    fn drop(&mut self) {
        super::stopbar::end(&self.0);
    }
}

/// `node.json` and the user's Stop (`stop.json` and the Stop kept beside
/// it) as last read (modified time, size), and the Stop as it then was
/// wherever it is kept (the registry too): any change means reading the
/// settings again.
type Stamp = ([Option<(std::time::SystemTime, u64)>; 3], (bool, u32));

fn stamp_of(paths: &Paths) -> Stamp {
    let one = |p: std::path::PathBuf| {
        let m = std::fs::metadata(p).ok()?;
        Some((m.modified().ok()?, m.len()))
    };
    let latch = super::StopLatch::load(paths);
    (
        [
            one(super::config_file(paths)),
            one(super::stop_file(paths)),
            one(paths.config_dir.join("stop.pending")),
        ],
        (latch.stopped, latch.stops),
    )
}

impl Node {
    /// The node for this computer; fails unless it was turned on here.
    pub fn open(paths: Paths) -> anyhow::Result<Arc<Self>> {
        Self::open_at(paths, super::home())
    }

    /// The same with `home` as the home folder (tests use a stand-in).
    pub fn open_at(paths: Paths, home: Option<std::path::PathBuf>) -> anyhow::Result<Arc<Self>> {
        Self::open_with(paths, home, None)
    }

    /// The same, and with the desktop session described by `session_env`
    /// (its variables) rather than the running one.
    pub fn open_with(
        paths: Paths,
        home: Option<std::path::PathBuf>,
        session_env: Option<HashMap<String, String>>,
    ) -> anyhow::Result<Arc<Self>> {
        let config = NodeConfig::load(&paths)?;
        if !config.enabled {
            anyhow::bail!(
                "this computer isn't turned on as a machine; run `tessera node enable` on it first"
            );
        }
        let stamp = stamp_of(&paths);
        let terminals = super::terminal::NodeTerminals::new(paths.clone(), home.clone());
        let node = Arc::new(Self {
            log: NodeLog::new(super::log_file(&paths)),
            backups: super::backups::Backups::new(super::backups::dir(&paths)),
            home,
            session_env: session_env.map(Arc::new),
            browser: super::browser::NodeBrowser::new(paths.clone()),
            terminals,
            desktop: Mutex::new(super::screen::Lease::default()),
            taken_in: Mutex::new(None),
            paths,
            config: RwLock::new(config),
            stamp: Mutex::new(stamp),
            unsaved_stop: Mutex::new(None),
            calls: Mutex::new(HashMap::new()),
            paused: AtomicBool::new(false),
            welcomed: AtomicBool::new(false),
            state: Mutex::new(NodeState {
                pid: std::process::id(),
                ..Default::default()
            }),
        });
        node.write_state(|_| {});
        Ok(node)
    }

    pub fn config(&self) -> NodeConfig {
        self.config.read().clone()
    }

    /// Actions running now.
    pub fn running(&self) -> usize {
        self.calls.lock().len()
    }

    fn write_state(&self, f: impl FnOnce(&mut NodeState)) {
        let mut st = self.state.lock();
        f(&mut st);
        st.calls = self.calls.lock().len() as u32;
        st.paused = self.paused.load(Ordering::SeqCst);
        st.stop_bar = super::stopbar::status();
        st.guard_error = self.taken_in.lock().clone().and_then(|r| r.err());
        st.updated_at = crate::util::now_ms();
        if let Ok(text) = serde_json::to_string(&*st) {
            let _ =
                crate::config::write_atomic(&super::state_file(&self.paths), text.as_bytes(), true);
        }
    }

    fn clear_state(&self) {
        let _ = std::fs::remove_file(super::state_file(&self.paths));
    }

    fn log_event(&self, event: NodeLogEvent, detail: impl Into<String>) {
        self.log.add(&entry(event, detail));
    }

    fn log_call(
        &self,
        call: &NodeCall,
        event: NodeLogEvent,
        detail: impl Into<String>,
        ms: Option<u64>,
    ) {
        let mut e: NodeLogEntry = entry(event, detail);
        e.call_id = Some(call.call_id.clone());
        e.session_id = Some(call.session_id.clone());
        e.chat = Some(crate::util::one_line(&call.chat, 120));
        e.action = Some(
            if matches!(call.action, NodeAction::Run { admin: true, .. }) {
                "run as administrator"
            } else {
                call.action.name()
            }
            .into(),
        );
        e.target = Some(target_of(&call.action));
        e.duration_ms = ms;
        e.bot = call
            .bot
            .as_ref()
            .map(|b| crate::util::one_line(&b.name, 80));
        self.log.add(&e);
    }

    /// Stop every running action, saying why.
    fn stop_all(&self, why: &str) {
        for r in self.calls.lock().values() {
            r.why.lock().get_or_insert_with(|| why.to_string());
            r.cancel.cancel();
        }
    }

    fn stop_one(&self, call_id: &str, why: &str) {
        if let Some(r) = self.calls.lock().get(call_id) {
            r.why.lock().get_or_insert_with(|| why.to_string());
            r.cancel.cancel();
        }
    }

    /// Re-read `node.json` if it changed. `Err(())` when turned off.
    fn reload(&self) -> Result<bool, ()> {
        // One at a time, and never during another change of the Stop (the
        // Stop bar's own task and the regular look both change it): every
        // such change takes the stamp's lock for its whole length.
        let mut stamp = self.stamp.lock();
        let now = stamp_of(&self.paths);
        if now == *stamp {
            return Ok(false);
        }
        let mut new = match NodeConfig::load(&self.paths) {
            Ok(c) => {
                *stamp = now;
                c
            }
            // Being rewritten, or unreadable: the settings we have, but the
            // user's Stop applies all the same (read again next time).
            Err(_) => {
                let mut c = self.config.read().clone();
                super::mirror_stop(&mut c.scope, super::StopLatch::load(&self.paths));
                c
            }
        };
        // A Stop that couldn't be written holds until it is (each second;
        // only Resume on the bar undoes it before that): nothing written
        // elsewhere meanwhile, a Resume included, can undo it unseen.
        if self.unsaved_stop.lock().is_some() {
            new.scope.stopped = true;
        }
        // The count of Stops only grows (what this machine counted stays,
        // so an approval from before a Stop never counts again).
        let (stopped_was, stops_was, terminal_was) = {
            let c = self.config.read();
            (c.scope.stopped, c.scope.stops, c.scope.terminal)
        };
        new.scope.stops = new.scope.stops.max(stops_was);
        // A Stop seen now counts, even when the file's count was behind ours.
        if new.scope.stopped && !stopped_was && new.scope.stops <= stops_was {
            new.scope.stops = stops_was + 1;
        }
        // The file never stays behind what this machine counted (tried
        // again each second until it is; meanwhile nothing is admitted).
        self.catch_up_count(new.scope.stops);
        if !new.enabled || new.revoked {
            return Err(());
        }
        let changed = new != *self.config.read();
        if changed {
            *self.config.write() = new.clone();
            self.apply_stop(stopped_was, stops_was, new.scope.stopped, new.scope.stops);
            self.show_bar();
            if !new.scope.webviews {
                self.browser
                    .stop("web views were switched off on this computer");
            }
            if new.scope.terminal != terminal_was {
                self.terminals
                    .close_all("the terminal setting changed on this computer");
            }
            // Stop what the narrower settings no longer allow.
            for r in self.calls.lock().values() {
                let bot = r.bot.as_ref().map(|(i, h)| (i.as_str(), h.as_str()));
                if let Err(why) = exec::permitted(
                    &self.paths,
                    self.home.as_deref(),
                    &new.scope,
                    &r.action,
                    bot,
                ) {
                    r.why
                        .lock()
                        .get_or_insert_with(|| format!("no longer allowed on this machine: {why}"));
                    r.cancel.cancel();
                }
            }
        }
        Ok(changed)
    }

    fn hello(&self) -> Request {
        let c = self.config.read();
        Request::NodeHello {
            name: c.name.clone(),
            os: super::os(),
            version: env!("CARGO_PKG_VERSION").into(),
            scope: c.scope.clone(),
            generation: c.generation,
            sandbox: crate::sandbox::describe(),
            sandbox_ok: super::commands_run_here(&c.scope),
            home: self.home.as_ref().map(|h| h.display().to_string()),
            computer: tessera_secure::computer_key(),
            host: tessera_secure::host_name(),
            features: Some(super::features()),
            // Tessera Guard here (Windows): the server pins its key and asks
            // its signed word before every action ("": a guard that can't
            // sign yet, so the server lets nothing run here).
            guard_key: match crate::guard::here() {
                crate::guard::Here::Absent => None,
                _ => Some(crate::guard::public_key().unwrap_or_default()),
            },
        }
    }

    /// The hello for a server too old to know "only risky ones": it is
    /// told "every action" (it then asks for everything, which only adds
    /// approvals; this machine still checks with its own setting).
    fn compat_hello(&self) -> Request {
        let mut hello = self.hello();
        if let Request::NodeHello { scope, .. } = &mut hello
            && scope.ask == MachineAsk::Risky
        {
            scope.ask = MachineAsk::Always;
        }
        hello
    }

    /// Turned off from another device: stop, and stay off until turned on here.
    fn revoke(&self) {
        self.stop_all("this machine was turned off from another device");
        self.terminals
            .close_all("this machine was turned off from another device");
        self.browser
            .stop("this machine was turned off from another device");
        if let Ok(mut c) = NodeConfig::load(&self.paths) {
            c.enabled = false;
            c.revoked = true;
            let _ = c.save(&self.paths);
        }
        self.log_event(
            NodeLogEvent::Revoked,
            "turned off from another device; turn it on here again with `tessera node enable`",
        );
    }

    /// The user's Stop as it now is (`stopped`, and how many there were):
    /// a Stop since the last look ends everything here, even when it was
    /// already resumed meanwhile; desktop actions follow the latest state.
    /// Every way of stopping (the Stop bar, its key, `tessera node stop`,
    /// the app) writes `stop.json`, which the reload brings here.
    fn apply_stop(&self, stopped_was: bool, stops_was: u32, stopped: bool, stops: u32) {
        let stopped_since = stops > stops_was;
        if stopped || stopped_since {
            // Windows only: elsewhere there are no desktop actions to end
            // (and the switch is one for the whole program).
            if super::screen::AVAILABLE {
                super::screen::halt();
            }
            self.stop_all(super::STOPPED_HERE);
            // Web views here are agents' too; terminals are the user's own.
            self.browser
                .stop("the user stopped Tessera on this computer");
            if super::screen::AVAILABLE {
                // Counted before anything can resume (no new input until it
                // is done); a drag's button at once, then whatever Windows
                // still holds.
                let letting_go = super::screen::begin_let_go();
                super::screen::release_live();
                std::thread::spawn(move || super::screen::let_go(letting_go));
            }
            if !stopped_was {
                self.log_event(
                    NodeLogEvent::Paused,
                    "stopped on this computer: every action ended, and nothing runs until it is resumed here",
                );
            }
        }
        if !stopped {
            if super::screen::AVAILABLE {
                super::screen::resume();
            }
            if stopped_was || stopped_since {
                self.log_event(NodeLogEvent::Resumed, "resumed on this computer");
            }
        }
        self.write_state(|_| {});
    }

    /// The Stop bar as the settings say (Windows: its key while the machine
    /// runs; the bar always with desktop control, otherwise while agents work).
    fn show_bar(&self) {
        let (desktop, stopped) = {
            let c = self.config.read();
            (c.scope.desktop, c.scope.stopped)
        };
        let on = super::screen::AVAILABLE;
        super::stopbar::sync(on, desktop && on, stopped);
        self.write_state(|_| {});
    }

    /// Its settings as they are now, to the server (through the one sender).
    fn report_scope(
        &self,
        report: &tokio::sync::watch::Sender<Option<(MachineScope, Option<String>)>>,
    ) {
        let c = self.config.read();
        report.send_replace(Some((c.scope.clone(), Some(c.name.clone()))));
    }

    /// Stop or Resume pressed on the Stop bar: saved like `tessera node
    /// stop`/`resume`. A Stop applies here at once (nothing new is admitted
    /// meanwhile) and the server is told; a Resume with the next reload
    /// (within a second).
    fn bar_event(&self, event: super::stopbar::BarEvent) -> bool {
        let _turn = self.stamp.lock();
        let stopped = event == super::stopbar::BarEvent::Stop;
        if stopped {
            // The Stop is already in force (the bar halted desktop actions);
            // commands and the rest end now, before anything is written.
            self.stop_all(super::STOPPED_HERE);
        }
        let counted = self.config.read().scope.stops;
        match super::StopLatch::set(&self.paths, stopped, counted) {
            Ok(l) if stopped => {
                let (was, stops_was) = {
                    let c = self.config.read();
                    (c.scope.stopped, c.scope.stops)
                };
                let stops = {
                    let mut c = self.config.write();
                    c.scope.stopped = true;
                    c.scope.stops = c.scope.stops.max(l.stops);
                    c.scope.stops
                };
                self.apply_stop(was, stops_was, true, stops);
                self.show_bar();
                true
            }
            Ok(_) => {
                *self.unsaved_stop.lock() = None;
                false
            }
            Err(e) if !stopped => {
                // A Resume that couldn't be saved resumes nothing: stopped
                // as before (the bar shows it), to be pressed again.
                self.log_event(
                    NodeLogEvent::Paused,
                    format!("couldn't resume ({e}); still stopped: press Resume again"),
                );
                self.show_bar();
                false
            }
            Err(e) => {
                // Saved nowhere (not beside stop.json, not in the registry):
                // held in memory until a Resume here (a restart would lose
                // it: the log says so), and saved as soon as it can be.
                let (was, stops_was) = {
                    let c = self.config.read();
                    (c.scope.stopped, c.scope.stops)
                };
                {
                    let mut c = self.config.write();
                    c.scope.stopped = stopped;
                    if stopped && !was {
                        c.scope.stops = c.scope.stops.wrapping_add(1);
                    }
                }
                *self.unsaved_stop.lock() = Some(crate::util::now_ms());
                self.log_event(
                    NodeLogEvent::Paused,
                    format!(
                        "couldn't save this Stop in {} ({e}); it holds until Tessera restarts here",
                        self.paths.config_dir.display()
                    ),
                );
                let stops = self.config.read().scope.stops;
                self.apply_stop(was, stops_was, stopped, stops);
                self.show_bar();
                // Changed here only: the server is told at once.
                true
            }
        }
    }

    /// A Stop held only in memory: written as soon as it can be (each
    /// second), so the server and a Tessera server on this computer see it;
    /// and the file's count brought up to this machine's.
    fn retry_unsaved_stop(&self) {
        // Decided and written in one go: never after a newer Resume.
        let _turn = self.stamp.lock();
        let counted = self.config.read().scope.stops;
        if self.unsaved_stop.lock().is_some() {
            if super::StopLatch::set(&self.paths, true, counted).is_ok() {
                *self.unsaved_stop.lock() = None;
                self.log_event(NodeLogEvent::Paused, "the Stop is saved now");
            }
        } else {
            self.catch_up_count(counted);
        }
    }

    /// Resume with Tessera Guard: only a Resume approved in Windows'
    /// administrator prompt counts. Asked on its own thread (the user takes
    /// their time); the next look applies it.
    fn resume_through_guard(self: &Arc<Self>) {
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            match crate::guard::resume_with_approval() {
                Ok(()) => {
                    // Kept here too, for what reads only this computer's files.
                    let seen = super::StopLatch::load(&me.paths).stops;
                    let _ = super::StopLatch::set(&me.paths, false, seen);
                    *me.unsaved_stop.lock() = None;
                }
                Err(why) => me.log_event(NodeLogEvent::Paused, format!("not resumed: {why}")),
            }
        });
    }

    /// Tessera Guard (Windows), each second: take this machine in while it
    /// hasn't (the guard may come later), and bring it Stops counted here
    /// that it hasn't got (it couldn't be reached then).
    fn guard_tick(&self) {
        let crate::guard::Here::Says(g) = crate::guard::here() else {
            return;
        };
        if !matches!(*self.taken_in.lock(), Some(Ok(()))) {
            self.take_in();
        }
        let local = super::StopLatch::load_local(&self.paths);
        if local.stopped && local.stops > g.stops {
            crate::guard::forward_stop(local.stops);
        }
    }

    /// Ask Tessera Guard to take this machine in. What runs from before
    /// (outside its reach) ends when it first does.
    fn take_in(&self) {
        let r = crate::guard::adopt_me();
        if let Err(why) = &r {
            tracing::warn!("Tessera Guard didn't take this machine in: {why}");
        }
        let first = r.is_ok() && !matches!(*self.taken_in.lock(), Some(Ok(())));
        *self.taken_in.lock() = Some(r);
        if first && !self.calls.lock().is_empty() {
            self.stop_all(
                "Tessera Guard was set up on this computer: what ran before it (out of its reach) was ended",
            );
        }
        self.write_state(|_| {});
    }

    /// Why `action` can't run for Tessera Guard's sake (Windows): it can't
    /// be asked, or hasn't taken this machine in (asked now if it hasn't
    /// yet); or commands and the screen without it (or with a User Account
    /// Control anyone could answer).
    fn guard_gate(&self, action: &NodeAction) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        let here = crate::guard::here();
        if matches!(here, crate::guard::Here::Says(_))
            && !matches!(*self.taken_in.lock(), Some(Ok(())))
        {
            self.take_in();
        }
        let needs = matches!(action, NodeAction::Run { .. } | NodeAction::Desktop { .. });
        let taken = self.taken_in.lock().clone();
        match (&here, taken) {
            (crate::guard::Here::Absent, _) if !needs => None,
            (crate::guard::Here::Says(_), Some(Ok(()))) if !needs => None,
            (crate::guard::Here::Says(_), None) => {
                Some("Tessera Guard hasn't taken this machine in yet; try again in a moment".into())
            }
            (here, taken) => crate::guard::blocked(here, taken.as_ref()),
        }
    }

    /// `stop.json`'s count up to `counted` when it is behind.
    fn catch_up_count(&self, counted: u32) {
        if super::StopLatch::load(&self.paths).stops < counted {
            let _ = super::StopLatch::raise(&self.paths, counted);
        }
    }

    fn set_paused(&self, paused: bool) {
        if self.paused.swap(paused, Ordering::SeqCst) == paused {
            return;
        }
        if paused {
            self.stop_all("this machine was paused from another device");
            self.terminals
                .close_all("this machine was paused from another device");
            self.browser
                .stop("this machine was paused from another device");
            self.log_event(NodeLogEvent::Paused, "paused from another device");
        } else {
            self.log_event(NodeLogEvent::Resumed, "resumed from another device");
        }
        self.write_state(|_| {});
    }

    /// Run until turned off, revoked or stopped.
    pub async fn serve(
        self: Arc<Self>,
        link: Arc<dyn Link>,
        mut events: mpsc::UnboundedReceiver<LinkEvent>,
        stop: CancellationToken,
    ) -> Exit {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Stopped before (even before a restart): still stopped. The Stop
        // bar shows while desktop control is on; its Stop and Resume come here.
        let (bar_tx, mut bar_rx) = mpsc::unbounded_channel();
        if super::screen::AVAILABLE {
            if self.config.read().scope.stopped {
                super::screen::halt();
            }
            super::stopbar::listen(bar_tx);
        } else {
            drop(bar_tx);
        }
        // Tessera Guard (Windows) takes this machine in before it does
        // anything: on a Stop, or when this process goes, it ends this and
        // everything it started.
        self.guard_tick();
        // Keys or buttons left down when an earlier run was ended (by the
        // guard, say): let go of them now.
        if super::screen::AVAILABLE && self.config.read().scope.desktop {
            super::screen::release_stuck();
        }
        self.show_bar();
        // Its settings go to the server in order through one sender (the
        // newest wins): a report overtaken by an older one could undo a Stop
        // there.
        let (report, mut reports) = tokio::sync::watch::channel(None);
        let report = Arc::new(report);
        {
            let link = link.clone();
            tokio::spawn(async move {
                while reports.changed().await.is_ok() {
                    let next: Option<(MachineScope, Option<String>)> =
                        reports.borrow_and_update().clone();
                    if let Some((scope, name)) = next {
                        let _ = link.call(Request::NodeScope { scope, name }).await;
                    }
                }
            });
        }
        // The user's Stop and Resume on the bar are handled on their own,
        // never behind a slow answer from the server (a hello can wait 20 s).
        let bars = {
            let (me, report) = (self.clone(), report.clone());
            tokio::spawn(async move {
                while let Some(event) = bar_rx.recv().await {
                    // With Tessera Guard only Windows' administrator prompt
                    // resumes.
                    if event == super::stopbar::BarEvent::Resume
                        && !matches!(crate::guard::here(), crate::guard::Here::Absent)
                    {
                        me.resume_through_guard();
                        continue;
                    }
                    // Saved: taken in and told at once rather than at the next
                    // look, so a server on this computer hears it from here as
                    // it reads it from the Stop file.
                    let told = me.bar_event(event) || matches!(me.reload(), Ok(true));
                    if told && me.welcomed.load(Ordering::SeqCst) {
                        me.report_scope(&report);
                    }
                }
            })
        };
        // The Stop bar or its key stopped working: whatever runs here ends
        // (nothing new is admitted meanwhile, see `precheck`). On its own,
        // never behind a slow answer from the server.
        let controls = {
            let me = self.clone();
            tokio::spawn(async move {
                let mut look = tokio::time::interval(Duration::from_millis(500));
                let mut worked = true;
                loop {
                    look.tick().await;
                    // Tessera Guard installed but not answering: the same.
                    let ready = super::stopbar::controls_ready().and_then(|()| {
                        match cfg!(windows).then(crate::guard::here) {
                            Some(crate::guard::Here::Silent(why)) => Err(format!(
                                "Tessera Guard isn't answering on this computer ({why}), so what ran here was ended"
                            )),
                            _ => Ok(()),
                        }
                    });
                    match ready {
                        Err(why) => {
                            me.stop_all(&why);
                            if worked {
                                me.browser.stop(&why);
                            }
                            worked = false;
                        }
                        Ok(()) => worked = true,
                    }
                }
            })
        };
        // Tessera Guard's signed words being asked for the server at once.
        let attesting = Arc::new(tokio::sync::Semaphore::new(4));
        // Connected but not yet welcomed: when to say hello (again).
        let mut hello_at: Option<tokio::time::Instant> = None;
        let mut endpoint: Option<String> = None;
        let exit = loop {
            tokio::select! {
                _ = stop.cancelled() => {
                    // `tessera node disable` turns it off, then stops it.
                    if NodeConfig::load(&self.paths).is_ok_and(|c| !c.enabled) {
                        self.stop_all("this machine was turned off on this computer");
                        break Exit::Disabled;
                    }
                    self.stop_all("Tessera was stopped on this computer");
                    break Exit::Stopped;
                }
                _ = tick.tick() => {
                    self.retry_unsaved_stop();
                    self.guard_tick();
                    match self.reload() {
                        Err(()) => {
                            self.stop_all("this machine was turned off on this computer");
                            break Exit::Disabled;
                        }
                        // Its name too: renamed here (`tessera node name`).
                        Ok(true) if self.welcomed.load(Ordering::SeqCst) => self.report_scope(&report),
                        _ => {}
                    }
                    if hello_at.is_some_and(|t| t <= tokio::time::Instant::now()) {
                        hello_at = None;
                        match self.greet(&link, endpoint.clone()).await {
                            // What changed while it waited (a Stop) follows.
                            Greeting::Welcomed => self.report_scope(&report),
                            Greeting::Revoked => break Exit::Revoked,
                            Greeting::Retry => hello_at = Some(tokio::time::Instant::now() + Duration::from_secs(5)),
                        }
                    }
                }
                ev = events.recv() => match ev {
                    None => {
                        self.stop_all("Tessera was stopped on this computer");
                        break Exit::Stopped;
                    }
                    Some(LinkEvent::Up(ep)) => {
                        endpoint = ep;
                        self.browser.connected();
                        match self.greet(&link, endpoint.clone()).await {
                            Greeting::Welcomed => {
                                hello_at = None;
                                self.report_scope(&report);
                            }
                            Greeting::Revoked => break Exit::Revoked,
                            Greeting::Retry => hello_at = Some(tokio::time::Instant::now() + Duration::from_secs(5)),
                        }
                    }
                    Some(LinkEvent::Down(why)) => {
                        hello_at = None;
                        if self.welcomed.swap(false, Ordering::SeqCst) {
                            self.log_event(NodeLogEvent::Disconnected, why.clone().unwrap_or_default());
                        }
                        // Nobody is waiting for these any more.
                        self.stop_all("the connection to the server was lost");
                        self.browser.disconnected("the connection to the server was lost");
                        self.terminals.disconnected("the connection to the server was lost");
                        self.write_state(|s| {
                            s.connected = false;
                            s.error = why;
                        });
                    }
                    Some(LinkEvent::Event(Event::NodeCall { call })) => {
                        if self.welcomed.load(Ordering::SeqCst) {
                            tokio::spawn(self.clone().handle(link.clone(), call));
                        }
                    }
                    // The server asks Tessera Guard's signed word on the Stop
                    // before an action: asked of the guard, passed on as is.
                    Some(LinkEvent::Event(Event::NodeAttest { nonce })) => {
                        let link = link.clone();
                        // A few at a time; more (or an odd nonce) aren't
                        // answered at all (the server gives up after a few
                        // seconds and does nothing): nothing piles up here.
                        let slot = (nonce.len() <= 128)
                            .then(|| attesting.clone().try_acquire_owned().ok())
                            .flatten();
                        if let Some(slot) = slot {
                            tokio::spawn(async move {
                                let _slot = slot;
                                let asked = nonce.clone();
                                let said = tokio::task::spawn_blocking(move || crate::guard::attest(&asked))
                                    .await
                                    .ok()
                                    .flatten();
                                let (state, signature) = said.unwrap_or((
                                    crate::guard::State {
                                        stopped: true,
                                        ..Default::default()
                                    },
                                    String::new(),
                                ));
                                let _ = link
                                    .call(Request::NodeAttestation {
                                        nonce,
                                        stopped: state.stopped,
                                        stops: state.stops,
                                        generation: state.generation,
                                        signature,
                                    })
                                    .await;
                            });
                        }
                    }
                    Some(LinkEvent::Event(Event::NodeCancel { call_id })) => {
                        self.stop_one(&call_id, "stopped from the chat");
                    }
                    Some(LinkEvent::Event(Event::NodeRefused { call, reason })) => {
                        // Never ran here; recorded so the log has every call.
                        self.log_call(
                            &call,
                            NodeLogEvent::Refused,
                            format!("refused before it reached this machine: {}", crate::util::one_line(&reason, 300)),
                            None,
                        );
                    }
                    Some(LinkEvent::Event(Event::NodeControl { revoked: true, .. })) => {
                        self.revoke();
                        break Exit::Revoked;
                    }
                    Some(LinkEvent::Event(Event::NodeControl { paused, .. })) => self.set_paused(paused),
                    Some(LinkEvent::Event(Event::NodeBrowser { msgs })) => {
                        // "Web views here" (it checks the setting itself).
                        if self.welcomed.load(Ordering::SeqCst) {
                            // A Stop saved but not yet taken in (or not saved
                            // yet) counts here too, as for its other actions.
                            let saved = super::StopLatch::load(&self.paths);
                            let mut config = self.config.read().clone();
                            config.scope.stops = config.scope.stops.max(saved.stops);
                            config.scope.stopped |=
                                saved.stopped || self.unsaved_stop.lock().is_some();
                            let paused = self.paused.load(Ordering::SeqCst);
                            let ctx = super::browser::Ctx { config: &config, paused };
                            self.browser.handle(&link, msgs, &ctx);
                        }
                    }
                    Some(LinkEvent::Event(Event::NodeTerminal { msgs })) => {
                        // "Terminal from my devices" (it checks the setting itself).
                        if self.welcomed.load(Ordering::SeqCst) {
                            let scope = self.config.read().scope.clone();
                            let paused = self.paused.load(Ordering::SeqCst);
                            self.terminals.handle(&link, msgs, &scope, paused);
                        }
                    }
                    Some(LinkEvent::Event(_)) => {}
                },
            }
        };
        bars.abort();
        controls.abort();
        self.terminals.disconnected(match exit {
            Exit::Disabled => "this machine was turned off on this computer",
            Exit::Revoked => "this machine was turned off from another device",
            Exit::Stopped => "Tessera was stopped on this computer",
        });
        self.browser.disconnected(match exit {
            Exit::Disabled => "this machine was turned off on this computer",
            Exit::Revoked => "this machine was turned off from another device",
            Exit::Stopped => "Tessera was stopped on this computer",
        });
        // Give the replies of what was just stopped a moment to go out.
        for _ in 0..40 {
            if self.calls.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        match exit {
            Exit::Disabled => {
                self.log_event(NodeLogEvent::Disconnected, "turned off on this computer")
            }
            Exit::Stopped => self.log_event(NodeLogEvent::Disconnected, "stopped"),
            Exit::Revoked => {}
        }
        self.clear_state();
        exit
    }

    async fn greet(&self, link: &Arc<dyn Link>, endpoint: Option<String>) -> Greeting {
        let mut reply =
            tokio::time::timeout(Duration::from_secs(20), link.call(self.hello())).await;
        if matches!(&reply, Ok(Err(e)) if e.code == ErrorCode::BadRequest && e.message.contains("risky"))
        {
            reply =
                tokio::time::timeout(Duration::from_secs(20), link.call(self.compat_hello())).await;
        }
        match reply {
            Ok(Ok(v)) => match serde_json::from_value::<NodeWelcome>(v) {
                Ok(w) => {
                    self.welcomed.store(true, Ordering::SeqCst);
                    self.terminals.connected(link.clone());
                    self.paused.store(w.paused, Ordering::SeqCst);
                    self.log_event(
                        NodeLogEvent::Connected,
                        format!(
                            "connected to {}{}{}",
                            w.server,
                            endpoint
                                .as_deref()
                                .map(|e| format!(" at {e}"))
                                .unwrap_or_default(),
                            if w.paused {
                                " (paused from another device)"
                            } else {
                                ""
                            }
                        ),
                    );
                    self.write_state(|s| {
                        s.connected = true;
                        s.endpoint = endpoint;
                        s.server = Some(w.server);
                        s.error = None;
                    });
                    Greeting::Welcomed
                }
                Err(e) => {
                    self.write_state(|s| {
                        s.error = Some(format!("unexpected answer from the server: {e}"))
                    });
                    Greeting::Retry
                }
            },
            Ok(Err(e)) if e.code == ErrorCode::Unauthorized => {
                self.revoke();
                Greeting::Revoked
            }
            Ok(Err(e)) => {
                self.write_state(|s| s.error = Some(e.message));
                Greeting::Retry
            }
            Err(_) => {
                self.write_state(|s| s.error = Some("the server didn't answer".into()));
                Greeting::Retry
            }
        }
    }

    /// Why this machine won't run `call` at all, before looking at paths.
    fn precheck(&self, call: &NodeCall) -> Option<String> {
        // A Stop's count not yet saved: nothing until it is (it could be
        // counted again otherwise, and an older approval match it). Read
        // before the settings are locked.
        let counted = self.config.read().scope.stops;
        let saved = super::StopLatch::load(&self.paths);
        // Asked before the settings are locked (it may take a moment).
        let guard_says = self.guard_gate(&call.action);
        if saved.stops < counted {
            return Some("the user's latest Stop on this computer is still being saved; try again in a moment".into());
        }
        let c = self.config.read();
        if !c.enabled {
            return Some("this machine is turned off".into());
        }
        // Nothing without the Stop bar and its key (Windows): the user must
        // always be able to see and stop what agents do here.
        if let Err(why) = super::stopbar::controls_ready() {
            return Some(why);
        }
        // Tessera Guard (Windows): nothing while it can't be asked or didn't
        // take this machine in; commands and the screen not without it.
        if let Some(why) = guard_says {
            return Some(why);
        }
        // Saved but not yet taken in (`tessera node stop`, the app), or not
        // saved yet at all, counts too.
        if c.scope.stopped
            || saved.stopped
            || self.unsaved_stop.lock().is_some()
            || super::screen::halted()
        {
            return Some(super::STOPPED_HERE.into());
        }
        // Asked for by a turn from before the user's latest Stop here (also
        // one saved and resumed since the last look): never run after it.
        if call
            .stops
            .is_some_and(|s| s != saved.stops.max(c.scope.stops))
        {
            return Some(super::ASKED_BEFORE_STOP.into());
        }
        if self.paused.load(Ordering::SeqCst) {
            return Some("this machine is paused from another device; resume it there (Settings → Devices) first".into());
        }
        let bot = call
            .bot
            .as_ref()
            .map(|b| (b.id.as_str(), b.handle.as_str()));
        if let Err(why) = super::may_use(&c.scope, bot) {
            return Some(format!(
                "{why}; who may use this machine is set on this computer"
            ));
        }
        if call.mode == PermissionMode::ReadOnly && call.action.changes() {
            return Some("the chat is read-only, so this machine changes nothing for it".into());
        }
        if call.files_only && !call.action.only_reads_files() {
            return Some("this bot may only read files and list folders on this machine".into());
        }
        // As administrator: only where this computer allows it, and (by
        // default) only what the user approved, whatever the chat's mode.
        if matches!(call.action, NodeAction::Run { admin: true, .. }) {
            match c.scope.admin {
                AdminAccess::Off => {
                    return Some("administrator commands are off on this machine; only this computer can turn them on (Settings → Devices → This computer → Administrator, or `tessera node admin on`)".into());
                }
                AdminAccess::Ask if !call.approved => {
                    return Some(
                        "this machine asks before every administrator command, and this one wasn't approved in the chat"
                            .into(),
                    );
                }
                _ => {}
            }
        }
        if !call.approved {
            match c.scope.ask {
                MachineAsk::Always => {
                    return Some(
                        "this machine asks before every action, and this one wasn't approved in the chat"
                            .into(),
                    );
                }
                MachineAsk::Risky => {
                    let access = exec::Access::new(&self.paths, &c.scope, self.home.as_deref());
                    if let Some(why) = super::risky(&call.action, |p| {
                        c.scope.desktop && access.changes_startup(p)
                    }) {
                        return Some(format!(
                            "this machine asks before risky actions, and this one wasn't approved in the chat ({why})"
                        ));
                    }
                }
                MachineAsk::Chat => {}
            }
        }
        if let NodeAction::Run { admin, .. } = &call.action {
            if !c.scope.commands {
                return Some("commands are off on this machine (only that computer can turn them on: Work in folders or Full access)".into());
            }
            // Administrator commands run outside the sandbox (that is what
            // they are for), so they don't need it here.
            if !admin && !super::commands_run_here(&c.scope) {
                return Some(super::commands_refused_why());
            }
        }
        None
    }

    async fn handle(self: Arc<Self>, link: Arc<dyn Link>, call: NodeCall) {
        let reply = |result: NodeResult| {
            let link = link.clone();
            let call_id = call.call_id.clone();
            async move {
                let _ = link.call(Request::NodeReply { call_id, result }).await;
            }
        };
        if let Some(why) = self.precheck(&call) {
            self.log_call(&call, NodeLogEvent::Refused, &why, None);
            reply(exec::refused(why)).await;
            return;
        }
        // The screen is one chat's at a time for the mouse and keyboard:
        // another chat's clicks and keys never land between this one's.
        // Looking takes no turn. (Taken in its own statement: the lock is
        // let go before any wait.)
        let input = matches!(&call.action, NodeAction::Desktop { desktop, .. } if desktop.acts());
        let taken = input.then(|| {
            self.desktop
                .lock()
                .begin(&call.session_id, std::time::Instant::now())
        });
        if let Some(Err(left)) = taken {
            let why = format!(
                "another chat is using this computer's screen right now; it is free again about {} s after its last action, or once that chat is done",
                left.as_secs().max(1)
            );
            self.log_call(&call, NodeLogEvent::Refused, &why, None);
            reply(exec::refused(why)).await;
            return;
        }
        // Its turn ends however this ends (refused below, done, stopped).
        let _turn = input.then(|| ScreenTurn {
            node: self.clone(),
            chat: call.session_id.clone(),
        });
        let running = Arc::new(Running {
            action: call.action.clone(),
            bot: call.bot.as_ref().map(|b| (b.id.clone(), b.handle.clone())),
            cancel: CancellationToken::new(),
            why: Mutex::new(None),
        });
        let admitted = {
            let mut calls = self.calls.lock();
            if calls.len() >= super::MAX_CALLS {
                Some(false)
            } else if calls.contains_key(&call.call_id) {
                None
            } else {
                calls.insert(call.call_id.clone(), running.clone());
                Some(true)
            }
        };
        match admitted {
            None => return,
            Some(false) => {
                let why = format!(
                    "{} actions are already running on this machine; wait for one to finish",
                    super::MAX_CALLS
                );
                self.log_call(&call, NodeLogEvent::Refused, &why, None);
                reply(exec::refused(why)).await;
                return;
            }
            Some(true) => {}
        }
        // Narrowed, paused or turned off between the check above and now:
        // checked again now that it is registered (what changes from here
        // on finds it among the running actions and ends it).
        if let Some(why) = self.precheck(&call) {
            self.calls.lock().remove(&call.call_id);
            self.log_call(&call, NodeLogEvent::Refused, &why, None);
            reply(exec::refused(why)).await;
            return;
        }
        // Stopped by the user between the check above and now: the Stop
        // already went through the running actions, so this one must not run.
        let saved = super::StopLatch::load(&self.paths);
        let (stopped, stale) = {
            let c = self.config.read();
            let stopped = c.scope.stopped || saved.stopped || self.unsaved_stop.lock().is_some();
            (
                stopped,
                stopped
                    || call
                        .stops
                        .is_some_and(|s| s != saved.stops.max(c.scope.stops)),
            )
        };
        if stale || super::screen::halted() {
            self.calls.lock().remove(&call.call_id);
            let why = if stopped || super::screen::halted() {
                super::STOPPED_HERE
            } else {
                super::ASKED_BEFORE_STOP
            }
            .to_string();
            self.log_call(&call, NodeLogEvent::Refused, &why, None);
            reply(exec::refused(why)).await;
            return;
        }
        // Said on the Stop bar while it runs, and only once the bar is on
        // screen saying it (nothing runs here unseen).
        let seq = super::stopbar::begin(&call.call_id, &call.chat, &super::doing(&call.action));
        let _shown = Shown(call.call_id.clone());
        if !super::stopbar::shown(seq, Duration::from_secs(3)).await {
            self.calls.lock().remove(&call.call_id);
            let why = "this computer's Stop bar didn't come on screen to show this action, so it didn't run: the user must always be able to see and stop agents there".to_string();
            self.log_call(&call, NodeLogEvent::Refused, &why, None);
            reply(exec::refused(why)).await;
            return;
        }
        self.write_state(|_| {});
        let scope = self.config.read().scope.clone();
        let asked = match &call.action {
            NodeAction::Run { timeout_secs, .. } => timeout_secs.unwrap_or(scope.timeout_secs),
            _ => scope.timeout_secs,
        };
        let timeout = Duration::from_secs(asked.clamp(1, scope.timeout_secs));
        self.log_call(
            &call,
            NodeLogEvent::Started,
            if call.approved {
                "approved in the chat"
            } else {
                "allowed by the chat's mode"
            },
            None,
        );
        // Stored secrets the command got never leave this machine (nor
        // reach its log), whatever it prints.
        let scrub: Arc<Vec<String>> = Arc::new(crate::auth::env_forms(&call.env.0));
        let clean = {
            let scrub = scrub.clone();
            move |t: &str| -> String {
                if scrub.is_empty() {
                    t.to_string()
                } else {
                    crate::tools::redact_known(t, &scrub)
                }
            }
        };
        // Live output goes out in batches, before the reply.
        let (tx, mut rx) = mpsc::unbounded_channel::<(String, &'static str)>();
        let progress: exec::Progress = Arc::new(move |chunk: &str, stream| {
            let _ = tx.send((chunk.to_string(), stream));
        });
        let forward = {
            let link = link.clone();
            let call_id = call.call_id.clone();
            tokio::spawn(async move {
                while let Some((chunk, stream)) = rx.recv().await {
                    let _ = link
                        .call(Request::NodeProgress {
                            call_id: call_id.clone(),
                            chunk,
                            stream: Some(stream.into()),
                        })
                        .await;
                }
            })
        };
        let started = std::time::Instant::now();
        // Desktop control: startup files are kept before anything changes
        // them, and every one that did change is noted (`tessera node restore`).
        let watch = (scope.desktop
            && call.action.changes()
            && !matches!(call.action, NodeAction::Desktop { .. }))
        .then(|| {
            exec::Access::new(&self.paths, &scope, self.home.as_deref())
                .startup()
                .to_vec()
        });
        let snap = match &watch {
            Some(roots) => {
                let (me, roots) = (self.clone(), roots.clone());
                tokio::task::spawn_blocking(move || me.backups.before(&roots))
                    .await
                    .ok()
            }
            None => None,
        };
        let outcome = exec::run(
            &self.paths,
            self.home.as_deref(),
            &scope,
            call.action.clone(),
            timeout,
            running.cancel.clone(),
            Some(progress),
            self.session_env.clone(),
            &call.env,
        )
        .await;
        if let (Some(roots), Some(snap)) = (watch, snap) {
            let me = self.clone();
            let by = match &call.bot {
                Some(b) => format!("{} in “{}”", b.name, call.chat),
                None => format!("“{}”", call.chat),
            };
            let action = call.action.name();
            let by2 = by.clone();
            let changed =
                tokio::task::spawn_blocking(move || me.backups.after(&roots, snap, action, &by2))
                    .await
                    .unwrap_or_default();
            if !changed.is_empty() {
                // Its own line (an action's lines show as one).
                self.log_event(
                    NodeLogEvent::StartupChanged,
                    format!(
                        "{} changed by {by}; a copy was kept to put back (`tessera node restore`)",
                        crate::util::truncate(&changed.join(", "), 300)
                    ),
                );
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), forward).await;
        let ms = Some(started.elapsed().as_millis() as u64);
        let why = running.why.lock().clone();
        let outcome = match outcome {
            Outcome::Finished(mut r) => {
                r.output = clean(&r.output);
                Outcome::Finished(r)
            }
            Outcome::TimedOut(mut r) => {
                r.output = clean(&r.output);
                Outcome::TimedOut(r)
            }
            Outcome::Cancelled(mut r) => {
                r.output = clean(&r.output);
                Outcome::Cancelled(r)
            }
        };
        let result = match outcome {
            Outcome::Finished(r) => {
                let event = if r.refused {
                    NodeLogEvent::Refused
                } else if r.ok {
                    NodeLogEvent::Done
                } else {
                    NodeLogEvent::Failed
                };
                let detail = match (r.refused || !r.ok, r.exit_code) {
                    (_, Some(c)) => format!("exit {c}"),
                    (true, None) => crate::util::one_line(&r.output, 300),
                    (false, None) => "ok".into(),
                };
                self.log_call(&call, event, detail, ms);
                r
            }
            Outcome::TimedOut(r) => {
                self.log_call(
                    &call,
                    NodeLogEvent::TimedOut,
                    format!("time limit {}s", timeout.as_secs()),
                    ms,
                );
                r
            }
            Outcome::Cancelled(mut r) => {
                let why = why.unwrap_or_else(|| "stopped".into());
                self.log_call(&call, NodeLogEvent::Cancelled, &why, ms);
                r.output = format!("{}\nStopped on this machine: {why}.", r.output);
                r
            }
        };
        self.calls.lock().remove(&call.call_id);
        self.write_state(|_| {});
        reply(result).await;
    }
}

enum Greeting {
    Welcomed,
    Revoked,
    Retry,
}
