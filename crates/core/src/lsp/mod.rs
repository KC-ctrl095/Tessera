//! Language servers: the diagnostics an edit introduced, on every edit, and
//! navigation for the `lsp` tool.
//!
//! - **Detection** is by project markers plus an installed binary
//!   (`servers.rs`); Tessera never installs a server.
//! - **Lifecycle**: a server starts on first need (a read warms it, an edit or
//!   `lsp` call needs it), one per (server, project root); it stops after
//!   `idle_minutes`, when it exceeds `max_memory_mb`, or to make room under
//!   `max_servers`. A server that crashes is restarted at most
//!   [`MAX_CRASHES`] times.
//! - **Security**: servers run project code (build scripts, proc macros,
//!   plugins), so they get the same sandbox as `bash` (Tessera's own
//!   directories hidden), the same withheld environment minus anything that
//!   looks like a secret, a lower CPU priority and no core dumps.
//! - **Diagnostics delta**: after `write`/`edit` the document is synced and
//!   the server's diagnostics are collected for a bounded, adaptive time
//!   (`wait_ms` at most). Only errors and warnings that weren't there before
//!   the edit are reported. Whatever arrives later (e.g. `cargo check` on
//!   save) is queued and shown with the session's next tool result, if it
//!   still applies, at most [`LATE_BLOCKS_PER_TURN`] times per turn.

pub mod client;
pub mod diagnostics;
#[cfg(test)]
pub(crate) mod fake;
pub mod rpc;
pub mod servers;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tessera_proto::PermissionMode;
use tokio::sync::oneshot;

pub use client::{LspClient, RpcError, Transport};
pub use diagnostics::{Diagnostic, Severity};
pub use servers::ServerDef;

use crate::config::LspConfig;
use crate::tools::linemap::LineMap;

pub const MAX_CRASHES: u32 = 3;
pub const LATE_BLOCKS_PER_TURN: usize = 3;
/// Lines of diagnostics per report.
pub const CAP_LINES: usize = 10;
const START_TIMEOUT: Duration = Duration::from_secs(45);
/// How long after an edit late diagnostics are still collected.
const LATE_WINDOW: Duration = Duration::from_secs(90);
/// After the first publish, how long to wait for follow-ups.
const SETTLE: Duration = Duration::from_millis(200);
/// A server that failed to start isn't retried sooner than this.
const RETRY_AFTER: Duration = Duration::from_secs(300);
/// Larger files aren't opened just because they were read.
const WARM_MAX: usize = 2 * 1024 * 1024;

/// What a tool knows about the session asking.
#[derive(Clone, Debug)]
pub struct SessionEnv {
    pub session: String,
    pub cwd: PathBuf,
    /// The session's user-turn counter (caps late reports per turn).
    pub turn: u32,
    pub sandbox: Option<(crate::sandbox::Spec, bool)>,
    /// Whether this call may start a server that isn't running.
    pub may_start: bool,
}

pub type Spawner = dyn Fn(&ServerDef, &Path, Option<&(crate::sandbox::Spec, bool)>) -> Result<Transport, String>
    + Send
    + Sync;

type Key = (String, PathBuf);

#[derive(Default)]
struct Slot {
    cell: tokio::sync::OnceCell<Result<Arc<LspClient>, String>>,
}

#[derive(Default)]
struct Health {
    crashes: u32,
    /// Refused until this instant, with the reason.
    failed: Option<(Instant, String)>,
}

struct LateBlock {
    server: String,
    client: Weak<LspClient>,
    items: Vec<(PathBuf, Diagnostic)>,
}

#[derive(Default)]
struct SessionLate {
    blocks: Vec<LateBlock>,
    /// (turn, reports delivered in it)
    delivered: (u32, usize),
}

pub struct LspManager {
    cfg: LspConfig,
    defs: Vec<ServerDef>,
    idle: Mutex<Duration>,
    spawner: Box<Spawner>,
    slots: Mutex<HashMap<Key, Arc<Slot>>>,
    health: Mutex<HashMap<Key, Health>>,
    late: Mutex<HashMap<String, SessionLate>>,
    /// Latest edit check per (session, file); older ones stop reporting.
    watches: Mutex<HashMap<(String, PathBuf), u64>>,
    next_watch: AtomicU64,
    /// (session, server) pairs already told "(diagnostics pending)".
    noted: Mutex<HashSet<(String, String)>>,
    reaper: AtomicBool,
    me: Weak<LspManager>,
}

impl LspManager {
    pub fn new(cfg: LspConfig) -> Arc<Self> {
        Self::with_spawner(cfg, Box::new(spawn_process))
    }

    pub fn with_spawner(cfg: LspConfig, spawner: Box<Spawner>) -> Arc<Self> {
        let defs = servers::definitions(&cfg);
        let idle = Duration::from_secs(cfg.idle_minutes.max(1) * 60);
        Arc::new_cyclic(|me| Self {
            cfg,
            defs,
            idle: Mutex::new(idle),
            spawner,
            slots: Mutex::new(HashMap::new()),
            health: Mutex::new(HashMap::new()),
            late: Mutex::new(HashMap::new()),
            watches: Mutex::new(HashMap::new()),
            next_watch: AtomicU64::new(0),
            noted: Mutex::new(HashSet::new()),
            reaper: AtomicBool::new(false),
            me: me.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_idle(&self, idle: Duration) {
        *self.idle.lock() = idle;
    }

    fn wait(&self) -> Duration {
        Duration::from_millis(self.cfg.wait_ms.clamp(200, 10_000))
    }

    /// Whether a session in `mode` may start servers on its own: not in
    /// `ask` mode (the `lsp` tool asks first there) and not read-only unless
    /// configured.
    pub fn may_start(&self, mode: PermissionMode) -> bool {
        match mode {
            PermissionMode::ReadOnly => self.cfg.in_read_only,
            PermissionMode::Ask => false,
            PermissionMode::Default | PermissionMode::Auto => true,
        }
    }

    /// Servers (one per language) installed and matching the project at
    /// `cwd`. Cheap: a few `stat`s.
    pub fn detect(&self, cwd: &Path) -> Vec<&ServerDef> {
        let mut langs = HashSet::new();
        self.defs
            .iter()
            .filter(|d| {
                !langs.contains(&d.language)
                    && d.matches_project(cwd)
                    && d.binary_for(cwd).is_some()
                    && langs.insert(d.language.clone())
            })
            .collect()
    }

    /// The server for `path` and its project root. A file outside the
    /// session's directory (a dependency's source, say) goes to the
    /// session's own server for that language, which knows it through its
    /// workspace; it never starts a server rooted somewhere else.
    pub fn route(&self, path: &Path, cwd: &Path) -> Option<(ServerDef, PathBuf)> {
        let inside = path.starts_with(cwd);
        self.defs.iter().filter(|d| d.handles(path)).find_map(|d| {
            let root = if inside {
                d.root_for(path, cwd)?
            } else {
                if !d.matches_project(cwd) {
                    return None;
                }
                let ext = path.extension()?.to_string_lossy();
                d.root_for(&cwd.join(format!("_.{ext}")), cwd)?
            };
            d.binary_for(&root)?;
            Some((d.clone(), root))
        })
    }

    /// Servers for workspace-wide questions about `cwd`, with their roots.
    pub fn workspace_servers(&self, cwd: &Path) -> Vec<(ServerDef, PathBuf)> {
        self.detect(cwd)
            .into_iter()
            .filter_map(|d| {
                let probe = cwd.join(format!("_.{}", d.extensions.first()?));
                Some((d.clone(), d.root_for(&probe, cwd)?))
            })
            .collect()
    }

    pub fn running(&self, server: &str, root: &Path) -> Option<Arc<LspClient>> {
        let slot = self
            .slots
            .lock()
            .get(&(server.to_string(), root.to_path_buf()))
            .cloned()?;
        match slot.cell.get() {
            Some(Ok(c)) if c.alive() => Some(c.clone()),
            _ => None,
        }
    }

    /// Running servers whose project overlaps `cwd`.
    pub fn running_for(&self, cwd: &Path) -> Vec<Arc<LspClient>> {
        self.slots
            .lock()
            .values()
            .filter_map(|s| match s.cell.get() {
                Some(Ok(c))
                    if c.alive() && (c.root.starts_with(cwd) || cwd.starts_with(&c.root)) =>
                {
                    Some(c.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The running server for (`def`, `root`), started if `may_start`.
    pub async fn client(
        &self,
        def: &ServerDef,
        root: &Path,
        env: &SessionEnv,
        may_start: bool,
    ) -> Result<Arc<LspClient>, String> {
        let key = (def.name.clone(), root.to_path_buf());
        loop {
            let existing = self.slots.lock().get(&key).cloned();
            let slot = match existing {
                Some(slot) => {
                    match slot.cell.get() {
                        Some(Ok(c)) if c.alive() => {
                            c.touch();
                            return Ok(c.clone());
                        }
                        Some(Ok(c)) => {
                            let crashed = !c.stopping();
                            self.forget(&key, &slot, crashed);
                            continue;
                        }
                        Some(Err(e)) => {
                            let retry = self
                                .health
                                .lock()
                                .get(&key)
                                .and_then(|h| h.failed.as_ref().map(|f| f.0))
                                .is_none_or(|until| Instant::now() >= until);
                            if !retry || !may_start {
                                return Err(e.clone());
                            }
                            self.forget(&key, &slot, false);
                            continue;
                        }
                        None => slot, // someone is starting it
                    }
                }
                None => {
                    if !may_start {
                        return Err(format!("{} is not running", def.name));
                    }
                    if let Some(h) = self.health.lock().get(&key)
                        && let Some((until, why)) = &h.failed
                        && Instant::now() < *until
                    {
                        return Err(why.clone());
                    }
                    self.make_room().await;
                    let slot = Arc::new(Slot::default());
                    let mut slots = self.slots.lock();
                    slots.entry(key.clone()).or_insert(slot).clone()
                }
            };
            let result = slot
                .cell
                .get_or_init(|| self.start(def, root, env))
                .await
                .clone();
            if let Err(e) = &result {
                let mut h = self.health.lock();
                let entry = h.entry(key.clone()).or_default();
                if entry.failed.is_none() {
                    entry.failed = Some((Instant::now() + RETRY_AFTER, e.clone()));
                }
            }
            return result;
        }
    }

    async fn start(
        &self,
        def: &ServerDef,
        root: &Path,
        env: &SessionEnv,
    ) -> Result<Arc<LspClient>, String> {
        let mut def = def.clone();
        let bin = def
            .binary_for(root)
            .ok_or_else(|| format!("{} is not installed", def.name))?;
        def.command[0] = bin.display().to_string();
        let transport = (self.spawner)(&def, root, env.sandbox.as_ref())?;
        let client = LspClient::start(
            &def.name,
            root,
            transport,
            def.init_options.clone(),
            def.settings.clone(),
            START_TIMEOUT,
        )
        .await?;
        tracing::info!("started {} for {}", def.name, root.display());
        self.ensure_reaper();
        Ok(client)
    }

    /// Drop a slot; a crash counts towards the restart limit.
    fn forget(&self, key: &Key, slot: &Arc<Slot>, crashed: bool) {
        {
            let mut slots = self.slots.lock();
            if slots.get(key).is_some_and(|s| Arc::ptr_eq(s, slot)) {
                slots.remove(key);
            }
        }
        if crashed {
            let mut h = self.health.lock();
            let entry = h.entry(key.clone()).or_default();
            entry.crashes += 1;
            tracing::warn!(
                "{} for {} exited unexpectedly ({} times)",
                key.0,
                key.1.display(),
                entry.crashes
            );
            if entry.crashes >= MAX_CRASHES {
                entry.failed = Some((
                    Instant::now() + Duration::from_secs(365 * 24 * 3600),
                    format!(
                        "{} crashed {} times and was not restarted",
                        key.0, entry.crashes
                    ),
                ));
            }
        }
    }

    /// Stop the least recently used server when at `max_servers`.
    async fn make_room(&self) {
        let running: Vec<(Key, Arc<Slot>, Arc<LspClient>)> = self
            .slots
            .lock()
            .iter()
            .filter_map(|(k, s)| match s.cell.get() {
                Some(Ok(c)) if c.alive() => Some((k.clone(), s.clone(), c.clone())),
                _ => None,
            })
            .collect();
        if running.len() < self.cfg.max_servers.max(1) {
            return;
        }
        if let Some((k, s, c)) = running.into_iter().max_by_key(|(_, _, c)| c.idle_for()) {
            self.forget(&k, &s, false);
            c.shutdown().await;
        }
    }

    fn ensure_reaper(&self) {
        if self.reaper.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.me.clone();
        tokio::spawn(async move {
            loop {
                let tick = match me.upgrade() {
                    Some(m) => (*m.idle.lock() / 4)
                        .clamp(Duration::from_millis(50), Duration::from_secs(30)),
                    None => return,
                };
                tokio::time::sleep(tick).await;
                let Some(m) = me.upgrade() else { return };
                m.reap().await;
                // Nothing left to watch: stop waking up until a server starts.
                if m.slots.lock().is_empty() {
                    m.reaper.store(false, Ordering::SeqCst);
                    if m.slots.lock().is_empty() || m.reaper.swap(true, Ordering::SeqCst) {
                        return;
                    }
                }
            }
        });
    }

    /// Stop idle and oversized servers; notice crashed ones.
    pub async fn reap(&self) {
        let idle = *self.idle.lock();
        let slots: Vec<(Key, Arc<Slot>)> = self
            .slots
            .lock()
            .iter()
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect();
        for (key, slot) in slots {
            let Some(Ok(c)) = slot.cell.get().cloned() else {
                continue;
            };
            if !c.alive() {
                self.forget(&key, &slot, !c.stopping());
            } else if c.idle_for() >= idle {
                tracing::info!("stopping idle {} for {}", key.0, key.1.display());
                self.forget(&key, &slot, false);
                c.shutdown().await;
            } else if self.cfg.max_memory_mb > 0
                && let Some(mb) = c.rss_mb().filter(|mb| *mb > self.cfg.max_memory_mb)
            {
                tracing::warn!(
                    "{} uses {mb} MiB (limit {}); stopping it",
                    key.0,
                    self.cfg.max_memory_mb
                );
                self.forget(&key, &slot, true);
                c.kill();
            }
        }
    }

    /// Open `path` in its server in the background, so the server has a
    /// baseline (and is warm) by the time the file is edited.
    pub fn touch(&self, env: &SessionEnv, path: &Path, text: &str) {
        if text.len() > WARM_MAX {
            return;
        }
        let path = crate::tools::canonical(path);
        let Some((def, root)) = self.route(&path, &env.cwd) else {
            return;
        };
        let running = self.running(&def.name, &root);
        if running.is_none() && !env.may_start {
            return;
        }
        let lang = def.language_id(&path);
        if let Some(c) = running {
            let _ = c.sync(&path, text, &lang);
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        let env = env.clone();
        let text = text.to_string();
        tokio::spawn(async move {
            if let Ok(c) = me.client(&def, &root, &env, true).await {
                let _ = c.sync(&path, &text, &lang);
            }
        });
    }

    /// Sync a file the session just wrote and report the errors and
    /// warnings the change introduced (`cross_file`: also new errors in other
    /// files). Returns within the adaptive budget; later findings are queued
    /// for [`take_late`](Self::take_late).
    pub async fn after_write(
        &self,
        env: &SessionEnv,
        path: &Path,
        old: Option<&str>,
        new: &str,
        cross_file: bool,
    ) -> Option<String> {
        self.after_write_checked(env, path, old, new, cross_file)
            .await
            .text
    }

    /// [`after_write`](Self::after_write), saying also whether a server
    /// handles the file and answered for this version in time (so `None`
    /// text means a clean change, not "nobody looked").
    pub async fn after_write_checked(
        &self,
        env: &SessionEnv,
        path: &Path,
        old: Option<&str>,
        new: &str,
        cross_file: bool,
    ) -> AfterWrite {
        let path = crate::tools::canonical(path);
        let Some((def, root)) = self.route(&path, &env.cwd) else {
            return AfterWrite::default();
        };
        let running = self.running(&def.name, &root);
        if running.is_none() && !env.may_start {
            return AfterWrite::default();
        }
        let mut out = AfterWrite {
            routed: true,
            ..Default::default()
        };
        let budget = running
            .as_ref()
            .map(|c| c.budget(self.wait()))
            .unwrap_or(self.wait());
        let deadline = Instant::now() + budget;
        let watch = self.next_watch.fetch_add(1, Ordering::SeqCst) + 1;
        self.watches
            .lock()
            .insert((env.session.clone(), path.clone()), watch);
        let (tx, mut rx) = oneshot::channel();
        let handoff = Arc::new(Mutex::new(Handoff {
            late: false,
            tx: Some(tx),
        }));
        let Some(mgr) = self.me.upgrade() else {
            return out;
        };
        let check = Check {
            mgr,
            env: env.clone(),
            def: def.clone(),
            root,
            path,
            old: old.map(str::to_string),
            new: new.to_string(),
            cross_file,
            watch,
            deadline,
            handoff: handoff.clone(),
        };
        tokio::spawn(check.run());
        let answer = match tokio::time::timeout_at(deadline.into(), &mut rx).await {
            Ok(Ok(r)) => Some(r),
            _ => {
                handoff.lock().late = true;
                rx.try_recv().ok()
            }
        };
        if let Some((text, answered)) = answer {
            out.text = text;
            out.answered = answered;
            return out;
        }
        out.text = self
            .noted
            .lock()
            .insert((env.session.clone(), def.name.clone()))
            .then(|| format!("LSP {}: (diagnostics pending)", def.name));
        out
    }

    fn is_current(&self, session: &str, path: &Path, watch: u64) -> bool {
        self.watches
            .lock()
            .get(&(session.to_string(), path.to_path_buf()))
            == Some(&watch)
    }

    fn queue_late(&self, session: &str, block: LateBlock) {
        if block.items.is_empty() {
            return;
        }
        self.late
            .lock()
            .entry(session.to_string())
            .or_default()
            .blocks
            .push(block);
    }

    /// Late diagnostics for the session that still apply, rendered; at most
    /// [`LATE_BLOCKS_PER_TURN`] reports per turn.
    pub fn take_late(&self, session: &str, turn: u32, cwd: &Path) -> Option<String> {
        let blocks = {
            let mut late = self.late.lock();
            let s = late.get_mut(session)?;
            if s.blocks.is_empty() {
                return None;
            }
            if s.delivered.0 != turn {
                s.delivered = (turn, 0);
            }
            if s.delivered.1 >= LATE_BLOCKS_PER_TURN {
                return None;
            }
            std::mem::take(&mut s.blocks)
        };
        let mut by_server: Vec<(String, Vec<(PathBuf, Diagnostic)>)> = Vec::new();
        for b in blocks {
            let Some(client) = b.client.upgrade().filter(|c| c.alive()) else {
                continue;
            };
            // Only what the server still reports: a later edit may have fixed it.
            let current: Vec<(PathBuf, Diagnostic)> = b
                .items
                .into_iter()
                .filter(|(p, d)| {
                    client.published(p).is_some_and(|pubd| {
                        pubd.items.iter().any(|x| {
                            x.line == d.line && x.message == d.message && x.severity == d.severity
                        })
                    })
                })
                .collect();
            match by_server.iter_mut().find(|(s, _)| *s == b.server) {
                Some((_, items)) => items.extend(current),
                None => by_server.push((b.server, current)),
            }
        }
        let mut out = Vec::new();
        for (server, items) in by_server {
            let mut groups: Vec<(String, Vec<Diagnostic>)> = Vec::new();
            for (p, d) in items {
                let display = display(&p, cwd);
                match groups.iter_mut().find(|(g, _)| *g == display) {
                    Some((_, v)) => {
                        if !v.contains(&d) {
                            v.push(d)
                        }
                    }
                    None => groups.push((display, vec![d])),
                }
            }
            if let Some(text) = diagnostics::render_block(&server, true, &groups, 0, CAP_LINES) {
                out.push(text);
            }
        }
        if out.is_empty() {
            return None;
        }
        if let Some(s) = self.late.lock().get_mut(session) {
            s.delivered.1 += 1;
        }
        Some(out.join("\n"))
    }

    /// Forget everything about a session (it was deleted).
    pub fn forget_session(&self, session: &str) {
        self.late.lock().remove(session);
        self.watches.lock().retain(|(s, _), _| s != session);
        self.noted.lock().retain(|(s, _)| s != session);
    }
}

fn display(p: &Path, cwd: &Path) -> String {
    p.strip_prefix(cwd)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}

struct Handoff {
    /// The tool stopped waiting: findings go to the late queue.
    late: bool,
    /// The report, and whether the server answered for this version.
    tx: Option<oneshot::Sender<(Option<String>, bool)>>,
}

/// What [`LspManager::after_write_checked`] found.
#[derive(Debug, Default)]
pub struct AfterWrite {
    /// New problems (or "(diagnostics pending)"), as shown to the model.
    pub text: Option<String>,
    /// A server handles this file.
    pub routed: bool,
    /// It answered for this version within the budget.
    pub answered: bool,
}

impl AfterWrite {
    /// The server answered and the change brought no new error.
    pub fn clean(&self) -> bool {
        self.answered
            && !self
                .text
                .as_deref()
                .is_some_and(|t| t.lines().next().is_some_and(|l| l.contains("new error")))
    }
}

/// One post-edit diagnostics check, run in the background so an edit never
/// waits longer than its budget.
struct Check {
    mgr: Arc<LspManager>,
    env: SessionEnv,
    def: ServerDef,
    root: PathBuf,
    path: PathBuf,
    old: Option<String>,
    new: String,
    cross_file: bool,
    watch: u64,
    deadline: Instant,
    handoff: Arc<Mutex<Handoff>>,
}

struct Findings {
    file: Vec<Diagnostic>,
    fixed: usize,
    others: Vec<(PathBuf, Vec<Diagnostic>)>,
}

impl Check {
    /// Hand the early result to the waiting tool; false if it stopped waiting.
    fn deliver(&self, text: Option<String>) -> bool {
        self.deliver_as(text, false)
    }

    /// `answered`: the server published diagnostics for this version.
    fn deliver_as(&self, text: Option<String>, answered: bool) -> bool {
        let mut h = self.handoff.lock();
        if h.late {
            return false;
        }
        if let Some(tx) = h.tx.take() {
            let _ = tx.send((text, answered));
        }
        true
    }

    async fn run(self) {
        let client = match self
            .mgr
            .client(&self.def, &self.root, &self.env, self.env.may_start)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("no diagnostics for {}: {e}", self.path.display());
                self.deliver(None);
                return;
            }
        };
        let lang = self.def.language_id(&self.path);
        let baseline = self.baseline(&client, &lang).await;
        let before_all = if self.cross_file && baseline.is_some() {
            client.all_published()
        } else {
            HashMap::new()
        };
        let Ok((version, gen0)) = client.sync(&self.path, &self.new, &lang) else {
            self.deliver(None);
            return;
        };
        client.save(&self.path, &self.new);
        let t0 = Instant::now();
        let map = LineMap::new(self.old.as_deref().unwrap_or(""), &self.new);
        let mut reported: HashSet<String> = HashSet::new();
        if client
            .wait_publish(&self.path, gen0, Some(version), self.deadline)
            .await
            .is_some()
        {
            client.record_latency(t0.elapsed());
            let settle = (Instant::now() + SETTLE).min(self.deadline);
            let mut g = client.publish_gen();
            while client.wait_any(g, settle).await {
                g = client.publish_gen();
            }
            let f = self.findings(&client, baseline.as_deref(), &map, &before_all, gen0);
            let groups = self.groups(&f);
            for (d, items) in &groups {
                for i in items {
                    reported.insert(format!("{d}\u{0}{}", i.render(d)));
                }
            }
            let text =
                diagnostics::render_block(&self.def.name, false, &groups, f.fixed, CAP_LINES);
            if !self.deliver_as(text, true) {
                // The tool stopped waiting a moment ago: report it late.
                self.queue(&client, &f, &mut HashSet::new());
            }
        } else if !client.alive() {
            self.deliver(None);
            return;
        }
        // Keep listening for a while: servers publish more later (rust-analyzer
        // runs `cargo check` on save), and a slow first answer lands here.
        let until = t0 + LATE_WINDOW;
        let mut g = client.publish_gen();
        while self
            .mgr
            .is_current(&self.env.session, &self.path, self.watch)
        {
            if !client.wait_any(g, until).await {
                break;
            }
            g = client.publish_gen();
            let Some(p) = client.published(&self.path) else {
                continue;
            };
            if p.seq <= gen0 || p.version.is_some_and(|v| v < version) {
                continue;
            }
            // Let a burst of publishes finish before comparing.
            let settle = (Instant::now() + SETTLE).min(until);
            while client.wait_any(g, settle).await {
                g = client.publish_gen();
            }
            if !self
                .mgr
                .is_current(&self.env.session, &self.path, self.watch)
            {
                break;
            }
            let f = self.findings(&client, baseline.as_deref(), &map, &before_all, gen0);
            self.queue(&client, &f, &mut reported);
        }
    }

    /// What the server reports for the text before the edit, if it can say
    /// within part of the budget.
    async fn baseline(&self, client: &LspClient, lang: &str) -> Option<Vec<Diagnostic>> {
        let old = self.old.as_deref()?;
        let now = Instant::now();
        let until = now + self.deadline.saturating_duration_since(now).mul_f32(0.4);
        let (version, after) = match client.doc(&self.path) {
            Some(doc) if &*doc.text == old => {
                if let Some(p) = client
                    .published(&self.path)
                    .filter(|p| p.seq > doc.synced_gen)
                {
                    return Some(p.items.to_vec());
                }
                (doc.version, doc.synced_gen)
            }
            _ => client.sync(&self.path, old, lang).ok()?,
        };
        client
            .wait_publish(&self.path, after, Some(version), until)
            .await
            .map(|p| p.items.to_vec())
    }

    fn findings(
        &self,
        client: &LspClient,
        baseline: Option<&[Diagnostic]>,
        map: &LineMap,
        before_all: &HashMap<PathBuf, client::Published>,
        gen0: u64,
    ) -> Findings {
        let after = client
            .published(&self.path)
            .map(|p| p.items.to_vec())
            .unwrap_or_default();
        let d = diagnostics::delta(baseline, &after, Some(map));
        let mut others = Vec::new();
        // Other files only when the server was warm (a baseline exists):
        // a cold server's first reports are all "new".
        if self.cross_file && baseline.is_some() {
            for (p, published) in client.all_published() {
                if p == self.path || published.seq <= gen0 {
                    continue;
                }
                let before = before_all
                    .get(&p)
                    .map(|b| b.items.to_vec())
                    .unwrap_or_default();
                let errors: Vec<Diagnostic> =
                    diagnostics::delta(Some(&before), &published.items, None)
                        .new
                        .into_iter()
                        .filter(|x| x.severity == Severity::Error)
                        .collect();
                if !errors.is_empty() {
                    others.push((p, errors));
                }
            }
            others.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Findings {
            file: d.new,
            fixed: d.fixed,
            others,
        }
    }

    fn groups(&self, f: &Findings) -> Vec<(String, Vec<Diagnostic>)> {
        let mut groups = vec![(display(&self.path, &self.env.cwd), f.file.clone())];
        for (p, items) in &f.others {
            groups.push((display(p, &self.env.cwd), items.clone()));
        }
        groups.retain(|(_, v)| !v.is_empty());
        groups
    }

    fn queue(&self, client: &Arc<LspClient>, f: &Findings, reported: &mut HashSet<String>) {
        if !self
            .mgr
            .is_current(&self.env.session, &self.path, self.watch)
        {
            return;
        }
        let mut items = Vec::new();
        let all = std::iter::once((self.path.clone(), &f.file))
            .chain(f.others.iter().map(|(p, v)| (p.clone(), v)));
        for (p, diags) in all {
            let d = display(&p, &self.env.cwd);
            for x in diags {
                if reported.insert(format!("{d}\u{0}{}", x.render(&d))) {
                    items.push((p.clone(), x.clone()));
                }
            }
        }
        self.mgr.queue_late(
            &self.env.session,
            LateBlock {
                server: self.def.name.clone(),
                client: Arc::downgrade(client),
                items,
            },
        );
    }
}

/// Environment variables whose names suggest a credential. Servers get the
/// same environment as `bash` minus these: they run project code nobody
/// reviewed (a `build.rs` could otherwise read `GITHUB_TOKEN`).
fn secret_like(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n != "SSH_AUTH_SOCK"
        && [
            "TOKEN",
            "SECRET",
            "PASSW",
            "API_KEY",
            "APIKEY",
            "PRIVATE_KEY",
            "CREDENTIAL",
            "AUTH",
        ]
        .iter()
        .any(|w| n.contains(w))
}

/// Start a server process in the `bash` sandbox.
pub fn spawn_process(
    def: &ServerDef,
    root: &Path,
    sandbox: Option<&(crate::sandbox::Spec, bool)>,
) -> Result<Transport, String> {
    use std::process::Stdio;
    let prepared = crate::sandbox::prepare(sandbox, root)
        .map_err(|e| format!("{} was not started: {e}", def.name))?;
    let (program, args) = def
        .command
        .split_first()
        .ok_or_else(|| format!("{} has no command", def.name))?;
    let args = args.iter().map(Into::into).collect();
    let (program, args) = match &prepared {
        Some(p) => p.argv(program.into(), args),
        None => (program.into(), args),
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::tools::bash::withhold_env(&mut cmd);
    for (name, _) in std::env::vars_os() {
        if secret_like(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
    cmd.env("TESSERA", "1");
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            // Set the session before private anchors are born, so they may join
            // the requested program's group without a session mismatch.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if let Some(s) = &prepared {
        s.protect_environment(cmd.as_std_mut())
            .map_err(|e| format!("could not prepare {}: {e}", def.name))?;
    }
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            if let Some(s) = &prepared {
                s.enter()?;
            }
            // Best effort: yield the CPU to the user, never dump core.
            libc::setpriority(libc::PRIO_PROCESS as _, 0, 10);
            let none = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &none);
            Ok(())
        });
    }
    #[cfg(not(unix))]
    let _ = prepared;
    let mut child = crate::platform::spawn(&mut cmd, crate::platform::Start::Agent)
        .map_err(|e| format!("could not start {}: {e}", def.name))?;
    #[cfg(target_os = "linux")]
    let tree = child.id().map(crate::tools::bash::ChildTree::capture);
    #[cfg(windows)]
    let windows_job = child.id().and_then(crate::platform::windows::take_job);
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    Ok(Transport {
        reader: Box::new(stdout),
        writer: Box::new(stdin),
        process: Some(child),
        #[cfg(windows)]
        windows_job,
        #[cfg(target_os = "linux")]
        tree,
    })
}

#[cfg(test)]
mod tests;
