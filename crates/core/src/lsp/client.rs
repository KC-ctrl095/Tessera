//! One language-server connection: requests, document sync and the
//! diagnostics it publishes.
//!
//! Writes go through a queue drained by a writer task, so the reader never
//! waits on the server's stdin (a server blocked writing a big response while
//! we block writing a big document would otherwise deadlock).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, oneshot};

use super::diagnostics::Diagnostic;
use super::rpc::{self, FrameError, Framer};

pub type Reader = Box<dyn AsyncRead + Send + Unpin>;
pub type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// Documents kept open per server; the least recently used is closed.
const MAX_OPEN: usize = 64;

/// A connection: the server's stdout and stdin, and its process if it is one.
pub struct Transport {
    pub reader: Reader,
    pub writer: Writer,
    pub process: Option<tokio::process::Child>,
    #[cfg(windows)]
    pub windows_job: Option<crate::platform::windows::TreeJob>,
    #[cfg(target_os = "linux")]
    pub(crate) tree: Option<crate::tools::bash::ChildTree>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub const CONTENT_MODIFIED: i64 = -32801;
    pub const CLOSED: i64 = -1;
    pub const TIMEOUT: i64 = -2;

    fn closed(server: &str) -> Self {
        Self {
            code: Self::CLOSED,
            message: format!("{server} is not running"),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Diagnostics a server last published for one file.
#[derive(Debug, Clone)]
pub struct Published {
    pub version: Option<i64>,
    pub items: Arc<Vec<Diagnostic>>,
    /// Position in the order of all publishes by this server.
    pub seq: u64,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub version: i64,
    pub text: Arc<str>,
    /// Publish counter when this text was sent: anything published later is
    /// about this text (or newer).
    pub synced_gen: u64,
    used: Instant,
}

struct Shared {
    server: String,
    root_uri: String,
    root_name: String,
    settings: Value,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>,
    diags: Mutex<HashMap<PathBuf, Published>>,
    seq: AtomicU64,
    /// Woken on every publish, progress change and on exit.
    changed: Notify,
    alive: AtomicBool,
    progress: Mutex<HashSet<String>>,
    /// rust-analyzer's `experimental/serverStatus`.
    quiescent: AtomicBool,
    stderr_tail: Mutex<String>,
}

pub struct LspClient {
    pub server: String,
    pub root: PathBuf,
    outbox: mpsc::UnboundedSender<Vec<u8>>,
    shared: Arc<Shared>,
    next_id: AtomicI64,
    /// Document versions, shared by all documents so a document closed and
    /// reopened never goes back to a lower version.
    next_version: AtomicI64,
    utf8: AtomicBool,
    caps: Mutex<Value>,
    docs: Mutex<HashMap<PathBuf, Doc>>,
    process: Mutex<Option<tokio::process::Child>>,
    #[cfg(windows)]
    windows_job: Mutex<Option<crate::platform::windows::TreeJob>>,
    #[cfg(target_os = "linux")]
    tree: Mutex<Option<crate::tools::bash::ChildTree>>,
    last_used: Mutex<Instant>,
    /// Smoothed time from a change to its first publish, in ms.
    latency: Mutex<Option<f64>>,
    stopping: AtomicBool,
}

impl LspClient {
    /// Connect and run the `initialize` handshake.
    pub async fn start(
        server: &str,
        root: &Path,
        transport: Transport,
        init_options: Value,
        settings: Value,
        timeout: Duration,
    ) -> Result<Arc<Self>, String> {
        let Transport {
            reader,
            writer,
            mut process,
            #[cfg(windows)]
            windows_job,
            #[cfg(target_os = "linux")]
            tree,
        } = transport;
        let root_uri = uri(root).map_err(|e| e.message)?;
        let shared = Arc::new(Shared {
            server: server.to_string(),
            root_uri: root_uri.clone(),
            root_name: root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".into()),
            settings,
            pending: Mutex::new(HashMap::new()),
            diags: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            changed: Notify::new(),
            alive: AtomicBool::new(true),
            progress: Mutex::new(HashSet::new()),
            quiescent: AtomicBool::new(true),
            stderr_tail: Mutex::new(String::new()),
        });
        if let Some(stderr) = process.as_mut().and_then(|c| c.stderr.take()) {
            let shared = shared.clone();
            tokio::spawn(async move {
                let mut stderr = stderr;
                let mut buf = vec![0u8; 8192];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&buf[..n]);
                    tracing::debug!("lsp[{}] {}", shared.server, text.trim_end());
                    let mut tail = shared.stderr_tail.lock();
                    tail.push_str(&text);
                    if tail.len() > 4096 {
                        let cut = crate::util::ceil_char(&tail, tail.len() - 2048);
                        *tail = tail[cut..].to_string();
                    }
                }
            });
        }
        let (outbox, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_loop(writer, rx));
        tokio::spawn(read_loop(reader, shared.clone(), outbox.clone()));
        let client = Arc::new(Self {
            server: server.to_string(),
            root: root.to_path_buf(),
            outbox,
            shared,
            next_id: AtomicI64::new(1),
            next_version: AtomicI64::new(1),
            utf8: AtomicBool::new(false),
            caps: Mutex::new(Value::Null),
            docs: Mutex::new(HashMap::new()),
            process: Mutex::new(process),
            #[cfg(windows)]
            windows_job: Mutex::new(windows_job),
            #[cfg(target_os = "linux")]
            tree: Mutex::new(tree),
            last_used: Mutex::new(Instant::now()),
            latency: Mutex::new(None),
            stopping: AtomicBool::new(false),
        });
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "tessera", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": root_uri,
            "rootPath": root.display().to_string(),
            "workspaceFolders": [{"uri": client.shared.root_uri, "name": client.shared.root_name}],
            "initializationOptions": init_options,
            "capabilities": {
                "general": {"positionEncodings": ["utf-8", "utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": true, "dynamicRegistration": false},
                    "publishDiagnostics": {"versionSupport": true, "relatedInformation": false},
                    "hover": {"contentFormat": ["plaintext", "markdown"]},
                    "definition": {"linkSupport": true},
                    "references": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "rename": {"prepareSupport": false}
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "symbol": {},
                    "applyEdit": false,
                    "workspaceEdit": {"documentChanges": true},
                    "didChangeWatchedFiles": {"dynamicRegistration": false}
                },
                "window": {"workDoneProgress": true},
                "experimental": {"serverStatusNotification": true}
            }
        });
        let result = match client.request("initialize", params, timeout).await {
            Ok(r) => r,
            Err(e) => {
                let tail = client.stderr_tail();
                client.kill();
                return Err(if tail.is_empty() {
                    format!("{server} did not initialize: {e}")
                } else {
                    format!(
                        "{server} did not initialize: {e} ({})",
                        crate::util::one_line(&tail, 300)
                    )
                });
            }
        };
        let caps = result.get("capabilities").cloned().unwrap_or(Value::Null);
        client.utf8.store(
            caps.get("positionEncoding").and_then(Value::as_str) == Some("utf-8"),
            Ordering::Relaxed,
        );
        *client.caps.lock() = caps;
        client.notify("initialized", json!({}));
        Ok(client)
    }

    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }

    /// Stopped by us (idle, limits, shutdown) rather than crashed.
    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    /// Still loading or checking the project.
    pub fn busy(&self) -> bool {
        !self.shared.progress.lock().is_empty() || !self.shared.quiescent.load(Ordering::Relaxed)
    }

    pub fn utf8(&self) -> bool {
        self.utf8.load(Ordering::Relaxed)
    }

    pub fn stderr_tail(&self) -> String {
        self.shared.stderr_tail.lock().clone()
    }

    pub fn touch(&self) {
        *self.last_used.lock() = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        self.last_used.lock().elapsed()
    }

    /// Whether the server offers `provider` (e.g. `renameProvider`).
    pub fn supports(&self, provider: &str) -> bool {
        match self.caps.lock().get(provider) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(_) => true,
        }
    }

    fn send(&self, msg: &Value) {
        let _ = self.outbox.send(rpc::encode(msg));
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        if !self.alive() {
            return Err(RpcError::closed(&self.server));
        }
        self.touch();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().insert(id, tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(RpcError::closed(&self.server)),
            Err(_) => {
                self.shared.pending.lock().remove(&id);
                self.notify("$/cancelRequest", json!({"id": id}));
                Err(RpcError {
                    code: RpcError::TIMEOUT,
                    message: format!(
                        "{} took longer than {}s to answer `{method}`",
                        self.server,
                        timeout.as_secs()
                    ),
                })
            }
        }
    }

    /// Make the server's copy of `path` equal `text` (`didOpen`, or a
    /// full-text `didChange`). Returns (document version, publish counter at
    /// that moment); nothing is sent when the text is already current.
    pub fn sync(&self, path: &Path, text: &str, language_id: &str) -> Result<(i64, u64), RpcError> {
        if !self.alive() {
            return Err(RpcError::closed(&self.server));
        }
        self.touch();
        let path = crate::tools::canonical(path);
        let uri = uri(&path)?;
        let seq = self.shared.seq.load(Ordering::SeqCst);
        let mut docs = self.docs.lock();
        if let Some(d) = docs.get_mut(&path) {
            d.used = Instant::now();
            if &*d.text == text {
                return Ok((d.version, d.synced_gen));
            }
            d.version = self.next_version.fetch_add(1, Ordering::Relaxed);
            d.text = Arc::from(text);
            d.synced_gen = seq;
            self.notify(
                "textDocument/didChange",
                json!({"textDocument": {"uri": uri, "version": d.version}, "contentChanges": [{"text": text}]}),
            );
            return Ok((d.version, seq));
        }
        if docs.len() >= MAX_OPEN
            && let Some(oldest) = docs
                .iter()
                .min_by_key(|(_, d)| d.used)
                .map(|(p, _)| p.clone())
        {
            docs.remove(&oldest);
            if let Ok(u) = self::uri(&oldest) {
                self.notify("textDocument/didClose", json!({"textDocument": {"uri": u}}));
            }
        }
        let version = self.next_version.fetch_add(1, Ordering::Relaxed);
        docs.insert(
            path,
            Doc {
                version,
                text: Arc::from(text),
                synced_gen: seq,
                used: Instant::now(),
            },
        );
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": language_id, "version": version, "text": text}}),
        );
        Ok((version, seq))
    }

    /// Tell the server the file was written (many run their checks on save).
    pub fn save(&self, path: &Path, text: &str) {
        let Ok(uri) = uri(&crate::tools::canonical(path)) else {
            return;
        };
        let include = self
            .caps
            .lock()
            .pointer("/textDocumentSync/save/includeText")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let params = if include {
            json!({"textDocument": {"uri": uri}, "text": text})
        } else {
            json!({"textDocument": {"uri": uri}})
        };
        self.notify("textDocument/didSave", params);
    }

    pub fn doc(&self, path: &Path) -> Option<Doc> {
        self.docs
            .lock()
            .get(&crate::tools::canonical(path))
            .cloned()
    }

    pub fn open_docs(&self) -> Vec<(PathBuf, Doc)> {
        self.docs
            .lock()
            .iter()
            .map(|(p, d)| (p.clone(), d.clone()))
            .collect()
    }

    pub fn published(&self, path: &Path) -> Option<Published> {
        self.shared
            .diags
            .lock()
            .get(&crate::tools::canonical(path))
            .cloned()
    }

    pub fn all_published(&self) -> HashMap<PathBuf, Published> {
        self.shared.diags.lock().clone()
    }

    pub fn publish_gen(&self) -> u64 {
        self.shared.seq.load(Ordering::SeqCst)
    }

    /// Wait until the server publishes for `path` after `after_gen` (and for
    /// at least `min_version` when it reports versions), or `until`.
    pub async fn wait_publish(
        &self,
        path: &Path,
        after_gen: u64,
        min_version: Option<i64>,
        until: Instant,
    ) -> Option<Published> {
        let path = crate::tools::canonical(path);
        let fresh = |p: &Published| {
            p.seq > after_gen
                && match (min_version, p.version) {
                    (Some(want), Some(got)) => got >= want,
                    _ => true,
                }
        };
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(p) = self.shared.diags.lock().get(&path).filter(|p| fresh(p)) {
                return Some(p.clone());
            }
            if !self.alive() || Instant::now() >= until {
                return None;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(until.into()) => {}
            }
        }
    }

    /// Wait for any publish after `after_gen`; false on timeout or exit.
    pub async fn wait_any(&self, after_gen: u64, until: Instant) -> bool {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.publish_gen() > after_gen {
                return true;
            }
            if !self.alive() || Instant::now() >= until {
                return false;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(until.into()) => {}
            }
        }
    }

    /// How long to wait for diagnostics after a change: about twice what
    /// this server usually needs, within `max`.
    pub fn budget(&self, max: Duration) -> Duration {
        match *self.latency.lock() {
            Some(ms) => Duration::from_millis((ms * 2.0 + 300.0) as u64)
                .clamp(Duration::from_millis(700).min(max), max),
            None => max,
        }
    }

    pub fn record_latency(&self, took: Duration) {
        let ms = took.as_secs_f64() * 1000.0;
        let mut l = self.latency.lock();
        *l = Some(match *l {
            Some(prev) => prev * 0.7 + ms * 0.3,
            None => ms,
        });
    }

    /// Resident memory of the server process in MiB (Linux).
    pub fn rss_mb(&self) -> Option<u64> {
        let pid = crate::sandbox::program_pid(self.process.lock().as_ref()?.id()?);
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb / 1024)
    }

    /// Polite shutdown, then the process group is killed.
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        if self.alive() {
            let _ = self
                .request("shutdown", Value::Null, Duration::from_secs(2))
                .await;
            self.notify("exit", Value::Null);
            let deadline = Instant::now() + Duration::from_millis(800);
            while self.alive() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.kill();
    }

    /// Stop now (the whole process group: servers start helpers such as
    /// `cargo check`).
    pub fn kill(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        #[cfg(windows)]
        if let Some(job) = self.windows_job.lock().take() {
            job.end_off_worker();
        }
        if let Some(mut child) = self.process.lock().take() {
            #[cfg(target_os = "linux")]
            {
                let tree = self.tree.lock().take();
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.spawn(async move {
                            if let Some(tree) = tree {
                                tree.stop_unreaped(&child).await;
                            }
                            let _ = child.start_kill();
                            let _ =
                                tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                        });
                    }
                    Err(_) => {
                        if let Some(tree) = tree {
                            tree.stop_unreaped_inline(&child);
                        }
                        let _ = child.start_kill();
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                #[cfg(unix)]
                crate::tools::bash::signal_unreaped(&mut child);
                let _ = child.start_kill();
            }
        }
    }

    /// Byte offset of an LSP position in `text` (clamped to the text).
    pub fn offset(&self, text: &str, line: u32, character: u32) -> usize {
        offset_at(text, line, character, self.utf8())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn write_loop(mut writer: Writer, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(bytes) = rx.recv().await {
        if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
}

async fn read_loop(
    mut reader: Reader,
    shared: Arc<Shared>,
    outbox: mpsc::UnboundedSender<Vec<u8>>,
) {
    let mut framer = Framer::default();
    let mut buf = vec![0u8; 64 * 1024];
    'outer: loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        framer.push(&buf[..n]);
        loop {
            match framer.next_message() {
                Ok(Some(msg)) => handle(&shared, &outbox, msg),
                Ok(None) => break,
                Err(FrameError::TooLarge(n)) => {
                    tracing::warn!(
                        "lsp[{}] sent a {n}-byte message; disconnecting",
                        shared.server
                    );
                    break 'outer;
                }
                Err(e) => tracing::debug!("lsp[{}] bad frame: {e:?}", shared.server),
            }
        }
    }
    shared.alive.store(false, Ordering::SeqCst);
    for (_, tx) in shared.pending.lock().drain() {
        let _ = tx.send(Err(RpcError::closed(&shared.server)));
    }
    shared.changed.notify_waiters();
}

fn handle(shared: &Shared, outbox: &mpsc::UnboundedSender<Vec<u8>>, msg: Value) {
    let method = msg.get("method").and_then(Value::as_str);
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match (method, msg.get("id")) {
        (Some(m), Some(id)) => {
            let reply = match server_request(shared, m, &params) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                }
            };
            let _ = outbox.send(rpc::encode(&reply));
        }
        (Some(m), None) => notification(shared, m, &params),
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = shared.pending.lock().remove(&id) else {
                return;
            };
            let r = match msg.get("error") {
                Some(e) => Err(RpcError {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string(),
                }),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
        _ => {}
    }
}

/// Requests a server sends us. Tessera applies edits itself, so
/// `workspace/applyEdit` is refused.
fn server_request(shared: &Shared, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "workspace/configuration" => {
            let items = params.get("items").and_then(Value::as_array);
            Ok(Value::Array(
                items
                    .map(|items| {
                        items
                            .iter()
                            .map(|i| {
                                i.get("section")
                                    .and_then(Value::as_str)
                                    .and_then(|s| {
                                        s.split('.').try_fold(&shared.settings, |v, k| v.get(k))
                                    })
                                    .cloned()
                                    .unwrap_or(Value::Null)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ))
        }
        "workspace/workspaceFolders" => {
            Ok(json!([{"uri": shared.root_uri, "name": shared.root_name}]))
        }
        "workspace/applyEdit" => {
            Ok(json!({"applied": false, "failureReason": "Tessera applies edits itself"}))
        }
        "window/showDocument" => Ok(json!({"success": false})),
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest"
        | "workspace/diagnostic/refresh"
        | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/inlineValue/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/foldingRange/refresh" => Ok(Value::Null),
        _ => Err((-32601, format!("unsupported: {method}"))),
    }
}

fn notification(shared: &Shared, method: &str, params: &Value) {
    match method {
        "textDocument/publishDiagnostics" => {
            let Some(path) = params.get("uri").and_then(Value::as_str).and_then(path_of) else {
                return;
            };
            let items: Vec<Diagnostic> = params
                .get("diagnostics")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Diagnostic::from_lsp).collect())
                .unwrap_or_default();
            let version = params.get("version").and_then(Value::as_i64);
            let path = crate::tools::canonical(&path);
            {
                let mut diags = shared.diags.lock();
                // A late answer about an older version must not replace a newer one.
                if let (
                    Some(new),
                    Some(Published {
                        version: Some(old), ..
                    }),
                ) = (version, diags.get(&path))
                    && new < *old
                {
                    return;
                }
                let seq = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
                diags.insert(
                    path,
                    Published {
                        version,
                        items: Arc::new(items),
                        seq,
                    },
                );
            }
            shared.changed.notify_waiters();
        }
        "$/progress" => {
            let token = match params.get("token") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => return,
            };
            match params.pointer("/value/kind").and_then(Value::as_str) {
                Some("begin") => {
                    shared.progress.lock().insert(token);
                }
                Some("end") => {
                    shared.progress.lock().remove(&token);
                }
                _ => return,
            }
            shared.changed.notify_waiters();
        }
        "experimental/serverStatus" => {
            let q = params
                .get("quiescent")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            shared.quiescent.store(q, Ordering::Relaxed);
            shared.changed.notify_waiters();
        }
        "window/logMessage" | "window/showMessage" => {
            let message = params.get("message").and_then(Value::as_str).unwrap_or("");
            tracing::debug!("lsp[{}] {message}", shared.server);
        }
        _ => {}
    }
}

pub fn uri(path: &Path) -> Result<String, RpcError> {
    url::Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|_| RpcError {
            code: 0,
            message: format!("not an absolute path: {}", path.display()),
        })
}

pub fn path_of(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

/// Column in LSP units (UTF-8 bytes or UTF-16 code units) of byte `byte`.
pub fn units_at(line: &str, byte: usize, utf8: bool) -> u32 {
    let byte = crate::util::floor_char(line, byte.min(line.len()));
    if utf8 {
        byte as u32
    } else {
        line[..byte].encode_utf16().count() as u32
    }
}

/// Byte offset within `line` of an LSP column (clamped to the line).
pub fn byte_at(line: &str, units: u32, utf8: bool) -> usize {
    if utf8 {
        return crate::util::floor_char(line, (units as usize).min(line.len()));
    }
    let mut seen = 0u32;
    for (i, ch) in line.char_indices() {
        if seen >= units {
            return i;
        }
        seen += ch.len_utf16() as u32;
    }
    line.len()
}

/// Byte offset in `text` of an LSP position; lines end at `\n` (a `\r`
/// before it is not part of the line). Past the end clamps to the end.
pub fn offset_at(text: &str, line: u32, units: u32, utf8: bool) -> usize {
    let mut start = 0usize;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let end = text[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(text.len());
    let content = text[start..end].trim_end_matches('\r');
    start + byte_at(content, units, utf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::fake;

    #[cfg(target_os = "linux")]
    #[test]
    fn lsp_drop_without_runtime_stops_spawn_owned_setsid_descendant() {
        if !crate::tools::bash::tests::cleanup_fixture(
            "lsp::client::tests::lsp_drop_without_runtime_stops_spawn_owned_setsid_descendant",
        ) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, leader, descendant) = runtime.block_on(async {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "setsid /bin/sleep 30 & echo $! > \"$1\"; wait",
                    "fixture",
                ])
                .arg(&ready)
                .env_clear()
                .env("PATH", "/usr/bin:/bin");
            let child =
                crate::platform::spawn(&mut command, crate::platform::Start::Agent).unwrap();
            let leader = child.id().unwrap() as i32;
            let tree = child.id().map(crate::tools::bash::ChildTree::capture);
            let (mut transport, _fake) = fake::pair(fake::Behavior::default());
            transport.process = Some(child);
            transport.tree = tree;
            let client = LspClient::start(
                "no-runtime-fake",
                dir.path(),
                transport,
                Value::Null,
                Value::Null,
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            let until = Instant::now() + Duration::from_secs(3);
            let descendant: i32 = loop {
                if let Ok(text) = std::fs::read_to_string(&ready)
                    && let Ok(pid) = text.trim().parse()
                {
                    break pid;
                }
                assert!(
                    Instant::now() < until,
                    "fake did not publish its setsid child"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            tokio::time::sleep(Duration::from_millis(100)).await;
            (client, leader, descendant)
        });
        assert_eq!(
            unsafe { (libc::getpgid(descendant), libc::getsid(descendant)) },
            (descendant, descendant)
        );
        assert!(Path::new(&format!("/proc/{leader}")).exists());
        // Tear down the setup runtime first. Drop must take the inline branch,
        // not panic or schedule cleanup onto a runtime that cannot execute it.
        runtime.shutdown_timeout(Duration::from_secs(1));
        assert!(tokio::runtime::Handle::try_current().is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(client))).is_ok());
        crate::tools::bash::tests::reaped_disappearance(&[leader, descendant]);
    }
    #[test]
    fn positions_in_utf8_and_utf16() {
        let line = "let é = \"😀\"; x";
        let x = line.find('x').unwrap();
        assert_eq!(units_at(line, x, true), x as u32);
        // é is 1 UTF-16 unit, 😀 is 2.
        assert_eq!(units_at(line, x, false), 14);
        assert_eq!(byte_at(line, 14, false), x);
        assert_eq!(byte_at(line, 999, false), line.len());
        let text = "ab\r\ncdé\nlast";
        assert_eq!(offset_at(text, 1, 2, false), 6);
        assert_eq!(offset_at(text, 0, 99, false), 2, "stops before \\r");
        assert_eq!(offset_at(text, 9, 0, false), text.len());
        assert_eq!(&text[offset_at(text, 2, 1, true)..], "ast");
    }

    #[tokio::test]
    async fn handshake_sync_publish_config_and_exit() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::tools::canonical(dir.path());
        let file = root.join("a.foo");
        std::fs::write(&file, "ok\n").unwrap();
        let (transport, server) = fake::pair(fake::Behavior::default());
        let settings = json!({"foo": {"answer": 42}});
        let c = LspClient::start(
            "fake",
            &root,
            transport,
            Value::Null,
            settings,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(c.supports("definitionProvider") && !c.supports("renameProvider"));
        // The fake asks for `foo` settings right after `initialized`.
        let answered = server.config_answer().await;
        assert_eq!(answered, json!([{"answer": 42}, null]));

        let (v, g) = c.sync(&file, "ok\nBAD here\n", "foo").unwrap();
        assert_eq!(v, 1);
        let p = c
            .wait_publish(&file, g, Some(v), Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].line, 1);
        // Same text again: nothing is sent, the old counter comes back.
        assert_eq!(c.sync(&file, "ok\nBAD here\n", "foo").unwrap(), (1, g));
        assert_eq!(server.count("textDocument/didOpen"), 1);
        assert_eq!(server.count("textDocument/didChange"), 0);
        let (v2, g2) = c.sync(&file, "fine\n", "foo").unwrap();
        assert_eq!(v2, 2);
        let p = c
            .wait_publish(&file, g2, Some(v2), Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        assert!(p.items.is_empty());
        assert_eq!(p.version, Some(2));

        // A request the server never answers times out and is cancelled.
        let err = c
            .request("test/hang", Value::Null, Duration::from_millis(200))
            .await
            .unwrap_err();
        assert_eq!(err.code, RpcError::TIMEOUT);
        assert!(server.saw("$/cancelRequest").await);

        // The server going away fails what's pending and marks it dead.
        let pending = {
            let c = c.clone();
            tokio::spawn(async move {
                c.request("test/hang", Value::Null, Duration::from_secs(10))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.crash();
        let r = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.unwrap_err().code, RpcError::CLOSED);
        assert!(!c.alive() && !c.stopping());
    }

    #[test]
    fn older_publishes_never_replace_newer_ones() {
        let shared = Shared {
            server: "t".into(),
            root_uri: "file:///".into(),
            root_name: "r".into(),
            settings: Value::Null,
            pending: Mutex::new(HashMap::new()),
            diags: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            changed: Notify::new(),
            alive: AtomicBool::new(true),
            progress: Mutex::new(HashSet::new()),
            quiescent: AtomicBool::new(true),
            stderr_tail: Mutex::new(String::new()),
        };
        let publish = |version: i64, n: usize| {
            let diags: Vec<Value> = (0..n)
                .map(|i| json!({"range": {"start": {"line": i, "character": 0}}, "message": "m"}))
                .collect();
            notification(
                &shared,
                "textDocument/publishDiagnostics",
                &json!({"uri": "file:///nonexistent-tessera/x.rs", "version": version, "diagnostics": diags}),
            );
        };
        publish(3, 1);
        publish(2, 5);
        let d = shared.diags.lock();
        let p = d.values().next().unwrap();
        assert_eq!((p.version, p.items.len(), p.seq), (Some(3), 1, 1));
        drop(d);
        // Progress tokens make the server busy until they end.
        notification(
            &shared,
            "$/progress",
            &json!({"token": 7, "value": {"kind": "begin"}}),
        );
        assert_eq!(shared.progress.lock().len(), 1);
        notification(
            &shared,
            "$/progress",
            &json!({"token": 7, "value": {"kind": "end"}}),
        );
        assert!(shared.progress.lock().is_empty());
    }

    #[tokio::test]
    async fn applies_no_workspace_edits_and_answers_unknown_requests() {
        let dir = tempfile::tempdir().unwrap();
        let (transport, server) = fake::pair(fake::Behavior {
            ask_apply_edit: true,
            ..Default::default()
        });
        let _c = LspClient::start(
            "fake",
            dir.path(),
            transport,
            Value::Null,
            Value::Null,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let answer = server.apply_edit_answer().await;
        assert_eq!(answer["applied"], false);
    }
}
