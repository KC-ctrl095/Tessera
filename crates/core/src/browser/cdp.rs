//! A small Chrome DevTools Protocol client over `--remote-debugging-pipe`.
//!
//! The browser reads commands on its fd 3 and writes replies and events on
//! fd 4, one JSON message per NUL-terminated frame. Nothing listens on a
//! port, so only this process can drive the browser. Page targets are
//! attached in "flat" mode: every message travels on the one pipe, tagged
//! with its `sessionId`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

/// Longest message accepted from the browser (a full-page screenshot of a
/// long page is the largest thing it sends).
const MAX_MESSAGE: usize = 96 << 20;
/// How long a command may take before the call gives up.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// An event from the browser.
#[derive(Debug)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
}

type Reply = Result<Value, String>;
type WireReply = Result<Value, (Option<i64>, String)>;
/// The sole synthetic response allowed during a noopener target's setup.
/// Empty content, no redirects/cookies, no scripts, no cache entry.
pub(crate) fn inert_document_response(request_id: &str) -> Value {
    json!({
        "requestId":request_id, "responseCode":200,
        "responseHeaders":[
            {"name":"Content-Type","value":"text/html"},
            {"name":"Cache-Control","value":"no-store"}
        ], "body":""
    })
}

pub struct Cdp {
    out: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<WireReply>>>,
    /// Event subscribers by session id ("" = the browser itself).
    subscribers: Mutex<HashMap<String, mpsc::UnboundedSender<CdpEvent>>>,
    closed: watch::Sender<bool>,
    #[cfg(test)]
    delayed_reply: Mutex<Option<DelayedReply>>,
}

/// A request whose receiver and id remain owned across caller cancellation.
/// Rollback's generation deadline, not a per-reply timeout, owns recovery.
pub(super) struct OwnedReply {
    cdp: Arc<Cdp>,
    id: u64,
    reply: oneshot::Receiver<WireReply>,
}
impl OwnedReply {
    pub(super) async fn wait(&mut self) -> WireReply {
        (&mut self.reply)
            .await
            .unwrap_or_else(|_| Err((None, "the browser closed".into())))
    }
}
impl Drop for OwnedReply {
    fn drop(&mut self) {
        self.cdp.pending.lock().remove(&self.id);
    }
}

#[cfg(test)]
struct DelayedReply {
    method: &'static str,
    except_session: Option<String>,
    id: Option<u64>,
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}
#[derive(Deserialize)]
struct Incoming {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RemoteError>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

/// Messages a [`Cdp::channel`] client sends, one JSON text each.
pub type Outgoing = mpsc::UnboundedReceiver<Vec<u8>>;

impl Cdp {
    fn new() -> (Arc<Self>, Outgoing, mpsc::UnboundedReceiver<CdpEvent>) {
        let (out, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (browser_tx, browser_rx) = mpsc::unbounded_channel();
        let (closed, _) = watch::channel(false);
        let cdp = Arc::new(Self {
            out,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(HashMap::from([(String::new(), browser_tx)])),
            closed,
            #[cfg(test)]
            delayed_reply: Mutex::new(None),
        });
        (cdp, out_rx, browser_rx)
    }

    /// A client for a browser elsewhere (a machine's, through its
    /// connection): what it sends comes out of [`Outgoing`], and what the
    /// browser answers goes in through [`Cdp::feed`].
    pub fn channel() -> (Arc<Self>, Outgoing, mpsc::UnboundedReceiver<CdpEvent>) {
        Self::new()
    }

    /// One message from the browser (a [`Cdp::channel`] client).
    pub fn feed(&self, raw: &[u8]) {
        if raw.len() <= MAX_MESSAGE {
            self.dispatch(raw);
        }
    }

    /// The browser is gone (a [`Cdp::channel`] client): pending calls fail
    /// and every subscriber's loop ends.
    pub fn close(&self) {
        self.shut();
    }

    /// Drive a browser through its pipe ends. Browser-level events arrive on
    /// the returned receiver.
    pub fn start<R, W>(reader: R, writer: W) -> (Arc<Self>, mpsc::UnboundedReceiver<CdpEvent>)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (cdp, mut out_rx, browser_rx) = Self::new();
        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(mut msg) = out_rx.recv().await {
                msg.push(0);
                if writer.write_all(&msg).await.is_err() {
                    break;
                }
            }
        });
        let me = Arc::downgrade(&cdp);
        tokio::spawn(async move {
            let mut reader = reader;
            let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
            let mut chunk = vec![0u8; 1 << 16];
            let mut scanned = 0;
            'read: loop {
                let n = match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                loop {
                    let Some(end) = buf[scanned..].iter().position(|b| *b == 0) else {
                        scanned = buf.len();
                        if buf.len() > MAX_MESSAGE {
                            tracing::warn!("browser sent an oversized message; disconnecting");
                            break 'read;
                        }
                        break;
                    };
                    let end = scanned + end;
                    let Some(cdp) = me.upgrade() else {
                        break 'read;
                    };
                    cdp.dispatch(&buf[..end]);
                    buf.drain(..=end);
                    scanned = 0;
                }
            }
            if let Some(cdp) = me.upgrade() {
                cdp.shut();
            }
        });
        (cdp, browser_rx)
    }

    fn dispatch(&self, raw: &[u8]) {
        let msg: Incoming = match serde_json::from_slice(raw) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("unreadable message from the browser: {e}");
                return;
            }
        };
        if let Some(id) = msg.id {
            if let Some(tx) = self.pending.lock().remove(&id) {
                let reply = match msg.error {
                    Some(e) => Err((
                        Some(e.code),
                        match e.data {
                            Some(d) => format!("{} ({d})", e.message),
                            _ => e.message,
                        },
                    )),
                    None => Ok(msg.result.unwrap_or(Value::Null)),
                };
                #[cfg(test)]
                if let Some(delayed) = self
                    .delayed_reply
                    .lock()
                    .take_if(|gate| gate.id == Some(id))
                {
                    let _ = delayed.reached.send(());
                    tokio::spawn(async move {
                        let _ = delayed.release.await;
                        let _ = tx.send(reply);
                    });
                    return;
                }
                let _ = tx.send(reply);
            }
            return;
        }
        let Some(method) = msg.method else { return };
        let key = msg.session_id.unwrap_or_default();
        let subs = self.subscribers.lock();
        if let Some(tx) = subs.get(&key) {
            let _ = tx.send(CdpEvent {
                method,
                params: msg.params.unwrap_or(Value::Null),
            });
        }
    }

    /// The pipe closed (the browser exited or was stopped).
    fn shut(&self) {
        self.closed.send_replace(true);
        for (_, tx) in self.pending.lock().drain() {
            let _ = tx.send(Err((None, "the browser closed".into())));
        }
        // Dropping the senders ends every subscriber's loop.
        self.subscribers.lock().clear();
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Resolves once the browser is gone.
    pub async fn wait_closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }

    fn encode(&self, id: u64, session: Option<&str>, method: &str, params: Value) -> Vec<u8> {
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        serde_json::to_vec(&msg).unwrap_or_default()
    }

    /// Start now and keep the pending id owned until reply or rollback.
    pub(super) fn enqueue_owned(self: &Arc<Self>, method: &str, params: Value) -> OwnedReply {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, reply) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        #[cfg(test)]
        self.arm_delayed_reply(id, None, method);
        if (self.is_closed()
            || self
                .out
                .send(self.encode(id, None, method, params))
                .is_err())
            && let Some(tx) = self.pending.lock().remove(&id)
        {
            let _ = tx.send(Err((None, "the browser closed".into())));
        }
        OwnedReply {
            cdp: self.clone(),
            id,
            reply,
        }
    }

    #[cfg(test)]
    pub(super) fn delay_next_reply(
        &self,
        method: &'static str,
        except_session: Option<&str>,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached, waiting) = oneshot::channel();
        let (release, delayed) = oneshot::channel();
        let mut gate = self.delayed_reply.lock();
        assert!(gate.is_none());
        *gate = Some(DelayedReply {
            method,
            except_session: except_session.map(str::to_string),
            id: None,
            reached,
            release: delayed,
        });
        (waiting, release)
    }

    #[cfg(test)]
    fn arm_delayed_reply(&self, id: u64, session: Option<&str>, method: &str) {
        if let Some(gate) = self.delayed_reply.lock().as_mut()
            && gate.id.is_none()
            && gate.method == method
            && gate
                .except_session
                .as_deref()
                .is_none_or(|except| session != Some(except))
        {
            gate.id = Some(id);
        }
    }
    /// Send a command and wait for its result.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Reply {
        self.call_timeout(session, method, params, CALL_TIMEOUT)
            .await
    }

    pub async fn call_timeout(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Reply {
        self.enqueue_timeout(session, method, params, timeout)
            .await
            .map_err(|(_, message)| message)
    }

    /// Enqueue now, returning the reply separately. Ordered setup commands
    /// can be sent before waiting for a blocked renderer's acknowledgements.
    pub fn enqueue(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> impl std::future::Future<Output = Reply> + Send + '_ {
        let pending = self.enqueue_timeout(session, method, params, CALL_TIMEOUT);
        async move { pending.await.map_err(|(_, message)| message) }
    }

    /// Any well-formed protocol refusal means this optional feature is absent.
    /// Transport loss, malformed responses and timeouts remain failures.
    pub(crate) async fn optional_call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Option<Value>, String> {
        match self
            .enqueue_timeout(None, method, params, CALL_TIMEOUT)
            .await
        {
            Ok(value) => Ok(Some(value)),
            Err((Some(_), _)) => Ok(None),
            Err((_, message)) => Err(message),
        }
    }

    fn enqueue_timeout(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> impl std::future::Future<Output = WireReply> + Send + '_ {
        struct Pending<'a> {
            cdp: &'a Cdp,
            id: u64,
        }
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                self.cdp.pending.lock().remove(&self.id);
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        #[cfg(test)]
        self.arm_delayed_reply(id, session, method);
        let sent = !self.is_closed()
            && self
                .out
                .send(self.encode(id, session, method, params))
                .is_ok();
        let pending = Pending { cdp: self, id };
        let method = method.to_string();
        async move {
            let _pending = pending;
            if !sent {
                return Err((None, "the browser closed".into()));
            }
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => Err((None, "the browser closed".into())),
                Err(_) => Err((
                    None,
                    format!("the browser didn't answer `{method}` in time"),
                )),
            }
        }
    }

    /// Confirm independent script (freeze) and all-network (Fetch) holds
    /// before lifting debugger-start wait to unblock about:blank setup ACKs.
    /// A noopener target has no renderer until its first response: supply only
    /// an inert, uncacheable empty document, then reload after all guards ACK.
    /// No failed setup ever sends the original request or thaws page scripts.
    pub(crate) async fn initialize_page(
        &self,
        session: &str,
        keys: &str,
        patterns: Option<Vec<Value>>,
        bootstrap_target: Option<&str>,
        events: &mut mpsc::UnboundedReceiver<CdpEvent>,
    ) -> Result<(bool, Vec<CdpEvent>), String> {
        let s = Some(session);
        self.call(s, "Page.setWebLifecycleState", json!({"state":"frozen"}))
            .await?;
        self.call(s, "Fetch.enable", json!({"patterns":[{"urlPattern":"*"}]}))
            .await?;
        let page = self.enqueue(s, "Page.enable", json!({}));
        let guard = self.enqueue(
            s,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":keys,"runImmediately":true}),
        );
        let resume = self.enqueue(s, "Runtime.runIfWaitingForDebugger", json!({}));
        let mut bootstrap = false;
        let mut held_events = Vec::new();
        tokio::pin!(page);
        loop {
            tokio::select! {
                biased;
                result = &mut page => { result?; break; }
                event = events.recv() => {
                    let event = event.ok_or("the browser closed during setup")?;
                    let p = &event.params;
                    if !bootstrap && event.method == "Fetch.requestPaused"
                        && p["resourceType"] == "Document"
                        && bootstrap_target.is_some_and(|t| p["frameId"] == t)
                    {
                        // Chrome's native reload retains the initial GET/POST,
                        // body and referrer. Nothing from the site (or network)
                        // enters this document while mandatory setup is pending.
                        let id = p["requestId"].as_str().filter(|id| !id.is_empty())
                            .ok_or("the held document needs its request id")?;
                        self.call(s, "Fetch.fulfillRequest", inert_document_response(id)).await?;
                        bootstrap = true;
                    } else {
                        held_events.push(event);
                    }
                }
            }
        }
        let guard = guard.await?;
        if !guard["identifier"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
        {
            return Err("the browser didn't install the sign-in guard".into());
        }
        resume.await?;
        if let Some(patterns) = patterns {
            self.call(s, "Fetch.enable", json!({"patterns":patterns}))
                .await?;
        } else {
            self.call(s, "Fetch.disable", json!({})).await?;
        }
        self.call(s, "Page.setWebLifecycleState", json!({"state":"active"}))
            .await?;
        Ok((bootstrap, held_events))
    }
    /// Send a command without waiting (its reply is dropped).
    pub fn send(&self, session: Option<&str>, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(self.encode(id, session, method, params));
    }

    /// Receive the events of one attached target.
    pub fn subscribe(&self, session: &str) -> mpsc::UnboundedReceiver<CdpEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        if !self.is_closed() {
            self.subscribers.lock().insert(session.to_string(), tx);
        }
        rx
    }

    pub fn unsubscribe(&self, session: &str) {
        self.subscribers.lock().remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake browser on the other end of two in-memory pipes.
    #[tokio::test]
    async fn calls_resolve_and_events_reach_their_session() {
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let (r, w) = tokio::io::split(ours);
        let (cdp, mut browser_events) = Cdp::start(r, w);
        let (mut tr, mut tw) = tokio::io::split(theirs);
        let fake = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = tr.read(&mut chunk).await.unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                while let Some(end) = buf.iter().position(|b| *b == 0) {
                    let msg: Value = serde_json::from_slice(&buf[..end]).unwrap();
                    buf.drain(..=end);
                    let id = msg["id"].as_u64().unwrap();
                    let mut out = Vec::new();
                    // An event for the session first, split across writes.
                    if let Some(s) = msg["sessionId"].as_str() {
                        out.extend(
                            serde_json::to_vec(&json!({"method": "Page.loadEventFired", "params": {"t": 1}, "sessionId": s}))
                                .unwrap(),
                        );
                        out.push(0);
                    }
                    out.extend(
                        serde_json::to_vec(
                            &json!({"method": "Target.targetCreated", "params": {}}),
                        )
                        .unwrap(),
                    );
                    out.push(0);
                    let reply = if msg["method"] == "Fail.me" {
                        json!({"id": id, "error": {"code": -32000, "message": "nope"}})
                    } else {
                        json!({"id": id, "result": {"echo": msg["method"]}})
                    };
                    out.extend(serde_json::to_vec(&reply).unwrap());
                    out.push(0);
                    let (a, b) = out.split_at(out.len() / 2);
                    tw.write_all(a).await.unwrap();
                    tw.flush().await.unwrap();
                    tw.write_all(b).await.unwrap();
                }
            }
        });
        let mut page = cdp.subscribe("S1");
        let r = cdp
            .call(Some("S1"), "Page.enable", json!({}))
            .await
            .unwrap();
        assert_eq!(r["echo"], "Page.enable");
        assert_eq!(page.recv().await.unwrap().method, "Page.loadEventFired");
        assert_eq!(
            browser_events.recv().await.unwrap().method,
            "Target.targetCreated"
        );
        assert_eq!(
            cdp.call(None, "Fail.me", json!({})).await.unwrap_err(),
            "nope"
        );
        drop(cdp);
        fake.abort();
    }

    #[tokio::test]
    async fn a_closed_pipe_fails_pending_calls() {
        let (ours, theirs) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(ours);
        let (cdp, _ev) = Cdp::start(r, w);
        let c = cdp.clone();
        let call = tokio::spawn(async move { c.call(None, "Browser.getVersion", json!({})).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(theirs);
        assert!(call.await.unwrap().is_err());
        cdp.wait_closed().await;
        assert!(cdp.is_closed());
    }
    #[tokio::test]
    async fn startup_calls_enqueue_in_order_before_any_reply_is_polled() {
        let (cdp, mut rx, _events) = Cdp::channel();
        let page = cdp.enqueue(Some("page"), "Page.enable", json!({}));
        let keys = cdp.enqueue(
            Some("page"),
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":"keys"}),
        );
        let resume = cdp.enqueue(Some("page"), "Runtime.runIfWaitingForDebugger", json!({}));
        for (id, method) in [
            (1, "Page.enable"),
            (2, "Page.addScriptToEvaluateOnNewDocument"),
            (3, "Runtime.runIfWaitingForDebugger"),
        ] {
            let msg: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
            assert_eq!(msg["id"], id);
            assert_eq!(msg["method"], method);
            assert_eq!(msg["sessionId"], "page");
        }
        assert_eq!(cdp.pending.lock().len(), 3);
        cdp.feed(br#"{"id":1,"result":{}}"#);
        assert!(page.await.is_ok());
        drop(keys);
        drop(resume);
        assert!(
            cdp.pending.lock().is_empty(),
            "cancelled startup replies leave no pending waiters"
        );
    }
    #[tokio::test]
    async fn failed_mandatory_setup_never_thaws_and_about_blank_acks_can_wait_for_resume() {
        for fail in [
            "hold",
            "networkhold",
            "page",
            "guard",
            "identifier",
            "fetch",
            "none",
        ] {
            let (cdp, mut rx, _events) = Cdp::channel();
            let init = cdp.clone();
            let mut events = init.subscribe("page");
            let task = tokio::spawn(async move {
                init.initialize_page(
                    "page",
                    "keys",
                    Some(vec![json!({"urlPattern":"*://localhost:7878/*"})]),
                    None,
                    &mut events,
                )
                .await
            });
            let hold: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
            assert_eq!(hold["method"], "Page.setWebLifecycleState");
            assert_eq!(hold["params"]["state"], "frozen");
            assert!(
                rx.try_recv().is_err(),
                "must confirm independent hold before debugger resume"
            );
            let answer = if fail == "hold" {
                json!({"id":hold["id"],"error":{"code":-32000,"message":"injected hold error"}})
            } else {
                json!({"id":hold["id"],"result":{}})
            };
            cdp.feed(&serde_json::to_vec(&answer).unwrap());
            if fail != "hold" {
                let network: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(network["method"], "Fetch.enable");
                assert_eq!(network["params"]["patterns"], json!([{"urlPattern":"*"}]));
                assert!(
                    rx.try_recv().is_err(),
                    "confirm all-network hold before resume"
                );
                let answer = if fail == "networkhold" {
                    json!({"id":network["id"],"error":{"code":-32000,"message":"injected network hold error"}})
                } else {
                    json!({"id":network["id"],"result":{}})
                };
                cdp.feed(&serde_json::to_vec(&answer).unwrap());
            }
            if !matches!(fail, "hold" | "networkhold") {
                let mut sent = Vec::new();
                for method in [
                    "Page.enable",
                    "Page.addScriptToEvaluateOnNewDocument",
                    "Runtime.runIfWaitingForDebugger",
                ] {
                    let msg: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                    assert_eq!(msg["method"], method);
                    assert_eq!(msg["sessionId"], "page");
                    sent.push(msg);
                }
                // Simulate synchronous about:blank: no setup ACK until resume
                // reaches the renderer, still held by the acknowledged freeze.
                for msg in sent {
                    let method = msg["method"].as_str().unwrap();
                    let reject = matches!(
                        (fail, method),
                        ("page", "Page.enable")
                            | ("guard", "Page.addScriptToEvaluateOnNewDocument")
                    );
                    let result = if method == "Page.addScriptToEvaluateOnNewDocument"
                        && fail != "identifier"
                    {
                        json!({"identifier":"confirmed"})
                    } else {
                        json!({})
                    };
                    let answer = if reject {
                        json!({"id":msg["id"],"error":{"code":-32000,"message":"injected mandatory setup error"}})
                    } else {
                        json!({"id":msg["id"],"result":result})
                    };
                    cdp.feed(&serde_json::to_vec(&answer).unwrap());
                }
            }
            if matches!(fail, "fetch" | "none") {
                let network: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(network["method"], "Fetch.enable");
                assert_ne!(network["params"]["patterns"], json!([{"urlPattern":"*"}]));
                let answer = if fail == "fetch" {
                    json!({"id":network["id"],"error":{"code":-32000,"message":"injected final network setup error"}})
                } else {
                    json!({"id":network["id"],"result":{}})
                };
                cdp.feed(&serde_json::to_vec(&answer).unwrap());
            }
            if fail == "none" {
                let thaw: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(thaw["method"], "Page.setWebLifecycleState");
                assert_eq!(thaw["params"]["state"], "active");
                cdp.feed(&serde_json::to_vec(&json!({"id":thaw["id"],"result":{}})).unwrap());
            }
            assert_eq!(task.await.unwrap().is_ok(), fail == "none", "{fail}");
            assert!(
                rx.try_recv().is_err(),
                "{fail}: rejected setup must never enqueue thaw"
            );
            assert!(
                cdp.pending.lock().is_empty(),
                "{fail}: failed setup leaves no pending waiter"
            );
        }
    }
    #[tokio::test]
    async fn noopener_bootstrap_is_inert_and_never_releases_original_network_on_failure() {
        for fail in ["inert", "identifier", "none"] {
            let (cdp, mut rx, _events) = Cdp::channel();
            let init = cdp.clone();
            let mut events = init.subscribe("page");
            let task = tokio::spawn(async move {
                init.initialize_page("page", "keys", None, Some("target"), &mut events)
                    .await
            });
            let answer = |msg: &Value, result: Value| {
                cdp.feed(&serde_json::to_vec(&json!({"id":msg["id"],"result":result})).unwrap());
            };
            for method in ["Page.setWebLifecycleState", "Fetch.enable"] {
                let msg: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(msg["method"], method);
                answer(&msg, json!({}));
            }
            let mut startup = Vec::new();
            for method in [
                "Page.enable",
                "Page.addScriptToEvaluateOnNewDocument",
                "Runtime.runIfWaitingForDebugger",
            ] {
                let msg: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(msg["method"], method);
                startup.push(msg);
            }
            // A subframe request stays held; only this target's first Document
            // may be replaced with the fixed empty, uncacheable response.
            for frame in ["other", "target"] {
                cdp.feed(
                    &serde_json::to_vec(&json!({
                        "method":"Fetch.requestPaused", "sessionId":"page", "params":{
                            "requestId":"held", "frameId":frame, "resourceType":"Document"
                        }
                    }))
                    .unwrap(),
                );
            }
            let blank: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
            assert_eq!(blank["method"], "Fetch.fulfillRequest");
            assert_eq!(blank["params"], inert_document_response("held"));
            assert!(rx.try_recv().is_err(), "original navigation remains held");
            if fail == "inert" {
                cdp.feed(
                    &serde_json::to_vec(
                        &json!({"id":blank["id"],"error":{"code":-32000,"message":"injected"}}),
                    )
                    .unwrap(),
                );
            } else {
                answer(&blank, json!({}));
                for msg in startup {
                    let result = if msg["method"] == "Page.addScriptToEvaluateOnNewDocument"
                        && fail != "identifier"
                    {
                        json!({"identifier":"confirmed"})
                    } else {
                        json!({})
                    };
                    answer(&msg, result);
                }
            }
            if fail == "none" {
                let network: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(network["method"], "Fetch.disable");
                answer(&network, json!({}));
                let thaw: Value = serde_json::from_slice(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(thaw["params"]["state"], "active");
                answer(&thaw, json!({}));
            }
            let result = task.await.unwrap();
            assert_eq!(result.is_ok(), fail == "none");
            if let Ok((bootstrap, held)) = result {
                assert!(bootstrap);
                assert_eq!(held.len(), 1);
                assert_eq!(held[0].params["frameId"], "other");
            }
            assert!(
                rx.try_recv().is_err(),
                "{fail}: no network continue, reload or late thaw"
            );
            assert!(cdp.pending.lock().is_empty());
        }
    }
}
