//! An in-process language server for tests, speaking real LSP framing over
//! an in-memory pipe. It reports an error on every line containing `BAD`
//! (a warning for `WARN`), answers definition/references/hover/symbols from
//! the open documents, renames words across them, and can crash on demand.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::client::Transport;
use super::rpc::{Framer, encode};

#[derive(Debug, Clone, Default)]
pub struct Behavior {
    /// Ask the client to apply an edit after `initialized`.
    pub ask_apply_edit: bool,
    /// Wait this long before publishing diagnostics.
    pub publish_delay: Duration,
    /// Never publish diagnostics.
    pub silent: bool,
}

#[derive(Default)]
struct State {
    log: Mutex<Vec<(String, Value)>>,
    replies: Mutex<HashMap<String, Value>>,
    docs: Mutex<HashMap<String, String>>,
    notify: Notify,
    crash: CancellationToken,
}

#[derive(Clone)]
pub struct Handle(Arc<State>);

impl Handle {
    async fn wait_for<T>(&self, mut f: impl FnMut(&State) -> Option<T>) -> Option<T> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let n = self.0.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if let Some(v) = f(&self.0) {
                return Some(v);
            }
            if tokio::time::timeout_at(deadline, n).await.is_err() {
                return f(&self.0);
            }
        }
    }

    /// What the client answered to our `workspace/configuration`.
    pub async fn config_answer(&self) -> Value {
        self.wait_for(|s| s.replies.lock().get("cfg").cloned())
            .await
            .unwrap_or(Value::Null)
    }

    pub async fn apply_edit_answer(&self) -> Value {
        self.wait_for(|s| s.replies.lock().get("apply").cloned())
            .await
            .unwrap_or(Value::Null)
    }

    /// Whether the server received `method` (waits a little for it).
    pub async fn saw(&self, method: &str) -> bool {
        self.wait_for(|s| s.log.lock().iter().any(|(m, _)| m == method).then_some(()))
            .await
            .is_some()
    }

    pub fn count(&self, method: &str) -> usize {
        self.0
            .log
            .lock()
            .iter()
            .filter(|(m, _)| m == method)
            .count()
    }

    pub fn crash(&self) {
        self.0.crash.cancel();
    }
}

/// A client-side transport and a handle on the server behind it.
pub fn pair(behavior: Behavior) -> (Transport, Handle) {
    let (client, server) = tokio::io::duplex(1 << 20);
    let (cr, cw) = tokio::io::split(client);
    let state = Arc::new(State::default());
    tokio::spawn(serve(server, behavior, state.clone()));
    (
        Transport {
            reader: Box::new(cr),
            writer: Box::new(cw),
            process: None,
            #[cfg(windows)]
            windows_job: None,
            #[cfg(target_os = "linux")]
            tree: None,
        },
        Handle(state),
    )
}

async fn serve(stream: tokio::io::DuplexStream, b: Behavior, st: Arc<State>) {
    let (mut r, mut w) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if w.write_all(&encode(&msg)).await.is_err() {
                break;
            }
        }
    });
    let mut framer = Framer::default();
    let mut buf = vec![0u8; 16 * 1024];
    'outer: loop {
        let n = tokio::select! {
            n = r.read(&mut buf) => match n { Ok(0) | Err(_) => break, Ok(n) => n },
            _ = st.crash.cancelled() => break,
        };
        framer.push(&buf[..n]);
        while let Ok(Some(msg)) = framer.next_message() {
            let method = msg
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string);
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let id = msg.get("id").cloned();
            match method {
                None => {
                    // A reply to one of our requests.
                    if let Some(id) = id.as_ref().and_then(Value::as_str) {
                        let v = msg.get("result").cloned().unwrap_or(Value::Null);
                        st.replies.lock().insert(id.to_string(), v);
                    }
                }
                Some(m) => {
                    st.log.lock().push((m.clone(), params.clone()));
                    let reply = |result: Value| {
                        if let Some(id) = &id {
                            let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
                        }
                    };
                    match m.as_str() {
                        "initialize" => reply(json!({"capabilities": {
                            "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
                            "definitionProvider": true, "referencesProvider": true, "hoverProvider": true,
                            "documentSymbolProvider": true, "workspaceSymbolProvider": true
                        }})),
                        "initialized" => {
                            let _ = tx.send(json!({"jsonrpc": "2.0", "id": "cfg", "method": "workspace/configuration",
                                "params": {"items": [{"section": "foo"}, {"section": "nope"}]}}));
                            if b.ask_apply_edit {
                                let _ = tx.send(json!({"jsonrpc": "2.0", "id": "apply", "method": "workspace/applyEdit",
                                    "params": {"edit": {"changes": {}}}}));
                            }
                        }
                        "textDocument/didOpen" | "textDocument/didChange" => {
                            let uri = params
                                .pointer("/textDocument/uri")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let version = params
                                .pointer("/textDocument/version")
                                .cloned()
                                .unwrap_or(Value::Null);
                            let text = params
                                .pointer("/textDocument/text")
                                .or_else(|| params.pointer("/contentChanges/0/text"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            st.docs.lock().insert(uri.clone(), text.clone());
                            if !b.silent {
                                let tx = tx.clone();
                                let delay = b.publish_delay;
                                tokio::spawn(async move {
                                    tokio::time::sleep(delay).await;
                                    let _ = tx.send(publish(&uri, &version, &text));
                                });
                            }
                        }
                        "textDocument/definition" => {
                            let uri = params
                                .pointer("/textDocument/uri")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let line = st
                                .docs
                                .lock()
                                .get(uri)
                                .and_then(|t| t.lines().position(|l| l.contains("DEF")))
                                .unwrap_or(0);
                            reply(json!([{"uri": uri, "range": range(line, 0, 3)}]))
                        }
                        "shutdown" => reply(Value::Null),
                        "exit" => break 'outer,
                        "test/hang" => {}
                        _ => reply(Value::Null),
                    }
                }
            }
            st.notify.notify_waiters();
        }
    }
    drop(tx);
    let _ = writer.await;
}

fn range(line: usize, a: usize, b: usize) -> Value {
    json!({"start": {"line": line, "character": a}, "end": {"line": line, "character": b}})
}

pub fn publish(uri: &str, version: &Value, text: &str) -> Value {
    let diags: Vec<Value> = text
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let (sev, word) = if let Some(c) = l.find("BAD") {
                (1, c)
            } else {
                (2, l.find("WARN")?)
            };
            Some(json!({"range": range(i, word, word + 3), "severity": sev, "source": "fake",
                "message": format!("{} on this line", if sev == 1 { "bad thing" } else { "iffy thing" })}))
        })
        .collect();
    json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "version": version, "diagnostics": diags}})
}
