//! `lsp`: language-server navigation and renames, offered only in sessions
//! whose directory matched an installed server when they were created.
//!
//! Positions are given the way the model sees code: `path`, a 1-based
//! `line` and a `symbol` (text on that line, `name#2` for its second
//! occurrence). Results are bounded and carry `[path#TAG]` headers and line
//! numbers, so `edit` can act on them directly; lines they show count as
//! seen. `rename` is a write: it is assessed like `edit`, refused in
//! read-only sessions, and applied through the edit machinery (snapshots,
//! tags, diagnostics), all files or none.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::PermissionMode;

use super::files::{commit, file_tag, path_write_risk};
use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str, opt_u64};
use crate::lsp::client::{LspClient, RpcError, units_at, uri};
use crate::lsp::{Diagnostic, LspManager, ServerDef, SessionEnv, Severity};

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOCATIONS: usize = 100;
const MAX_SYMBOLS: usize = 150;
const MAX_DIAGNOSTICS: usize = 40;
const MAX_HOVER: usize = 2000;
const MAX_RENAME_FILES: usize = 200;

pub struct Lsp;

impl Tool for Lsp {
    fn name(&self) -> &'static str {
        "lsp"
    }
    fn description(&self) -> String {
        "Language-server queries: `definition`, `references`, `hover`, `symbols` (outline of `path`, or workspace \
         search by `query`), `diagnostics` (of `path`, or every file), `rename` (to `new_name`, across files). Point \
         at code with `path` + `line` + `symbol` (text on that line; `name#2` = its 2nd occurrence). Results carry \
         `[path#TAG]` and line numbers for `edit`. Prefer it to grep for references and renames."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["definition", "references", "hover", "symbols", "diagnostics", "rename"]},
                "path": {"type": "string"},
                "line": {"type": "integer"},
                "symbol": {"type": "string"},
                "query": {"type": "string"},
                "new_name": {"type": "string"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn concurrent(&self, args: &Value) -> bool {
        opt_str(args, "action") != Some("rename")
    }
    fn summary(&self, args: &Value) -> String {
        let action = opt_str(args, "action").unwrap_or("lsp");
        let what = opt_str(args, "symbol")
            .or(opt_str(args, "query"))
            .unwrap_or("");
        let at = match (opt_str(args, "path"), opt_u64(args, "line")) {
            (Some(p), Some(l)) => format!("{p}:{l}"),
            (Some(p), None) => p.to_string(),
            _ => String::new(),
        };
        let to = opt_str(args, "new_name")
            .map(|n| format!(" → {n}"))
            .unwrap_or_default();
        format!("{action} {what}{to} {at}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let path = opt_str(args, "path").map(|p| ctx.resolve(p));
        if let Some(p) = &path
            && let r @ Risk::Block { .. } = super::path_risk(p, ctx)
        {
            return r;
        }
        let mode = ctx.host.permission(&ctx.session_id);
        if opt_str(args, "action") == Some("rename") {
            if mode == PermissionMode::ReadOnly {
                return Risk::Block {
                    reason: "This session is read-only; the rename was not performed.".into(),
                };
            }
            if let Some(p) = &path {
                let r = path_write_risk(p, ctx);
                if r != Risk::None {
                    return r;
                }
            }
            if mode == PermissionMode::Ask {
                return Risk::Ask {
                    reason: "`lsp rename` changes files (and may start a language server).".into(),
                    key: "tool:edit".into(),
                };
            }
        }
        // In ask mode the first start of a server (it can run the project's
        // build scripts) is approved once per project.
        if mode == PermissionMode::Ask
            && let Some(lsp) = ctx.host.lsp()
        {
            let wanted: Vec<(ServerDef, PathBuf)> = match &path {
                Some(p) => lsp.route(p, &ctx.cwd).into_iter().collect(),
                None => lsp.workspace_servers(&ctx.cwd),
            };
            if let Some((def, root)) = wanted
                .into_iter()
                .find(|(d, r)| lsp.running(&d.name, r).is_none())
            {
                return Risk::Ask {
                    reason: format!(
                        "starts the {} language server for {} (language servers can run the project's build scripts)",
                        def.name,
                        root.display()
                    ),
                    key: format!("lsp:{}:{}", def.name, root.display()),
                };
            }
        }
        Risk::None
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let action = match arg_str(&args, "action") {
                Ok(a) => a.to_string(),
                Err(e) => return e,
            };
            let Some((lsp, mut env)) = super::lsp_env(ctx) else {
                return ToolOutput::error(
                    "Language servers are turned off (`[lsp] enabled = false`).",
                );
            };
            let mode = ctx.host.permission(&ctx.session_id);
            // In ask mode `assess` asked before any start, so reaching here
            // means it was approved.
            env.may_start = env.may_start || mode == PermissionMode::Ask;
            let q = Query {
                ctx,
                lsp: &lsp,
                env: &env,
                args: &args,
            };
            let result = tokio::select! {
                r = q.run(&action, mode) => r,
                _ = ctx.cancel.cancelled() => Err("Cancelled.".to_string()),
            };
            let mut out = match result {
                Ok(text) => {
                    let clean = action == "diagnostics"
                        && args.get("path").and_then(Value::as_str).is_some_and(|p| {
                            p != "*"
                                && text
                                    == format!(
                                        "{}: no errors or warnings.",
                                        ctx.display(&ctx.resolve(p))
                                    )
                        });
                    let (text, _) = super::fit_output(&text, ctx, "lsp output");
                    let out = ToolOutput::text(text);
                    if clean {
                        out.with_details(json!({"checked": "clean"}))
                    } else {
                        out
                    }
                }
                Err(e) => ToolOutput::error(e),
            };
            super::attach_late_diagnostics(ctx, &mut out);
            out
        })
    }
}

struct Query<'a> {
    ctx: &'a ToolCtx,
    lsp: &'a Arc<LspManager>,
    env: &'a SessionEnv,
    args: &'a Value,
}

/// A position the model pointed at.
struct Spot {
    client: Arc<LspClient>,
    path: PathBuf,
    text: String,
    /// 0-based line and LSP column.
    line: u32,
    col: u32,
    symbol: String,
}

impl Query<'_> {
    async fn run(&self, action: &str, mode: PermissionMode) -> Result<String, String> {
        match action {
            "definition" => self.definition().await,
            "references" => self.references().await,
            "hover" => self.hover().await,
            "rename" => {
                if mode == PermissionMode::ReadOnly {
                    return Err("This session is read-only; the rename was not performed.".into());
                }
                self.rename(mode).await
            }
            "symbols" => match opt_str(self.args, "path") {
                Some(p) => self.outline(&self.ctx.resolve(p)).await,
                None => self.workspace_symbols().await,
            },
            "diagnostics" => match opt_str(self.args, "path").filter(|p| *p != "*") {
                Some(p) => self.file_diagnostics(&self.ctx.resolve(p)).await,
                None => self.workspace_diagnostics().await,
            },
            other => Err(format!(
                "Unknown action `{other}`: use definition, references, hover, symbols, diagnostics or rename."
            )),
        }
    }

    /// The running (or started) server for `path`, with the file's current
    /// text synced to it.
    async fn open(&self, path: &Path) -> Result<(Arc<LspClient>, ServerDef, String), String> {
        let display = self.ctx.display(path);
        let Some((def, root)) = self.lsp.route(path, &self.ctx.cwd) else {
            let found: Vec<String> = self
                .lsp
                .detect(&self.ctx.cwd)
                .iter()
                .map(|d| d.name.clone())
                .collect();
            return Err(if found.is_empty() {
                format!("No language server handles {display}.")
            } else {
                format!(
                    "No language server handles {display} (available here: {}).",
                    found.join(", ")
                )
            });
        };
        let client = self
            .lsp
            .client(&def, &root, self.env, self.env.may_start)
            .await
            .map_err(|e| format!("{}: {e}", def.name))?;
        let text = super::files::authorized_text(path, self.ctx)
            .await
            .map_err(|e| format!("Cannot read {display}: {e}"))?;
        client
            .sync(path, &text, &def.language_id(path))
            .map_err(|e| e.message)?;
        Ok((client, def, text))
    }

    async fn spot(&self) -> Result<Spot, String> {
        let path = self
            .ctx
            .resolve(opt_str(self.args, "path").ok_or("`path` is required for this action.")?);
        let line = opt_u64(self.args, "line")
            .ok_or("`line` (1-based) is required for this action.")? as usize;
        let (client, _, text) = self.open(&path).await?;
        let total = text.lines().count();
        let line_text = text.lines().nth(line.max(1) - 1).ok_or_else(|| {
            format!(
                "Line {line} is past the end ({} has {total} lines).",
                self.ctx.display(&path)
            )
        })?;
        let (byte, symbol) = match opt_str(self.args, "symbol") {
            Some(s) => {
                let (name, nth) = match s.rsplit_once('#') {
                    Some((n, k)) if !n.is_empty() && k.parse::<usize>().is_ok() => {
                        (n, k.parse::<usize>().unwrap_or(1).max(1))
                    }
                    _ => (s, 1),
                };
                let at = find_symbol(line_text, name, nth).ok_or_else(|| {
                    format!(
                        "`{name}` (occurrence {nth}) isn't on line {line}: {}",
                        crate::util::truncate(line_text.trim(), 200)
                    )
                })?;
                // `int add` or `self.size`: the identifier meant, not the phrase.
                let (inner, ident) = focus(name);
                (at + inner, ident.to_string())
            }
            None => {
                let at = line_text
                    .char_indices()
                    .find(|(_, c)| c.is_alphanumeric() || *c == '_')
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                let word: String = line_text[at..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (at, word)
            }
        };
        let col = units_at(line_text, byte, client.utf8());
        Ok(Spot {
            client,
            path,
            text,
            line: (line.max(1) - 1) as u32,
            col,
            symbol,
        })
    }

    /// A request, retried while the server says the content is still
    /// changing under it.
    async fn ask(&self, client: &LspClient, method: &str, params: Value) -> Result<Value, String> {
        let mut tries = 0;
        loop {
            match client.request(method, params.clone(), TIMEOUT).await {
                Err(RpcError { code, .. }) if code == RpcError::CONTENT_MODIFIED && tries < 3 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                Err(e) => return Err(format!("{}: {e}", client.server)),
                Ok(v) => return Ok(v),
            }
        }
    }

    /// `ask` at a spot; errors say where the request pointed.
    async fn ask_at(&self, spot: &Spot, method: &str, params: Value) -> Result<Value, String> {
        self.ask(&spot.client, method, params).await.map_err(|e| {
            format!(
                "{e} (asked about `{}` on line {}; `symbol` should be that identifier)",
                spot.symbol,
                spot.line + 1
            )
        })
    }

    fn position(spot: &Spot) -> Result<Value, String> {
        let u = uri(&crate::tools::canonical(&spot.path)).map_err(|e| e.message)?;
        Ok(
            json!({"textDocument": {"uri": u}, "position": {"line": spot.line, "character": spot.col}}),
        )
    }

    fn busy_note(client: &LspClient) -> String {
        if client.busy() {
            format!(
                "\n({} is still indexing; results may be incomplete. Retry shortly.)",
                client.server
            )
        } else {
            String::new()
        }
    }

    async fn definition(&self) -> Result<String, String> {
        let spot = self.spot().await?;
        let r = self
            .ask_at(&spot, "textDocument/definition", Self::position(&spot)?)
            .await?;
        let locs = locations(&r);
        if locs.is_empty() {
            return Ok(format!(
                "No definition found for `{}`.{}",
                spot.symbol,
                Self::busy_note(&spot.client)
            ));
        }
        let header = format!("Definition of `{}`:", spot.symbol);
        Ok(format!(
            "{}{}",
            self.render_locations(&header, locs, false).await,
            Self::busy_note(&spot.client)
        ))
    }

    async fn references(&self) -> Result<String, String> {
        let spot = self.spot().await?;
        let mut params = Self::position(&spot)?;
        params["context"] = json!({"includeDeclaration": true});
        let r = self
            .ask_at(&spot, "textDocument/references", params)
            .await?;
        let locs = locations(&r);
        if locs.is_empty() {
            return Ok(format!(
                "No references found for `{}`.{}",
                spot.symbol,
                Self::busy_note(&spot.client)
            ));
        }
        let files: std::collections::HashSet<&PathBuf> = locs.iter().map(|(p, _)| p).collect();
        let header = format!(
            "{} references to `{}` in {} files:",
            locs.len(),
            spot.symbol,
            files.len()
        );
        Ok(format!(
            "{}{}",
            self.render_locations(&header, locs, true).await,
            Self::busy_note(&spot.client)
        ))
    }

    async fn hover(&self) -> Result<String, String> {
        let spot = self.spot().await?;
        let r = self
            .ask_at(&spot, "textDocument/hover", Self::position(&spot)?)
            .await?;
        let text = hover_text(r.get("contents").unwrap_or(&Value::Null));
        if text.trim().is_empty() {
            return Ok(format!(
                "No hover information for `{}`.{}",
                spot.symbol,
                Self::busy_note(&spot.client)
            ));
        }
        let text = text.trim();
        Ok(if text.len() > MAX_HOVER {
            format!(
                "{}… [+{} chars]",
                &text[..crate::util::floor_char(text, MAX_HOVER)],
                text.len() - MAX_HOVER
            )
        } else {
            text.to_string()
        })
    }

    /// `[path#TAG]` groups of `N:line` rows, like grep output.
    async fn render_locations(
        &self,
        header: &str,
        mut locs: Vec<(PathBuf, u32)>,
        sort: bool,
    ) -> String {
        if sort {
            locs.sort();
        }
        locs.dedup();
        let total = locs.len();
        let mut groups: Vec<(PathBuf, Vec<u32>)> = Vec::new();
        for (p, l) in locs {
            match groups.iter_mut().find(|(g, _)| *g == p) {
                Some((_, v)) => v.push(l),
                None => groups.push((p, vec![l])),
            }
        }
        let protected = self.ctx.host.protected_paths();
        let mut out = format!("{header}\n");
        let mut shown = 0;
        let mut files_left = groups.len();
        for (path, lines) in &groups {
            if shown >= MAX_LOCATIONS {
                break;
            }
            files_left -= 1;
            let display = self.ctx.display(path);
            let hidden = protected.covers(path) || super::credential_file(path);
            let text = if hidden {
                None
            } else {
                super::files::authorized_text(path, self.ctx)
                    .await
                    .ok()
                    .filter(|t| t.len() <= 16 * 1024 * 1024)
            };
            let Some(text) = text else {
                let nums: Vec<String> = lines.iter().map(|l| (l + 1).to_string()).collect();
                out.push_str(&format!("{display}: lines {}\n", nums.join(", ")));
                shown += lines.len();
                continue;
            };
            let tag = file_tag(&text);
            let all: Vec<&str> = text.lines().collect();
            out.push_str(&format!("[{display}#{tag}]\n"));
            let mut seen = Vec::new();
            for l in lines {
                if shown >= MAX_LOCATIONS {
                    break;
                }
                let n = *l as usize + 1;
                let row = all.get(n - 1).copied().unwrap_or("");
                out.push_str(&format!("{n}:{}\n", crate::util::truncate(row, 300)));
                seen.push((n, n));
                shown += 1;
            }
            if let Some(cache) = self.ctx.host.file_cache() {
                cache.saw(&self.ctx.session_id, path, &tag, Some(&text), seen);
            }
        }
        if shown < total {
            out.push_str(&format!(
                "…and {} more{}\n",
                total - shown,
                if files_left > 0 {
                    format!(" (in {files_left} more files)")
                } else {
                    String::new()
                }
            ));
        }
        out.trim_end().to_string()
    }

    async fn outline(&self, path: &Path) -> Result<String, String> {
        let (client, _, text) = self.open(path).await?;
        let u = uri(&crate::tools::canonical(path)).map_err(|e| e.message)?;
        let r = self
            .ask(
                &client,
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": u}}),
            )
            .await?;
        let mut rows = Vec::new();
        for item in r.as_array().into_iter().flatten() {
            outline_rows(item, 0, &mut rows);
        }
        let display = self.ctx.display(path);
        let tag = file_tag(&text);
        if rows.is_empty() {
            return Ok(format!(
                "[{display}#{tag}] no symbols.{}",
                Self::busy_note(&client)
            ));
        }
        let total = rows.len();
        let mut out = format!("[{display}#{tag}] {total} symbols\n");
        for r in rows.iter().take(MAX_SYMBOLS) {
            out.push_str(r);
            out.push('\n');
        }
        if total > MAX_SYMBOLS {
            out.push_str(&format!("…and {} more\n", total - MAX_SYMBOLS));
        }
        Ok(format!("{}{}", out.trim_end(), Self::busy_note(&client)))
    }

    /// Servers for questions about the whole project (started if allowed).
    async fn workspace_clients(&self) -> Vec<Arc<LspClient>> {
        let mut clients = self.lsp.running_for(&self.ctx.cwd);
        for (def, root) in self.lsp.workspace_servers(&self.ctx.cwd) {
            if clients
                .iter()
                .any(|c| c.server == def.name && c.root == root)
            {
                continue;
            }
            if let Ok(c) = self
                .lsp
                .client(&def, &root, self.env, self.env.may_start)
                .await
            {
                clients.push(c);
            }
        }
        clients
    }

    async fn workspace_symbols(&self) -> Result<String, String> {
        let query = opt_str(self.args, "query")
            .ok_or("Give `path` for a file outline or `query` to search the workspace.")?;
        let clients = self.workspace_clients().await;
        if clients.is_empty() {
            return Err("No language server is available for this directory.".into());
        }
        let mut found: Vec<(bool, PathBuf, u32, String)> = Vec::new();
        let mut notes = String::new();
        for c in &clients {
            let r = match self
                .ask(c, "workspace/symbol", json!({"query": query}))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    notes.push_str(&format!("\n({e})"));
                    continue;
                }
            };
            for s in r.as_array().into_iter().flatten() {
                let Some(name) = s.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let loc = s.get("location").unwrap_or(&Value::Null);
                let Some(path) = loc
                    .get("uri")
                    .and_then(Value::as_str)
                    .and_then(crate::lsp::client::path_of)
                else {
                    continue;
                };
                let line = loc
                    .pointer("/range/start/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let kind = symbol_kind(s.get("kind").and_then(Value::as_u64).unwrap_or(0));
                let container = s
                    .get("containerName")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .map(|c| format!(" (in {c})"))
                    .unwrap_or_default();
                found.push((
                    name != query,
                    path,
                    line,
                    format!("{kind} {name}{container}"),
                ));
            }
            notes.push_str(&Self::busy_note(c));
        }
        if found.is_empty() {
            return Ok(format!("No symbols match `{query}`.{notes}"));
        }
        // Exact names first, then by file and line.
        found.sort();
        found.dedup();
        let total = found.len();
        let mut by_file: BTreeMap<PathBuf, Vec<(u32, String)>> = BTreeMap::new();
        for (_, p, l, what) in found.into_iter().take(60) {
            by_file.entry(p).or_default().push((l, what));
        }
        let mut out = format!("{total} symbols match `{query}`:\n");
        for (p, rows) in by_file {
            let display = self.ctx.display(&p);
            let tag = super::files::authorized_text(&p, self.ctx)
                .await
                .map(|t| format!("#{}", file_tag(&t)))
                .unwrap_or_default();
            out.push_str(&format!("[{display}{tag}]\n"));
            for (l, what) in rows {
                out.push_str(&format!("{} {what}\n", l + 1));
            }
        }
        if total > 60 {
            out.push_str(&format!("…and {} more; narrow the query\n", total - 60));
        }
        Ok(format!("{}{notes}", out.trim_end()))
    }

    async fn file_diagnostics(&self, path: &Path) -> Result<String, String> {
        let (client, _, text) = self.open(path).await?;
        let doc = client.doc(path).ok_or("the document isn't open")?;
        let fresh = client.published(path).filter(|p| p.seq > doc.synced_gen);
        let published = match fresh {
            Some(p) => Some(p),
            None => {
                client
                    .wait_publish(
                        path,
                        doc.synced_gen,
                        Some(doc.version),
                        Instant::now() + Duration::from_secs(5),
                    )
                    .await
            }
        };
        let display = self.ctx.display(path);
        let tag = file_tag(&text);
        let Some(p) = published else {
            return Ok(format!(
                "[{display}#{tag}] {} hasn't reported on this file yet.{}",
                client.server,
                Self::busy_note(&client)
            ));
        };
        let mut items: Vec<Diagnostic> = p.items.iter().filter(|d| d.relevant()).cloned().collect();
        items.sort_by_key(|d| (d.severity, d.line, d.col));
        if items.is_empty() {
            return Ok(format!(
                "[{display}#{tag}] no errors or warnings.{}",
                Self::busy_note(&client)
            ));
        }
        let mut out = format!("[{display}#{tag}] {}\n", counts(&items));
        for d in items.iter().take(MAX_DIAGNOSTICS) {
            out.push_str(&d.render(&display));
            out.push('\n');
        }
        if items.len() > MAX_DIAGNOSTICS {
            out.push_str(&format!("…and {} more\n", items.len() - MAX_DIAGNOSTICS));
        }
        Ok(format!("{}{}", out.trim_end(), Self::busy_note(&client)))
    }

    async fn workspace_diagnostics(&self) -> Result<String, String> {
        let fresh_start = self.lsp.running_for(&self.ctx.cwd).is_empty();
        let clients = self.workspace_clients().await;
        if clients.is_empty() {
            return Err("No language server is available for this directory.".into());
        }
        if fresh_start {
            // Just started: give the servers a moment to report.
            let until = Instant::now() + Duration::from_secs(3);
            for c in &clients {
                c.wait_any(0, until).await;
            }
        }
        let mut by_file: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
        let mut servers = Vec::new();
        let mut notes = String::new();
        for c in &clients {
            servers.push(c.server.clone());
            notes.push_str(&Self::busy_note(c));
            for (p, published) in c.all_published() {
                if !p.starts_with(&self.ctx.cwd) && !p.starts_with(&c.root) {
                    continue;
                }
                let items: Vec<Diagnostic> = published
                    .items
                    .iter()
                    .filter(|d| d.relevant())
                    .cloned()
                    .collect();
                if !items.is_empty() {
                    by_file.entry(p).or_default().extend(items);
                }
            }
        }
        if by_file.is_empty() {
            return Ok(format!(
                "No errors or warnings reported ({}).{notes}",
                servers.join(", ")
            ));
        }
        let all: Vec<Diagnostic> = by_file.values().flatten().cloned().collect();
        let mut rows: Vec<(Severity, String, u32, String)> = Vec::new();
        for (p, items) in &by_file {
            let display = self.ctx.display(p);
            for d in items {
                rows.push((d.severity, display.clone(), d.line, d.render(&display)));
            }
        }
        rows.sort();
        let mut out = format!(
            "{} in {} files ({}):\n",
            counts(&all),
            by_file.len(),
            servers.join(", ")
        );
        for (_, _, _, r) in rows.iter().take(MAX_DIAGNOSTICS) {
            out.push_str(r);
            out.push('\n');
        }
        if rows.len() > MAX_DIAGNOSTICS {
            out.push_str(&format!("…and {} more\n", rows.len() - MAX_DIAGNOSTICS));
        }
        Ok(format!("{}{notes}", out.trim_end()))
    }

    async fn rename(&self, mode: PermissionMode) -> Result<String, String> {
        let new_name = opt_str(self.args, "new_name")
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or("`new_name` is required for rename.")?
            .to_string();
        let spot = self.spot().await?;
        let client = spot.client.clone();
        if !client.supports("renameProvider") {
            return Err(format!("{} can't rename.", client.server));
        }
        // The server must see what's on disk, or its offsets won't match.
        for (p, doc) in client.open_docs() {
            if let Ok(disk) = super::files::authorized_text(&p, self.ctx).await
                && *disk != *doc.text
                && let Some((def, _)) = self.lsp.route(&p, &self.ctx.cwd)
            {
                let _ = client.sync(&p, &disk, &def.language_id(&p));
            }
        }
        let mut params = Self::position(&spot)?;
        params["newName"] = json!(new_name);
        let edit = self.ask_at(&spot, "textDocument/rename", params).await?;
        let per_file = workspace_edits(&edit)?;
        if per_file.is_empty() {
            return Err(format!(
                "{} proposed no changes for renaming `{}`.",
                client.server, spot.symbol
            ));
        }
        if per_file.len() > MAX_RENAME_FILES {
            return Err(format!(
                "Renaming `{}` would change {} files (more than {MAX_RENAME_FILES}); not applied.",
                spot.symbol,
                per_file.len()
            ));
        }
        // Compute every file first: all or nothing.
        let mut planned = Vec::new();
        let mut refused = Vec::new();
        for (path, edits) in per_file {
            match path_write_risk(&path, self.ctx) {
                Risk::None => {}
                Risk::Ask { .. } if mode == PermissionMode::Auto => {}
                _ => {
                    refused.push(self.ctx.display(&path));
                    continue;
                }
            }
            let old = if path == spot.path {
                spot.text.clone()
            } else {
                super::files::authorized_text(&path, self.ctx)
                    .await
                    .map_err(|e| format!("Cannot read {}: {e}", self.ctx.display(&path)))?
            };
            let (new, changed) = apply_text_edits(&client, &old, &edits)
                .map_err(|e| format!("{}: {e}", self.ctx.display(&path)))?;
            if new != old {
                planned.push((path, old, new, changed, edits.len()));
            }
        }
        if !refused.is_empty() {
            return Err(format!(
                "Renaming `{}` would also change files outside the working directory or in protected places ({}); nothing was changed. Use `edit` for those.",
                spot.symbol,
                refused.join(", ")
            ));
        }
        let mut written: Vec<(PathBuf, String)> = Vec::new();
        let mut summary = Vec::new();
        let total_edits: usize = planned.iter().map(|p| p.4).sum();
        for (path, old, new, changed, count) in &planned {
            let display = self.ctx.display(path);
            match commit(self.ctx, path, &display, Some(old), new, false).await {
                Ok(c) => {
                    written.push((path.clone(), old.clone()));
                    let lines: Vec<&str> = new.lines().collect();
                    let mut block = format!(
                        "[{display}#{}] {count} edit{}\n",
                        c.tag,
                        if *count == 1 { "" } else { "s" }
                    );
                    for n in changed.iter().take(5) {
                        let row = lines.get(*n - 1).copied().unwrap_or("");
                        block.push_str(&format!("{n}:{}\n", crate::util::truncate(row, 200)));
                    }
                    if changed.len() > 5 {
                        block.push_str(&format!("…and {} more lines\n", changed.len() - 5));
                    }
                    summary.push(block);
                }
                Err(e) => {
                    // Put back what was already written.
                    for (p, o) in &written {
                        if let Some((_, _, new, _, _)) = planned.iter().find(|(path, ..)| path == p)
                        {
                            let _ = super::files::prepare_destination(p, self.ctx)
                                .and_then(|d| d.write(Some(new), o).map_err(|e| e.to_string()));
                        }
                    }
                    return Err(format!("{e}. The rename was rolled back."));
                }
            }
        }
        let mut out = format!(
            "Renamed `{}` to `{new_name}`: {total_edits} edits in {} files.\n{}",
            spot.symbol,
            planned.len(),
            summary.join("")
        );
        // What the rename broke, per file (servers can reject some renames
        // semantically without saying so up front).
        let checks = planned.iter().map(|(path, old, new, _, _)| {
            self.lsp.after_write(self.env, path, Some(old), new, false)
        });
        let mut seen = std::collections::HashSet::new();
        for block in futures::future::join_all(checks)
            .await
            .into_iter()
            .flatten()
        {
            for line in block.lines() {
                if seen.insert(line.to_string()) {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        Ok(out.trim_end().to_string())
    }
}

/// Byte offset of occurrence `nth` (1-based) of `name` on `line`,
/// preferring whole-word matches.
fn find_symbol(line: &str, name: &str, nth: usize) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let all: Vec<usize> = line.match_indices(name).map(|(i, _)| i).collect();
    let words: Vec<usize> = all
        .iter()
        .copied()
        .filter(|&i| {
            let before = line[..i].chars().next_back();
            let after = line[i + name.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
        .collect();
    let pick = if words.len() >= nth { &words } else { &all };
    pick.get(nth - 1).copied()
}

/// Words that introduce a declaration rather than name the thing.
const DECL_WORDS: &[&str] = &[
    "fn",
    "def",
    "func",
    "function",
    "let",
    "const",
    "var",
    "val",
    "mut",
    "pub",
    "static",
    "extern",
    "inline",
    "async",
    "await",
    "return",
    "new",
    "int",
    "void",
    "char",
    "long",
    "short",
    "float",
    "double",
    "bool",
    "unsigned",
    "signed",
    "auto",
    "class",
    "struct",
    "enum",
    "union",
    "type",
    "interface",
    "trait",
    "impl",
    "mod",
    "module",
    "package",
    "import",
    "from",
    "use",
    "export",
    "public",
    "private",
    "protected",
    "final",
    "override",
    "virtual",
    "self",
    "this",
    "super",
];

/// Where, inside a `symbol` the model gave, the identifier it means is, and
/// that identifier: the text itself when it is one identifier; else the
/// name just before a `(`, else the last word that isn't a declaration
/// keyword (`int add` → `add`, `self.size` → `size`, `add(2, 3)` → `add`).
fn focus(symbol: &str) -> (usize, &str) {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut words: Vec<(usize, &str)> = Vec::new();
    let mut start = None;
    for (i, c) in symbol
        .char_indices()
        .chain(std::iter::once((symbol.len(), ' ')))
    {
        match (start, ident(c)) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                words.push((s, &symbol[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    words.retain(|(_, w)| !w.chars().next().is_some_and(|c| c.is_ascii_digit()));
    match words.as_slice() {
        [] => (0, symbol),
        [one] => *one,
        _ => {
            let named = |w: &&(usize, &str)| !DECL_WORDS.contains(&w.1);
            let before_paren = symbol
                .find('(')
                .and_then(|p| words.iter().rfind(|(s, w)| s + w.len() <= p).filter(named));
            *before_paren
                .or_else(|| words.iter().rfind(named))
                .unwrap_or(words.last().expect("not empty"))
        }
    }
}

/// (file, 0-based line) from any of the shapes `definition`/`references`
/// return.
fn locations(v: &Value) -> Vec<(PathBuf, u32)> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|l| {
            let (u, range) = match l.get("targetUri") {
                Some(u) => (u, l.get("targetSelectionRange").or(l.get("targetRange"))?),
                None => (l.get("uri")?, l.get("range")?),
            };
            let path = crate::lsp::client::path_of(u.as_str()?)?;
            let line = range.pointer("/start/line")?.as_u64()? as u32;
            Some((path, line))
        })
        .collect()
}

fn hover_text(contents: &Value) -> String {
    match contents {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(hover_text).collect::<Vec<_>>().join("\n"),
        Value::Object(o) => o
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn symbol_kind(k: u64) -> &'static str {
    match k {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "variant",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_param",
        _ => "symbol",
    }
}

/// `N-M kind name` rows, children indented, from either symbol shape.
fn outline_rows(item: &Value, depth: usize, out: &mut Vec<String>) {
    let Some(name) = item.get("name").and_then(Value::as_str) else {
        return;
    };
    let range = item
        .get("range")
        .or_else(|| item.pointer("/location/range"));
    let line = |key: &str| {
        range
            .and_then(|r| r.pointer(key))
            .and_then(Value::as_u64)
            .map(|n| n + 1)
            .unwrap_or(0)
    };
    let (a, b) = (line("/start/line"), line("/end/line"));
    let span = if a == b {
        a.to_string()
    } else {
        format!("{a}-{b}")
    };
    let kind = symbol_kind(item.get("kind").and_then(Value::as_u64).unwrap_or(0));
    out.push(format!("{}{span} {kind} {name}", "  ".repeat(depth)));
    for child in item
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        outline_rows(child, depth + 1, out);
    }
}

fn counts(items: &[Diagnostic]) -> String {
    let e = items
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let w = items.len() - e;
    let n = |k: usize, what: &str| {
        if k == 1 {
            format!("1 {what}")
        } else {
            format!("{k} {what}s")
        }
    };
    match (e, w) {
        (0, w) => n(w, "warning"),
        (e, 0) => n(e, "error"),
        (e, w) => format!("{}, {}", n(e, "error"), n(w, "warning")),
    }
}

/// Text edits per file from a `WorkspaceEdit`; file creation, renaming and
/// deletion are refused (Tessera doesn't let a server move files).
fn workspace_edits(edit: &Value) -> Result<Vec<(PathBuf, Vec<Value>)>, String> {
    let mut out: Vec<(PathBuf, Vec<Value>)> = Vec::new();
    let mut push = |uri: &str, edits: &[Value]| -> Result<(), String> {
        let path = crate::lsp::client::path_of(uri)
            .ok_or_else(|| format!("unsupported location {uri}"))?;
        match out.iter_mut().find(|(p, _)| *p == path) {
            Some((_, v)) => v.extend(edits.iter().cloned()),
            None => out.push((path, edits.to_vec())),
        }
        Ok(())
    };
    if let Some(changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for c in changes {
            if c.get("kind").is_some() {
                return Err(format!(
                    "the server wants to {} a file as part of this rename; not applied. Rename files with `bash` (e.g. `git mv`).",
                    c.get("kind").and_then(Value::as_str).unwrap_or("change")
                ));
            }
            let uri = c
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .ok_or("malformed rename edit")?;
            push(
                uri,
                c.get("edits")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )?;
        }
    } else if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            push(uri, edits.as_array().map(Vec::as_slice).unwrap_or(&[]))?;
        }
    }
    Ok(out)
}

/// Apply LSP text edits to `text`; returns the new text and the 1-based
/// lines (in the new text) where edits landed.
fn apply_text_edits(
    client: &LspClient,
    text: &str,
    edits: &[Value],
) -> Result<(String, Vec<usize>), String> {
    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            let pos = |key: &str| -> Option<usize> {
                let p = e.pointer(key)?;
                Some(client.offset(
                    text,
                    p.get("line")?.as_u64()? as u32,
                    p.get("character")?.as_u64()? as u32,
                ))
            };
            let (a, b) = (pos("/range/start"), pos("/range/end"));
            let new = e.get("newText").and_then(Value::as_str);
            match (a, b, new) {
                (Some(a), Some(b), Some(n)) if a <= b => Ok((a, b, n.to_string())),
                _ => Err("malformed text edit".to_string()),
            }
        })
        .collect::<Result<_, _>>()?;
    spans.sort_by_key(|s| (s.0, s.1));
    if spans.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err("overlapping edits".into());
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut lines = Vec::new();
    let mut newlines = 0;
    for (a, b, new) in &spans {
        let kept = &text[last..*a];
        newlines += kept.matches('\n').count();
        out.push_str(kept);
        if lines.last() != Some(&(newlines + 1)) {
            lines.push(newlines + 1);
        }
        newlines += new.matches('\n').count();
        out.push_str(new);
        last = *b;
    }
    out.push_str(&text[last..]);
    Ok((out, lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_prefer_whole_words_and_count_occurrences() {
        let line = "let foo = foobar(foo) + foo;";
        assert_eq!(find_symbol(line, "foo", 1), Some(4));
        assert_eq!(find_symbol(line, "foo", 2), Some(17));
        assert_eq!(find_symbol(line, "foo", 3), Some(24));
        // Falls back to substrings when there aren't enough whole words.
        assert_eq!(find_symbol(line, "bar", 1), Some(13));
        assert_eq!(find_symbol(line, "nope", 1), None);
    }

    #[test]
    fn phrases_focus_on_the_identifier_meant() {
        assert_eq!(focus("add"), (0, "add"));
        assert_eq!(focus("int add"), (4, "add"));
        assert_eq!(focus("pub fn run_all"), (7, "run_all"));
        assert_eq!(focus("self.size"), (5, "size"));
        assert_eq!(focus("add(2, 3)"), (0, "add"));
        assert_eq!(focus("fn main()"), (3, "main"));
        assert_eq!(focus("self"), (0, "self"));
        assert_eq!(focus("->"), (0, "->"));
    }

    #[test]
    fn location_shapes_and_workspace_edits() {
        let loc = json!({"uri": "file:///a/b.rs", "range": {"start": {"line": 3, "character": 0}}});
        let link = json!([{"targetUri": "file:///a/c.rs", "targetRange": {"start": {"line": 9}}, "targetSelectionRange": {"start": {"line": 10}}}]);
        assert_eq!(locations(&loc), vec![(PathBuf::from("/a/b.rs"), 3)]);
        assert_eq!(locations(&link), vec![(PathBuf::from("/a/c.rs"), 10)]);
        assert!(locations(&Value::Null).is_empty());
        let e = json!({"changes": {"file:///a/b.rs": [{"range": {}, "newText": "x"}]}});
        assert_eq!(workspace_edits(&e).unwrap()[0].0, PathBuf::from("/a/b.rs"));
        let moves = json!({"documentChanges": [{"kind": "rename", "oldUri": "file:///a", "newUri": "file:///b"}]});
        assert!(
            workspace_edits(&moves)
                .unwrap_err()
                .contains("rename a file")
        );
        assert_eq!(
            hover_text(&json!([{"language": "rust", "value": "fn x()"}, "docs"])),
            "fn x()\ndocs"
        );
    }

    #[test]
    fn outline_rows_nest() {
        let s = json!({"name": "A", "kind": 23, "range": {"start": {"line": 0}, "end": {"line": 9}},
            "children": [{"name": "f", "kind": 6, "range": {"start": {"line": 2}, "end": {"line": 2}}}]});
        let mut rows = Vec::new();
        outline_rows(&s, 0, &mut rows);
        assert_eq!(rows, vec!["1-10 struct A", "  3 method f"]);
    }
}
