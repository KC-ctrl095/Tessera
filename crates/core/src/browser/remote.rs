//! A machine's browser ("Web views here"), driven from the server: the
//! same CDP client as for the server's own browser, fed through the
//! machine's end-to-end encrypted connection instead of a pipe.
//!
//! The machine runs the browser and passes on only the commands it allows
//! (`crate::node::browser`); everything here is the server's side of that
//! tunnel. Messages go out in batches, in order: one `node_browser` event
//! per batch of commands, and the machine answers with batches of replies and
//! events (`Engine::node_request`). `run` numbers each start, so news from an
//! earlier one is never taken for the current one.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use tessera_proto::{NodeBrowserDown, NodeBrowserUp};
use tokio::sync::{mpsc, oneshot};

use super::cdp::{Cdp, CdpEvent};

/// Bytes of commands sent to a machine in one message at most.
const BATCH_BYTES: usize = 1 << 20;
/// How long a machine may take to start its browser.
const START_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a machine may take to open (or close) a window on its screen.
const SCREEN_TIMEOUT: Duration = Duration::from_secs(40);

/// The way to one machine (the engine's side: its connection).
pub trait Remote: Send + Sync {
    /// The machine's id.
    fn machine(&self) -> String;
    /// Its name, for messages ("Desktop").
    fn name(&self) -> String;
    /// Send these to the machine's browser, in order. `Err`: it isn't
    /// connected (in plain words).
    fn send(&self, msgs: Vec<NodeBrowserDown>) -> BoxFuture<'static, Result<(), String>>;
}

/// Why a machine stops its browser when it uses too much memory (its
/// words since the first version: "it used 4.2 GB of memory, more than the
/// 4.0 GB allowed here").
pub(crate) fn memory_stop(why: &str) -> bool {
    why.contains("of memory, more than the")
}

/// The CDP event a machine sends (among its browser's) when its browser
/// nears its memory limit: the server closes a page nobody needs. Older
/// servers ignore it, as any event they don't know.
pub(crate) const MEMORY_HIGH: &str = "Tessera.memoryHigh";

/// What a machine said when its browser started.
#[derive(Debug, Clone)]
pub(crate) struct Started {
    pub product: String,
    pub speed_key: String,
    pub max_tabs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupKind {
    Unexpected,
    Refused,
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct StartupFailure {
    pub kind: StartupKind,
    pub message: String,
}
impl StartupFailure {
    pub(crate) fn unexpected(message: String) -> Self {
        Self {
            kind: StartupKind::Unexpected,
            message,
        }
    }
    fn new(kind: StartupKind, message: String) -> Self {
        Self { kind, message }
    }
}

struct Current {
    run: u64,
    cdp: Arc<Cdp>,
    started: Option<oneshot::Sender<Result<Started, StartupFailure>>>,
}

/// A machine as a place web views run.
pub(crate) struct RemoteHost {
    pub link: Arc<dyn Remote>,
    runs: AtomicU64,
    current: Mutex<Option<Current>>,
    /// Why the browser there last stopped without being asked to, and
    /// whether it may be started again at once (it crashed).
    pub why: Mutex<Option<String>>,
    pub crashed: std::sync::atomic::AtomicBool,
    pub startup_kind: Mutex<Option<StartupKind>>,
    /// Tabs the machine keeps open at most (0: not known yet).
    pub max_tabs: AtomicUsize,
    /// Waiting for the machine's answer about the window on its screen.
    screen: Mutex<Option<oneshot::Sender<Result<bool, String>>>>,
    /// The user closed that window there (nobody asked): the pages being
    /// watched can open again here.
    pub screen_closed: std::sync::atomic::AtomicBool,
}

/// One start of a machine's browser.
pub(crate) struct Run {
    run: u64,
    pub cdp: Arc<Cdp>,
    host: Arc<RemoteHost>,
}

impl RemoteHost {
    pub fn new(link: Arc<dyn Remote>) -> Arc<Self> {
        Arc::new(Self {
            link,
            runs: AtomicU64::new(0),
            current: Mutex::new(None),
            why: Mutex::new(None),
            crashed: std::sync::atomic::AtomicBool::new(false),
            startup_kind: Mutex::new(None),
            max_tabs: AtomicUsize::new(0),
            screen: Mutex::new(None),
            screen_closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Show `url` in an ordinary browser window on the machine's screen
    /// (`None`: close that window): whether it is open now, or why not.
    pub async fn screen(&self, url: Option<String>) -> Result<bool, String> {
        let name = self.link.name();
        let (tx, rx) = oneshot::channel();
        if let Some(old) = self.screen.lock().replace(tx) {
            let _ = old.send(Err("asked again".into()));
        }
        self.link
            .send(vec![NodeBrowserDown::Screen { url }])
            .await?;
        match tokio::time::timeout(SCREEN_TIMEOUT, rx).await {
            Ok(Ok(r)) => r.map_err(|e| format!("{name}: {e}")),
            Ok(Err(_)) => Err(format!("{name} went offline")),
            Err(_) => {
                self.screen.lock().take();
                Err(format!(
                    "{name} didn't answer (it may need a newer version of Tessera)"
                ))
            }
        }
    }

    /// Start the machine's browser (`scale`: one render scale for every
    /// tab) and connect to it.
    pub async fn launch(
        self: &Arc<Self>,
        scale: Option<f64>,
    ) -> Result<(Run, mpsc::UnboundedReceiver<CdpEvent>, Started), StartupFailure> {
        let run = self.runs.fetch_add(1, Ordering::Relaxed) + 1;
        let (cdp, mut out, events) = Cdp::channel();
        let (tx, rx) = oneshot::channel();
        let old = self.current.lock().replace(Current {
            run,
            cdp: cdp.clone(),
            started: Some(tx),
        });
        let running = Run {
            run,
            cdp: cdp.clone(),
            host: self.clone(),
        };
        if let Some(o) = old {
            o.cdp.close();
        }
        *self.why.lock() = None;
        *self.startup_kind.lock() = None;
        let name = self.link.name();
        if let Err(e) = self
            .link
            .send(vec![NodeBrowserDown::Start { run, scale }])
            .await
        {
            self.end(run);
            return Err(StartupFailure::new(StartupKind::Cancelled, e));
        }
        // Commands go out in order, as many as are waiting in one message.
        let link = self.link.clone();
        let writer_cdp = cdp.clone();
        tokio::spawn(async move {
            loop {
                let first = tokio::select! {
                    m = out.recv() => m,
                    _ = writer_cdp.wait_closed() => None,
                };
                let Some(first) = first else { break };
                let mut bytes = first.len();
                let mut messages = vec![String::from_utf8_lossy(&first).into_owned()];
                while bytes < BATCH_BYTES {
                    match out.try_recv() {
                        Ok(m) => {
                            bytes += m.len();
                            messages.push(String::from_utf8_lossy(&m).into_owned());
                        }
                        Err(_) => break,
                    }
                }
                if link
                    .send(vec![NodeBrowserDown::Cdp { run, messages }])
                    .await
                    .is_err()
                {
                    writer_cdp.close();
                    break;
                }
            }
        });
        let started = match tokio::time::timeout(START_TIMEOUT, rx).await {
            Ok(Ok(Ok(s))) => s,
            Ok(Ok(Err(e))) => {
                self.end(run);
                return Err(StartupFailure::new(
                    e.kind,
                    format!("{name}: {}", e.message),
                ));
            }
            Ok(Err(_)) => {
                self.end(run);
                return Err(StartupFailure::new(
                    self.startup_kind.lock().unwrap_or(StartupKind::Cancelled),
                    self.why
                        .lock()
                        .clone()
                        .unwrap_or_else(|| format!("{name} went offline")),
                ));
            }
            Err(_) => {
                self.stop_run(run).await;
                return Err(StartupFailure::unexpected(format!(
                    "the browser on {name} didn't start in time"
                )));
            }
        };
        self.max_tabs.store(started.max_tabs, Ordering::Relaxed);
        Ok((running, events, started))
    }

    /// Forget run `run` (its client closes).
    fn end(&self, run: u64) {
        let mut cur = self.current.lock();
        if cur.as_ref().is_some_and(|c| c.run == run)
            && let Some(c) = cur.take()
        {
            c.cdp.close();
        }
    }

    async fn stop_run(&self, run: u64) {
        let _ = self.link.send(vec![NodeBrowserDown::Stop { run }]).await;
        self.end(run);
    }

    /// News from the machine's browser.
    pub fn feed(&self, msgs: Vec<NodeBrowserUp>) {
        for m in msgs {
            match m {
                NodeBrowserUp::Cdp { run, messages } => {
                    let cdp = {
                        let cur = self.current.lock();
                        match cur.as_ref() {
                            Some(c) if c.run == run => c.cdp.clone(),
                            _ => continue,
                        }
                    };
                    for raw in messages {
                        cdp.feed(raw.as_bytes());
                    }
                }
                NodeBrowserUp::Started {
                    run,
                    product,
                    speed_key,
                    max_tabs,
                } => {
                    let mut cur = self.current.lock();
                    if let Some(tx) = cur
                        .as_mut()
                        .filter(|c| c.run == run)
                        .and_then(|c| c.started.take())
                    {
                        let _ = tx.send(Ok(Started {
                            product: crate::util::one_line(&product, 80),
                            speed_key: crate::util::one_line(&speed_key, 300),
                            max_tabs: (max_tabs as usize).clamp(1, 64),
                        }));
                    }
                }
                NodeBrowserUp::Failed { run, error } => {
                    let tx = {
                        let mut cur = self.current.lock();
                        cur.as_mut()
                            .filter(|c| c.run == run)
                            .and_then(|c| c.started.take())
                    };
                    if let Some(tx) = tx {
                        let _ = tx.send(Err(StartupFailure::unexpected(crate::util::one_line(
                            &crate::util::visible_controls(&error),
                            400,
                        ))));
                    }
                    self.end(run);
                }
                NodeBrowserUp::Screen { shown, error } => {
                    let waiting = self.screen.lock().take();
                    match waiting {
                        Some(tx) => {
                            let _ = tx.send(match error {
                                Some(e) => Err(crate::util::one_line(
                                    &crate::util::visible_controls(&e),
                                    300,
                                )),
                                None => Ok(shown),
                            });
                        }
                        None if !shown => self.screen_closed.store(true, Ordering::Relaxed),
                        None => {}
                    }
                }
                NodeBrowserUp::Stopped {
                    run,
                    reason,
                    crashed,
                } => {
                    let mut cur = self.current.lock();
                    if let Some(c) = cur.as_mut().filter(|c| c.run == run) {
                        let kind = if crashed || reason.as_deref().is_some_and(memory_stop) {
                            StartupKind::Unexpected
                        } else {
                            StartupKind::Refused
                        };
                        self.crashed.store(crashed, Ordering::Relaxed);
                        *self.startup_kind.lock() = Some(kind);
                        let why = reason.map(|r| {
                            crate::util::one_line(&crate::util::visible_controls(&r), 300)
                        });
                        *self.why.lock() = why.clone();
                        if let Some(tx) = c.started.take() {
                            let _ = tx.send(Err(StartupFailure::new(
                                kind,
                                why.unwrap_or_else(|| "the browser was stopped".into()),
                            )));
                        }
                        drop(cur);
                        self.end(run);
                    }
                }
            }
        }
    }

    /// The machine went offline, was paused or turned web views off (or
    /// connected again: `reopen`, pages being watched open again): its
    /// browser is gone as far as the server is concerned.
    pub fn gone(&self, why: &str, reopen: bool) {
        let cur = self.current.lock().take();
        if let Some(c) = cur {
            *self.why.lock() = Some(why.to_string());
            *self.startup_kind.lock() = Some(StartupKind::Cancelled);
            if let Some(tx) = c.started {
                let _ = tx.send(Err(StartupFailure::new(StartupKind::Cancelled, why.into())));
            }
            self.crashed.store(reopen, Ordering::Relaxed);
            c.cdp.close();
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.kill();
    }
}

impl Run {
    /// Ask the machine to stop the browser (the client closes at once).
    pub async fn stop(mut self) {
        self.host.stop_run(self.run).await;
        self.run = 0;
    }

    pub fn kill(&mut self) {
        let host = self.host.clone();
        let run = std::mem::replace(&mut self.run, 0);
        if run == 0 || !host.current.lock().as_ref().is_some_and(|c| c.run == run) {
            return;
        }
        host.end(run);
        tokio::spawn(async move {
            let _ = host.link.send(vec![NodeBrowserDown::Stop { run }]).await;
        });
    }
}
