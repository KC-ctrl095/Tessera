//! grep / glob — built in (no ripgrep dependency), gitignore-aware, parallel.

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::files::file_tag;
use super::{Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_bool, opt_str, opt_u64};

/// `root` must be canonical: the protected-location checks compare paths, and
/// a root like `work/../../.local/share` would otherwise slip past them.
fn walker(
    root: &std::path::Path,
    glob: Option<&str>,
    protected: super::Protected,
) -> Result<ignore::WalkParallel, String> {
    let protected = protected.canonical();
    let mut b = ignore::WalkBuilder::new(root);
    b.hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .follow_links(false);
    b.filter_entry(move |e| {
        let n = e.file_name();
        n != ".git"
            && n != "node_modules"
            && n != ".hg"
            && n != ".svn"
            && !(protected.skips(e.path())
                || (e.file_type().is_some_and(|t| !t.is_dir())
                    && (protected.covers(e.path()) || super::credential_file(e.path()))))
    });
    if let Some(g) = glob {
        let mut ob = ignore::overrides::OverrideBuilder::new(root);
        for pat in g.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            ob.add(pat).map_err(|e| format!("bad glob `{pat}`: {e}"))?;
        }
        b.overrides(ob.build().map_err(|e| e.to_string())?);
    }
    Ok(b.threads(
        std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4),
    )
    .build_parallel())
}

pub struct Grep;

struct FileHits {
    path: PathBuf,
    tag: String,
    lines: Vec<(usize, bool, String)>,
    count: usize,
}

impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> String {
        "Search file contents with a regex (Rust syntax), respecting .gitignore. Matches are grouped per file as \
         `[path#TAG]` with `N:` match and `N-` context lines, usable directly by `edit`. `glob` filters files \
         (\"*.rs,!*_test.rs\"); `files_only` lists matching files with counts."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"},
                "glob": {"type": "string"},
                "ignore_case": {"type": "boolean"},
                "context": {"type": "integer", "description": "Lines around each match (max 10)."},
                "files_only": {"type": "boolean"},
                "max": {"type": "integer", "description": "Match lines shown (default 200)."}
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> super::Risk {
        let root = opt_str(args, "path")
            .map(|p| ctx.resolve(p))
            .unwrap_or_else(|| ctx.cwd.clone());
        search_root_risk(&root, ctx)
    }
    fn summary(&self, args: &Value) -> String {
        let p = args.get("pattern").and_then(Value::as_str).unwrap_or("");
        match opt_str(args, "path") {
            Some(path) => format!("{p} in {path}"),
            None => p.to_string(),
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let pattern = match arg_str(&args, "pattern") {
                Ok(p) => p.to_string(),
                Err(e) => return e,
            };
            let re = match regex::RegexBuilder::new(&pattern)
                .case_insensitive(opt_bool(&args, "ignore_case").unwrap_or(false))
                .size_limit(1 << 24)
                .build()
            {
                Ok(r) => r,
                Err(e) => return ToolOutput::error(format!("Invalid regex: {e}")),
            };
            let root = super::canonical(
                &opt_str(&args, "path")
                    .map(|p| ctx.resolve(p))
                    .unwrap_or_else(|| ctx.cwd.clone()),
            );
            let glob = opt_str(&args, "glob").map(str::to_string);
            let context = opt_u64(&args, "context").unwrap_or(0).min(10) as usize;
            let files_only = opt_bool(&args, "files_only").unwrap_or(false);
            let max = opt_u64(&args, "max").unwrap_or(200).clamp(1, 2000) as usize;
            let cancel = ctx.cancel.clone();
            let protected = ctx.host.protected_paths();
            let read_ctx = ctx.clone();

            let result = tokio::task::spawn_blocking(move || {
                let hits: Arc<Mutex<Vec<FileHits>>> = Arc::default();
                let scanned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let walk = walker(&root, glob.as_deref(), protected)?;
                walk.run(|| {
                    let hits = hits.clone();
                    let re = re.clone();
                    let cancel = cancel.clone();
                    let read_ctx = read_ctx.clone();
                    let scanned = scanned.clone();
                    Box::new(move |entry| {
                        if cancel.is_cancelled() {
                            return ignore::WalkState::Quit;
                        }
                        let Ok(entry) = entry else {
                            return ignore::WalkState::Continue;
                        };
                        if !entry.file_type().is_some_and(|t| t.is_file()) {
                            return ignore::WalkState::Continue;
                        }
                        if entry
                            .metadata()
                            .map(|m| m.len() > 8 * 1024 * 1024)
                            .unwrap_or(true)
                        {
                            return ignore::WalkState::Continue;
                        }
                        let Ok(bytes) = super::files::authorized_bytes_sync(
                            entry.path(),
                            &read_ctx,
                            8 * 1024 * 1024,
                        ) else {
                            return ignore::WalkState::Continue;
                        };
                        scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if bytes.iter().take(8192).any(|b| *b == 0) {
                            return ignore::WalkState::Continue;
                        }
                        let text = String::from_utf8_lossy(&bytes);
                        let lines: Vec<&str> = text.lines().collect();
                        let matched: Vec<usize> = lines
                            .iter()
                            .enumerate()
                            .filter(|(_, l)| re.is_match(l))
                            .map(|(i, _)| i)
                            .collect();
                        if matched.is_empty() {
                            return ignore::WalkState::Continue;
                        }
                        let mut shown: Vec<(usize, bool, String)> = Vec::new();
                        let mut last_end = 0usize;
                        for &m in &matched {
                            let from = m.saturating_sub(context).max(last_end);
                            let to = (m + context + 1).min(lines.len());
                            for (i, line) in lines.iter().enumerate().take(to).skip(from) {
                                shown.push((
                                    i + 1,
                                    matched.binary_search(&i).is_ok(),
                                    crate::util::truncate(line, 400),
                                ));
                            }
                            last_end = to;
                        }
                        hits.lock().push(FileHits {
                            path: entry.path().to_path_buf(),
                            tag: file_tag(&text),
                            count: matched.len(),
                            lines: shown,
                        });
                        ignore::WalkState::Continue
                    })
                });
                let mut hits = std::mem::take(&mut *hits.lock());
                hits.sort_by(|a, b| a.path.cmp(&b.path));
                Ok::<_, String>((hits, scanned.load(std::sync::atomic::Ordering::Relaxed)))
            })
            .await;

            let (hits, scanned) = match result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return ToolOutput::error(e),
                Err(e) => return ToolOutput::error(format!("search failed: {e}")),
            };
            if hits.is_empty() {
                return ToolOutput::text(format!(
                    "No matches for /{pattern}/ ({scanned} files searched)."
                ));
            }
            let total: usize = hits.iter().map(|h| h.count).sum();
            let mut out = String::new();
            if files_only {
                out.push_str(&format!("{total} matches in {} files:\n", hits.len()));
                for h in hits.iter().take(max) {
                    out.push_str(&format!("{} ({})\n", ctx.display(&h.path), h.count));
                }
                return ToolOutput::text(out);
            }
            let mut budget = max;
            let mut omitted_files = 0;
            let cache = ctx.host.file_cache();
            for h in &hits {
                if budget == 0 {
                    omitted_files += 1;
                    continue;
                }
                out.push_str(&format!("[{}#{}]\n", ctx.display(&h.path), h.tag));
                let mut prev = 0;
                let mut shown = Vec::new();
                for (n, is_match, line) in &h.lines {
                    if budget == 0 {
                        out.push_str("…\n");
                        break;
                    }
                    if prev != 0 && *n > prev + 1 {
                        out.push_str("…\n");
                    }
                    out.push_str(&format!("{n}{}{line}\n", if *is_match { ':' } else { '-' }));
                    shown.push((*n, *n));
                    if *is_match {
                        budget -= 1;
                    }
                    prev = *n;
                }
                // Lines grep showed (matches and context) may be edited by number.
                if let Some(cache) = cache {
                    cache.saw(&ctx.session_id, &h.path, &h.tag, None, shown);
                }
            }
            let mut header = format!("{total} matches in {} files", hits.len());
            if omitted_files > 0 || total > max {
                header.push_str(&format!(" (showing first {max}; {omitted_files} more files — narrow with path/glob or use files_only)"));
            }
            let (text, _) = super::fit_output(&format!("{header}\n{out}"), ctx, "grep output");
            let mut out = ToolOutput::text(text);
            super::attach_late_diagnostics(ctx, &mut out);
            out
        })
    }
}

/// Searching the credential store is refused; searching a tree that *contains*
/// it (e.g. `$HOME`) skips it instead (see `walker`).
fn search_root_risk(root: &std::path::Path, ctx: &ToolCtx) -> super::Risk {
    match super::path_risk(root, ctx) {
        r @ super::Risk::Block { .. } => r,
        _ => super::Risk::None,
    }
}

pub struct Glob;

impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> String {
        "Find files by glob (\"**/*.rs\", \"src/**/test_*.py\"), respecting .gitignore; most recently modified first."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"},
                "limit": {"type": "integer", "description": "Default 200."}
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> super::Risk {
        let root = opt_str(args, "path")
            .map(|p| ctx.resolve(p))
            .unwrap_or_else(|| ctx.cwd.clone());
        search_root_risk(&root, ctx)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let pattern = match arg_str(&args, "pattern") {
                Ok(p) => p.to_string(),
                Err(e) => return e,
            };
            let root = super::canonical(
                &opt_str(&args, "path")
                    .map(|p| ctx.resolve(p))
                    .unwrap_or_else(|| ctx.cwd.clone()),
            );
            let limit = opt_u64(&args, "limit").unwrap_or(200).clamp(1, 5000) as usize;
            let matcher = match globset::GlobBuilder::new(&pattern)
                .literal_separator(false)
                .build()
            {
                Ok(g) => g.compile_matcher(),
                Err(e) => return ToolOutput::error(format!("Invalid glob: {e}")),
            };
            // A bare name like "*.rs" should match at any depth.
            let deep = if pattern.contains('/') {
                None
            } else {
                globset::Glob::new(&format!("**/{pattern}"))
                    .ok()
                    .map(|g| g.compile_matcher())
            };
            let root2 = root.clone();
            let protected = ctx.host.protected_paths();
            let found = tokio::task::spawn_blocking(move || {
                let found: Arc<Mutex<Vec<(std::time::SystemTime, PathBuf)>>> = Arc::default();
                let Ok(walk) = walker(&root2, None, protected) else {
                    return Vec::new();
                };
                walk.run(|| {
                    let found = found.clone();
                    let matcher = matcher.clone();
                    let deep = deep.clone();
                    let root = root2.clone();
                    Box::new(move |entry| {
                        let Ok(entry) = entry else {
                            return ignore::WalkState::Continue;
                        };
                        let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                        if rel.as_os_str().is_empty() {
                            return ignore::WalkState::Continue;
                        }
                        if matcher.is_match(rel) || deep.as_ref().is_some_and(|d| d.is_match(rel)) {
                            let mtime = entry
                                .metadata()
                                .ok()
                                .and_then(|m| m.modified().ok())
                                .unwrap_or(std::time::UNIX_EPOCH);
                            found.lock().push((mtime, entry.path().to_path_buf()));
                        }
                        ignore::WalkState::Continue
                    })
                });
                let mut v = std::mem::take(&mut *found.lock());
                v.sort_by_key(|x| std::cmp::Reverse(x.0));
                v.into_iter().map(|(_, p)| p).collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            if found.is_empty() {
                return ToolOutput::text(format!(
                    "No files match `{pattern}` under {}.",
                    ctx.display(&root)
                ));
            }
            let mut out = format!("{} files match `{pattern}`", found.len());
            if found.len() > limit {
                out.push_str(&format!(" (showing {limit} most recently modified)"));
            }
            out.push('\n');
            for p in found.iter().take(limit) {
                out.push_str(&ctx.display(p));
                out.push('\n');
            }
            ToolOutput::text(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(root: &std::path::Path, protected: super::super::Protected) -> Vec<PathBuf> {
        let found: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        walker(&super::super::canonical(root), None, protected)
            .unwrap()
            .run(|| {
                let found = found.clone();
                Box::new(move |e| {
                    if let Ok(e) = e {
                        found.lock().push(e.path().to_path_buf());
                    }
                    ignore::WalkState::Continue
                })
            });
        std::mem::take(&mut *found.lock())
    }

    #[test]
    fn walks_skip_protected_trees_however_the_root_is_spelled() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("work")).unwrap();
        std::fs::create_dir_all(home.join("store")).unwrap();
        std::fs::write(home.join("store/auth.json"), "x").unwrap();
        std::fs::write(home.join("work/a.txt"), "x").unwrap();
        let protected = super::super::Protected {
            roots: vec![home.join("store")],
            except: vec![],
        };
        // `work/..` names the parent without starting with it component-wise.
        let found = walk(&home.join("work/.."), protected);
        assert!(found.iter().any(|p| p.ends_with("work/a.txt")), "{found:?}");
        assert!(!found.iter().any(|p| p.ends_with("auth.json")), "{found:?}");
    }
}
