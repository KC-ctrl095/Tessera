//! Model Context Protocol client (stdio and streamable HTTP).
//!
//! Each configured server's tools are registered as `mcp__<server>__<tool>`.
//! Tools that declare `readOnlyHint` run freely; everything else asks for
//! approval in the default permission mode (once per session per tool).
//!
//! Their descriptions are capped like the built-in tools' (600 characters).
//! Where all of a chat's MCP tools would take more than a tenth of the
//! model's context window up front, the largest servers' tools wait until
//! first used ([`deferred`]): the prompt names each such server and its
//! tools in one line, and calling one (or reading `docs` topic "tools")
//! turns that server's tools on from the next step, one prompt-cache miss.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tessera_proto::ContentPart;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::config::McpServerConfig;
use crate::llm::ToolSpec;
use crate::tools::{Risk, Tool, ToolCtx, ToolKind, ToolOutput};

const PROTOCOL: &str = "2025-06-18";

/// Most characters of an MCP tool's description (the built-in tools' cap).
pub const MAX_DESCRIPTION: usize = 600;

/// Share of the context window a chat's MCP tools may take up front; above
/// it, the largest servers' tools wait until first used (Claude Code's tool
/// search starts at about the same share).
pub const UPFRONT_SHARE: f64 = 0.10;

/// Tool names a deferred server's prompt line lists (the rest: `docs`
/// topic "tools").
const LINE_NAMES: usize = 40;

/// MCP servers whose tools wait until first used, as a session froze them
/// with its prompt (see [`deferred`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Later {
    /// Servers whose tools are off until one of them is called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off: Vec<String>,
    /// Servers turned on since (each one prompt-cache miss), kept on when
    /// the prompt is built again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
}

/// The configured server a registered tool name belongs to (the longest
/// `mcp__<server>__` prefix that matches).
pub fn server_of<'a>(tool: &str, servers: &'a [McpServerConfig]) -> Option<&'a str> {
    servers
        .iter()
        .map(|s| (s.name.as_str(), format!("mcp__{}__", sanitize(&s.name))))
        .filter(|(_, prefix)| tool.starts_with(prefix.as_str()))
        .max_by_key(|(_, prefix)| prefix.len())
        .map(|(name, _)| name)
}

/// Servers whose tools (`specs`: the chat's MCP tools) wait until first
/// used on a model with a `window`-token context: `defer = true` ones
/// always, `defer = false` ones never, the others largest first while what
/// stays up front takes more than [`UPFRONT_SHARE`] of the window, except
/// those too small for waiting to pay (under a tenth of that share).
pub fn deferred(servers: &[McpServerConfig], specs: &[ToolSpec], window: u64) -> Vec<String> {
    let cost = |s: &McpServerConfig| -> u64 {
        specs
            .iter()
            .filter(|t| server_of(&t.name, servers) == Some(s.name.as_str()))
            .map(|t| crate::context::estimate_request("", std::slice::from_ref(t), &[]))
            .sum()
    };
    let mut sized: Vec<(&McpServerConfig, u64)> = servers
        .iter()
        .map(|s| (s, cost(s)))
        .filter(|(_, c)| *c > 0)
        .collect();
    let mut out: Vec<String> = sized
        .iter()
        .filter(|(s, _)| s.defer == Some(true))
        .map(|(s, _)| s.name.clone())
        .collect();
    let mut upfront: u64 = sized
        .iter()
        .filter(|(s, _)| s.defer != Some(true))
        .map(|(_, c)| c)
        .sum();
    let allowed = (window as f64 * UPFRONT_SHARE) as u64;
    sized.retain(|(s, c)| s.defer.is_none() && *c >= allowed / 10);
    sized.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.name.cmp(&b.0.name)));
    for (s, c) in sized {
        if upfront <= allowed {
            break;
        }
        upfront -= c;
        out.push(s.name.clone());
    }
    out
}

/// The prompt's line about the servers whose tools wait (`off`), naming
/// their tools (`specs`: the chat's MCP tools) so the model knows what to
/// call.
pub fn later_line(
    off: &[String],
    specs: &[ToolSpec],
    servers: &[McpServerConfig],
) -> Option<String> {
    let items: Vec<String> = off
        .iter()
        .filter_map(|name| {
            let prefix = format!("mcp__{}__", sanitize(name));
            let tools: Vec<&str> = specs
                .iter()
                .filter(|t| server_of(&t.name, servers) == Some(name.as_str()))
                .map(|t| t.name.strip_prefix(prefix.as_str()).unwrap_or(&t.name))
                .collect();
            if tools.is_empty() {
                return None;
            }
            let more = tools.len().saturating_sub(LINE_NAMES);
            let mut listed = tools[..tools.len().min(LINE_NAMES)].join(", ");
            if more > 0 {
                listed.push_str(&format!(" and {more} more"));
            }
            Some(format!("`{prefix}*` ({} tools: {listed})", tools.len()))
        })
        .collect();
    (!items.is_empty()).then(|| {
        format!(
            "# MCP tools\nOff until first called (calling one turns its server's tools on from your next step; `docs` topic \"tools\" turns all on): {}.",
            items.join("; ")
        )
    })
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

#[cfg_attr(windows, allow(clippy::large_enum_variant))]
enum Transport {
    Stdio {
        stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
        pending: Pending,
        _child: tokio::process::Child,
    },
    Http {
        http: reqwest::Client,
        url: String,
        headers: Vec<(String, String)>,
        session: Mutex<Option<String>>,
    },
}

pub struct McpClient {
    pub name: String,
    transport: Transport,
    next: AtomicU64,
}

impl McpClient {
    pub async fn connect(cfg: &McpServerConfig, http: reqwest::Client) -> Result<Arc<Self>> {
        let transport = if let Some(cmd) = &cfg.command {
            // Windows finds `npx` only as `npx.cmd`.
            #[cfg(windows)]
            let program = crate::platform::find_program(cmd)
                .map(std::ffi::OsString::from)
                .unwrap_or_else(|| cmd.into());
            #[cfg(not(windows))]
            let program = cmd;
            let mut c = tokio::process::Command::new(program);
            crate::platform::no_window(&mut c);
            c.args(&cfg.args)
                .envs(&cfg.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = c
                .spawn()
                .with_context(|| format!("starting MCP server `{}` ({cmd})", cfg.name))?;
            let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
            let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
            if let Some(stderr) = child.stderr.take() {
                let name = cfg.name.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        tracing::debug!("mcp[{name}] {l}");
                    }
                });
            }
            let pending: Pending = Arc::default();
            let p2 = pending.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    dispatch(&p2, msg);
                }
                for (_, tx) in p2.lock().drain() {
                    let _ = tx.send(Err("MCP server exited".into()));
                }
            });
            Transport::Stdio {
                stdin: tokio::sync::Mutex::new(stdin),
                pending,
                _child: child,
            }
        } else if let Some(url) = &cfg.url {
            Transport::Http {
                http,
                url: url.clone(),
                headers: cfg
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                session: Mutex::new(None),
            }
        } else {
            bail!("MCP server `{}` needs either `command` or `url`", cfg.name);
        };
        let client = Arc::new(Self {
            name: cfg.name.clone(),
            transport,
            next: AtomicU64::new(1),
        });
        client
            .request("initialize", json!({"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "tessera", "version": env!("CARGO_PKG_VERSION")}}))
            .await
            .with_context(|| format!("initializing MCP server `{}`", cfg.name))?;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut s = stdin.lock().await;
                s.write_all(format!("{msg}\n").as_bytes()).await?;
                s.flush().await?;
            }
            Transport::Http { .. } => {
                let _ = self.http_post(&msg).await;
            }
        }
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match &self.transport {
            Transport::Stdio { stdin, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().insert(id, tx);
                {
                    let mut s = stdin.lock().await;
                    s.write_all(format!("{msg}\n").as_bytes()).await?;
                    s.flush().await?;
                }
                match tokio::time::timeout(Duration::from_secs(300), rx).await {
                    Ok(Ok(r)) => r.map_err(|e| anyhow!(e)),
                    Ok(Err(_)) => Err(anyhow!("MCP server `{}` closed", self.name)),
                    Err(_) => {
                        pending.lock().remove(&id);
                        Err(anyhow!("MCP request `{method}` timed out"))
                    }
                }
            }
            Transport::Http { .. } => {
                let v = self.http_post(&msg).await?;
                match v.get("error") {
                    Some(e) => Err(anyhow!(
                        "{}",
                        e.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("MCP error")
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                }
            }
        }
    }

    async fn http_post(&self, msg: &Value) -> Result<Value> {
        let Transport::Http {
            http,
            url,
            headers,
            session,
        } = &self.transport
        else {
            unreachable!()
        };
        let mut rb = http
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL)
            .json(msg);
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        if let Some(s) = session.lock().clone() {
            rb = rb.header("mcp-session-id", s);
        }
        let resp = rb.timeout(Duration::from_secs(300)).send().await?;
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *session.lock() = Some(s.to_string());
        }
        let status = resp.status();
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("MCP HTTP {status}: {}", crate::util::truncate(&body, 300));
        }
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        if ctype.contains("text/event-stream") {
            // Take the last JSON-RPC response carried in the stream.
            let want = msg.get("id").cloned();
            let mut found = Value::Null;
            for line in body.lines() {
                if let Some(d) = line.strip_prefix("data:")
                    && let Ok(v) = serde_json::from_str::<Value>(d.trim())
                    && v.get("id") == want.as_ref()
                {
                    found = v;
                }
            }
            return Ok(found);
        }
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_tools(&self) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.request("tools/list", params).await?;
            tools.extend(
                r.get("tools")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            cursor = r
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() || tools.len() > 500 {
                break;
            }
        }
        Ok(tools)
    }
}

fn dispatch(pending: &Pending, msg: Value) {
    let Some(id) = msg.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Some(tx) = pending.lock().remove(&id) else {
        return;
    };
    let r = match msg.get("error") {
        Some(e) => Err(e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP error")
            .to_string()),
        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
    };
    let _ = tx.send(r);
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub struct McpTool {
    client: Arc<McpClient>,
    remote: String,
    name: &'static str,
    description: String,
    schema: Value,
    read_only: bool,
}

impl McpTool {
    pub fn from_spec(client: Arc<McpClient>, spec: &Value) -> Option<Self> {
        let remote = spec.get("name")?.as_str()?.to_string();
        let mut name = format!("mcp__{}__{}", sanitize(&client.name), sanitize(&remote));
        name.truncate(64);
        let annotations = spec.get("annotations");
        let read_only = annotations
            .and_then(|a| a.get("readOnlyHint"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut schema = spec
            .get("inputSchema")
            .cloned()
            .unwrap_or(json!({"type": "object"}));
        if schema.get("type").is_none() {
            schema["type"] = json!("object");
        }
        let mut description = format!(
            "[{} MCP] {}",
            client.name,
            spec.get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
        );
        if description.chars().count() > MAX_DESCRIPTION {
            description = description.chars().take(MAX_DESCRIPTION - 1).collect();
            description.push('…');
        }
        Some(Self {
            description,
            client,
            remote,
            // Tool names live for the process; MCP tool sets are small and stable.
            name: Box::leak(name.into_boxed_str()),
            schema,
            read_only,
        })
    }
}

impl Tool for McpTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> String {
        self.description.clone()
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    fn kind(&self) -> ToolKind {
        if self.read_only {
            ToolKind::Read
        } else {
            ToolKind::Exec
        }
    }
    fn assess(&self, _args: &Value, _ctx: &ToolCtx) -> Risk {
        if self.read_only {
            Risk::None
        } else {
            Risk::Ask {
                reason: format!(
                    "`{}` is an external MCP tool from `{}`.",
                    self.remote, self.client.name
                ),
                key: format!("mcp:{}", self.name),
            }
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let call = self.client.request(
                "tools/call",
                json!({"name": self.remote, "arguments": args}),
            );
            let r = tokio::select! {
                r = call => r,
                _ = ctx.cancel.cancelled() => return ToolOutput::error("Cancelled."),
            };
            match r {
                Ok(v) => {
                    let mut content = Vec::new();
                    for item in v
                        .get("content")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        match item.get("type").and_then(Value::as_str) {
                            Some("text") => content.push(ContentPart::text(
                                item.get("text").and_then(Value::as_str).unwrap_or(""),
                            )),
                            Some("image") => content.push(ContentPart::Image {
                                mime: item
                                    .get("mimeType")
                                    .and_then(Value::as_str)
                                    .unwrap_or("image/png")
                                    .into(),
                                data: item
                                    .get("data")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .into(),
                            }),
                            Some("resource") => content.push(ContentPart::text(
                                item["resource"]
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .unwrap_or(&item["resource"].to_string()),
                            )),
                            _ => content.push(ContentPart::text(item.to_string())),
                        }
                    }
                    if content.is_empty()
                        && let Some(s) = v.get("structuredContent")
                    {
                        content.push(ContentPart::text(s.to_string()));
                    }
                    ToolOutput {
                        content,
                        is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
                        details: None,
                    }
                }
                Err(e) => ToolOutput::error(format!("MCP call failed: {e}")),
            }
        })
    }
}

/// Connect every enabled server and return its tools. Failures are logged,
/// not fatal: one broken server must not take the harness down.
pub async fn connect_all(
    servers: &[McpServerConfig],
    http: reqwest::Client,
) -> (Vec<Arc<dyn Tool>>, Vec<(String, String)>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut errors = Vec::new();
    let results = futures::future::join_all(servers.iter().filter(|s| s.enabled).map(|s| {
        let http = http.clone();
        async move {
            let r = async {
                let c = tokio::time::timeout(Duration::from_secs(60), McpClient::connect(s, http))
                    .await
                    .map_err(|_| anyhow!("timed out"))??;
                let specs = c.list_tools().await?;
                Ok::<_, anyhow::Error>((c, specs))
            }
            .await;
            (s.name.clone(), r)
        }
    }))
    .await;
    for (name, r) in results {
        match r {
            Ok((client, specs)) => {
                tracing::info!("MCP server `{name}`: {} tools", specs.len());
                tools.extend(
                    specs
                        .iter()
                        .filter_map(|s| McpTool::from_spec(client.clone(), s))
                        .map(|t| Arc::new(t) as Arc<dyn Tool>),
                );
            }
            Err(e) => {
                tracing::warn!("MCP server `{name}` unavailable: {e:#}");
                errors.push((name, format!("{e:#}")));
            }
        }
    }
    (tools, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, defer: Option<bool>) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            command: Some("true".into()),
            args: Vec::new(),
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
            defer,
        }
    }

    fn tools(server: &str, n: usize, chars: usize) -> Vec<ToolSpec> {
        (0..n)
            .map(|i| ToolSpec {
                name: format!("mcp__{server}__tool_{i}"),
                description: "d".repeat(chars),
                parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            })
            .collect()
    }

    #[test]
    fn tools_belong_to_the_longest_matching_server() {
        let servers = [server("git", None), server("git_hub", None)];
        assert_eq!(server_of("mcp__git__log", &servers), Some("git"));
        assert_eq!(server_of("mcp__git_hub__issue", &servers), Some("git_hub"));
        assert_eq!(server_of("mcp__other__x", &servers), None);
        assert_eq!(server_of("read", &servers), None);
    }

    #[test]
    fn the_largest_servers_wait_while_the_rest_takes_too_much_of_the_window() {
        let servers = [
            server("big", None),
            server("mid", None),
            server("tiny", None),
            server("pinned", Some(false)),
            server("lazy", Some(true)),
        ];
        let mut specs = tools("big", 100, 600);
        specs.extend(tools("mid", 20, 600));
        specs.extend(tools("tiny", 1, 50));
        specs.extend(tools("pinned", 2, 50));
        specs.extend(tools("lazy", 1, 50));
        // 100k window: 10k up front. `big` alone is ~16k: it waits; `mid`
        // (~3k) and the small ones fit; `lazy` always waits.
        assert_eq!(deferred(&servers, &specs, 100_000), ["lazy", "big"]);
        // A small window: `mid` waits too; `pinned` never does.
        assert_eq!(deferred(&servers, &specs, 20_000), ["lazy", "big", "mid"]);
        // A big window: only what is set to wait.
        assert_eq!(deferred(&servers, &specs, 1_000_000), ["lazy"]);
        // The prompt names each waiting server and its tools in one line.
        let line = later_line(&["big".into()], &specs, &servers).unwrap();
        assert!(
            line.starts_with("# MCP tools\nOff until first called"),
            "{line}"
        );
        assert!(
            line.contains("`mcp__big__*` (100 tools: tool_0, tool_1,")
                && line.contains("and 60 more"),
            "{line}"
        );
        assert!(
            crate::context::estimate_request(&line, &[], &[]) < 500,
            "{line}"
        );
        assert_eq!(later_line(&[], &specs, &servers), None);
    }
}
