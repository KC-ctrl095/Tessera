//! `apply_patch` patches: the edit form GPT and Codex models are trained on
//! (OpenAI's "V4A" format). Taken only from those models: `engine/calls.rs`
//! turns their `apply_patch` calls (made, or written as a shell command) into
//! an `edit` with `patch`. Nothing offers it to other models, and the base
//! prompt doesn't mention it (in the hashline benchmark it was the weakest
//! edit form for 15 of 16 models; batch 14 research R2 T4).
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path            followed by "+line" lines
//! *** Delete File: path
//! *** Update File: path         optionally "*** Move to: new/path", then hunks:
//! @@ fn context_line            (optional: a line to find first)
//!  unchanged line
//! -removed line
//! +added line
//! *** End of File               (optional: the hunk ends the file)
//! *** End Patch
//! ```
//!
//! Hunks are found with the same tolerant matcher as `replace` (exact, then
//! ignoring indentation, then with lines trimmed), in order but anywhere in
//! the file; all files are worked out first and nothing is written unless
//! every hunk applies.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::files::{
    AfterWrite, Loose, block_fits, commit, common_indent, path_write_risk, post_write, reindent,
};
use super::{Risk, ToolCtx, ToolOutput};

/// GPT and Codex models (by model id, also behind a router: `openai/gpt-5`).
pub fn patch_family(model: &str) -> bool {
    let m = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    m.starts_with("gpt-")
        || m.contains("codex")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
}

/// Text that is a patch in this format.
pub fn looks_like_patch(s: &str) -> bool {
    s.contains("*** Begin Patch")
        || s.trim_start().starts_with("*** Update File:")
        || s.trim_start().starts_with("*** Add File:")
        || s.trim_start().starts_with("*** Delete File:")
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chunk {
    /// The `@@` line: found first, the hunk is looked for after it.
    pub context: Option<String>,
    pub old: Vec<String>,
    pub new: Vec<String>,
    /// The hunk ends the file (`*** End of File`).
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<Chunk>,
    },
}

impl Op {
    fn path(&self) -> &str {
        match self {
            Op::Add { path, .. } | Op::Delete { path } | Op::Update { path, .. } => path,
        }
    }
}

fn op_line(line: &str) -> bool {
    line.starts_with("*** ") && !line.starts_with("*** End of File")
}

/// A unified-diff hunk header (`@@ -12,3 +12,4 @@`): no context to find.
fn numeric_header(ctx: &str) -> bool {
    let t = ctx.trim().trim_end_matches("@@").trim();
    !t.is_empty()
        && t.split_whitespace().all(|w| {
            (w.starts_with('-') || w.starts_with('+'))
                && w[1..].split(',').all(|n| n.parse::<u64>().is_ok())
        })
}

/// Read a patch. Lenient where the intent is clear: no Begin/End lines, a
/// hunk without `@@`, a context line missing its leading space.
pub fn parse(text: &str) -> Result<Vec<Op>, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut i = lines
        .iter()
        .position(|l| l.trim() == "*** Begin Patch")
        .map_or(0, |p| p + 1);
    let mut ops = Vec::new();
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_end();
        if t.trim() == "*** End Patch" {
            break;
        }
        if t.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some(path) = t.strip_prefix("*** Add File:") {
            let path = path.trim().to_string();
            i += 1;
            let mut body = Vec::new();
            while i < lines.len() && !op_line(lines[i]) {
                let l = lines[i];
                body.push(l.strip_prefix('+').unwrap_or(l));
                i += 1;
            }
            // A blank line before the next file belongs to the patch layout.
            while body.last().is_some_and(|l| l.is_empty()) && i < lines.len() {
                body.pop();
            }
            let mut content = body.join("\n");
            content.push('\n');
            ops.push(Op::Add { path, content });
            continue;
        }
        if let Some(path) = t.strip_prefix("*** Delete File:") {
            ops.push(Op::Delete {
                path: path.trim().to_string(),
            });
            i += 1;
            continue;
        }
        if let Some(path) = t.strip_prefix("*** Update File:") {
            let path = path.trim().to_string();
            i += 1;
            let mut move_to = None;
            if let Some(to) = lines.get(i).and_then(|l| l.strip_prefix("*** Move to:")) {
                move_to = Some(to.trim().to_string());
                i += 1;
            }
            let mut chunks = Vec::new();
            let mut cur = Chunk::default();
            let mut started = false;
            while i < lines.len() && !op_line(lines[i]) {
                let l = lines[i];
                if let Some(ctx) = l.strip_prefix("@@") {
                    if started && (!cur.old.is_empty() || !cur.new.is_empty()) {
                        chunks.push(std::mem::take(&mut cur));
                    }
                    let ctx = ctx.trim();
                    cur.context =
                        (!ctx.is_empty() && !numeric_header(ctx)).then(|| ctx.to_string());
                    started = true;
                } else if l.starts_with("*** End of File") {
                    cur.eof = true;
                } else if let Some(x) = l.strip_prefix('-') {
                    cur.old.push(x.to_string());
                    started = true;
                } else if let Some(x) = l.strip_prefix('+') {
                    cur.new.push(x.to_string());
                    started = true;
                } else {
                    // Unchanged (a missing leading space is forgiven).
                    let x = l.strip_prefix(' ').unwrap_or(l);
                    cur.old.push(x.to_string());
                    cur.new.push(x.to_string());
                    started = true;
                }
                i += 1;
            }
            // Blank lines that only separate this file from the next.
            while cur.old.last().is_some_and(String::is_empty)
                && cur.new.last().is_some_and(String::is_empty)
                && i < lines.len()
            {
                cur.old.pop();
                cur.new.pop();
            }
            if !cur.old.is_empty() || !cur.new.is_empty() {
                chunks.push(cur);
            }
            if chunks.is_empty() && move_to.is_none() {
                return Err(format!("`*** Update File: {path}` has no changes."));
            }
            ops.push(Op::Update {
                path,
                move_to,
                chunks,
            });
            continue;
        }
        return Err(format!(
            "Unexpected line in the patch: `{}`. Each file starts with `*** Add File:`, `*** Update File:` or `*** Delete File:`.",
            crate::util::one_line(t, 80)
        ));
    }
    if ops.is_empty() {
        return Err("The patch changes no file.".into());
    }
    Ok(ops)
}

/// Every path a patch writes (moves included), in order.
pub fn paths(ops: &[Op]) -> Vec<String> {
    let mut out = Vec::new();
    for op in ops {
        out.push(op.path().to_string());
        if let Op::Update {
            move_to: Some(to), ..
        } = op
        {
            out.push(to.clone());
        }
    }
    out
}

/// Where `pat` stands in `lines` at or after `from`: exactly, then by the
/// loose strategies of `replace`. `eof`: at the very end first.
fn seek(lines: &[&str], pat: &[&str], from: usize, eof: bool) -> Option<(usize, Option<Loose>)> {
    let n = pat.len();
    if n == 0 || lines.len() < n {
        return None;
    }
    let last = lines.len() - n;
    let starts: Vec<usize> = if eof {
        std::iter::once(last).chain(from..=last).collect()
    } else {
        (from.min(last + 1)..=last).collect()
    };
    let indent = common_indent(pat);
    for how in [None, Some(Loose::Indent), Some(Loose::Trimmed)] {
        for &i in &starts {
            let window = &lines[i..i + n];
            let fits = match how {
                None => window == pat,
                Some(h) => block_fits(h, window, pat, indent),
            };
            if fits {
                return Some((i, how));
            }
        }
    }
    None
}

/// Apply one file's hunks to `old`.
pub fn apply_chunks(old: &str, chunks: &[Chunk]) -> Result<String, String> {
    let crlf = old.contains("\r\n");
    let text = if crlf {
        old.replace("\r\n", "\n")
    } else {
        old.to_string()
    };
    let ends_nl = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let mut edits: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut cursor = 0;
    for (k, c) in chunks.iter().enumerate() {
        let hunk = k + 1;
        if let Some(ctx) = &c.context {
            let found = seek(&lines, &[ctx.as_str()], cursor, false)
                .or_else(|| seek(&lines, &[ctx.as_str()], 0, false));
            match found {
                Some((at, _)) => cursor = at + 1,
                None => {
                    return Err(format!(
                        "hunk {hunk}: its `@@ {}` line isn't in the file.{}",
                        crate::util::one_line(ctx, 80),
                        super::files::closest_hint_for(&text, ctx)
                    ));
                }
            }
        }
        if c.old.is_empty() {
            // Only additions: after the `@@` line, or at the end.
            let at = if c.context.is_some() {
                cursor
            } else {
                lines.len()
            };
            edits.push((at, 0, c.new.clone()));
            continue;
        }
        let mut old: Vec<&str> = c.old.iter().map(String::as_str).collect();
        let mut new = c.new.clone();
        let mut found = seek(&lines, &old, cursor, c.eof).or_else(|| seek(&lines, &old, 0, c.eof));
        // A trailing blank line the file doesn't have.
        if found.is_none() && old.last() == Some(&"") {
            old.pop();
            if new.last().is_some_and(String::is_empty) {
                new.pop();
            }
            found = seek(&lines, &old, cursor, c.eof).or_else(|| seek(&lines, &old, 0, c.eof));
        }
        let Some((at, how)) = found else {
            return Err(format!(
                "hunk {hunk}: these lines aren't in the file:\n{}{}",
                old.iter()
                    .take(8)
                    .map(|l| format!("{l}\n"))
                    .collect::<String>(),
                super::files::closest_hint_for(&text, &old.join("\n"))
            ));
        };
        let (a0, a1) = (at, at + old.len());
        if edits.iter().any(|(s, len, _)| {
            if *len == 0 {
                *s > a0 && *s < a1
            } else {
                a0 < s + len && *s < a1
            }
        }) {
            return Err(format!(
                "hunk {hunk} overlaps an earlier hunk. Give each change once."
            ));
        }
        if how == Some(Loose::Indent) {
            let have = common_indent(&lines[at..at + old.len()]);
            let want = common_indent(&old);
            new = reindent(&new.join("\n"), want, have)
                .split('\n')
                .map(str::to_string)
                .collect();
        }
        edits.push((at, old.len(), new));
        cursor = at + old.len();
    }
    edits.sort_by_key(|e| (e.0, e.1));
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    for (at, len, new) in edits.into_iter().rev() {
        out.splice(at..at + len, new);
    }
    let mut s = out.join("\n");
    if ends_nl || (text.is_empty() && !s.is_empty()) {
        s.push('\n');
    }
    Ok(if crlf { s.replace('\n', "\r\n") } else { s })
}

/// Approval: the riskiest of the files the patch writes.
pub fn assess(args: &Value, ctx: &ToolCtx) -> Risk {
    let Some(ops) = args
        .get("patch")
        .and_then(Value::as_str)
        .and_then(|p| parse(p).ok())
    else {
        return Risk::None;
    };
    let mut asks: Vec<(String, String)> = Vec::new();
    for p in paths(&ops) {
        match path_write_risk(&ctx.resolve(&p), ctx) {
            r @ Risk::Block { .. } => return r,
            Risk::Ask { reason, key } => {
                if !asks.iter().any(|(_, k)| *k == key) {
                    asks.push((reason, key));
                }
            }
            Risk::None => {}
        }
    }
    match asks.len() {
        0 => Risk::None,
        _ => Risk::Ask {
            reason: asks
                .iter()
                .map(|(r, _)| r.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            key: asks
                .iter()
                .map(|(_, k)| k.as_str())
                .collect::<Vec<_>>()
                .join("+"),
        },
    }
}

/// One file's planned change.
struct Planned {
    path: PathBuf,
    display: String,
    old: Option<String>,
    /// `None`: delete it.
    new: Option<String>,
    /// Moved here from this path (removed after the write).
    from: Option<(PathBuf, String, String)>,
}

async fn remove(ctx: &ToolCtx, path: &std::path::Path, expected: &str) -> Result<(), String> {
    let destination = super::files::prepare_destination(path, ctx)?;
    let expected = expected.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || destination.delete(&expected))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
async fn patch_text(path: &std::path::Path, ctx: &ToolCtx) -> Result<Option<String>, String> {
    super::files::authorize_file(&super::canonical(path), ctx, true)?;
    match super::file_object::Opened::open(path) {
        Ok(opened) => {
            super::files::authorize_file(&opened.path, ctx, true)?;
            let ctx = ctx.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                super::files::authorized_opened_bytes(&opened, &ctx, 16 * 1024 * 1024)
            })
            .await
            .map_err(|e| e.to_string())??;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path).is_err() =>
        {
            Ok(None)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Apply a patch for the `edit` tool: every file worked out first, then
/// written (snapshotted for rewind), each checked like an edit.
pub async fn run(patch: &str, ctx: &ToolCtx) -> ToolOutput {
    let ops = match parse(patch) {
        Ok(o) => o,
        Err(e) => return ToolOutput::error(format!("Patch not applied: {e}")),
    };
    let mut plan: Vec<Planned> = Vec::new();
    let mut unchanged = Vec::new();
    for op in &ops {
        let path = ctx.resolve(op.path());
        let display = ctx.display(&path);
        let current = match patch_text(&path, ctx).await {
            Ok(text) => text,
            Err(e) => return ToolOutput::error(format!("Patch not applied: {e}")),
        };
        match op {
            Op::Add { content, .. } => match current {
                Some(c) if c == *content => unchanged.push(display),
                Some(_) => {
                    return ToolOutput::error(format!(
                        "Patch not applied: {display} exists already. Use `*** Update File: {display}` to change it."
                    ));
                }
                None => plan.push(Planned {
                    path,
                    display,
                    old: None,
                    new: Some(content.clone()),
                    from: None,
                }),
            },
            Op::Delete { .. } => match current {
                Some(c) => plan.push(Planned {
                    path,
                    display,
                    old: Some(c),
                    new: None,
                    from: None,
                }),
                None if !path.exists() => unchanged.push(display),
                None => {
                    return ToolOutput::error(format!(
                        "Patch not applied: cannot read {display} to delete it."
                    ));
                }
            },
            Op::Update {
                move_to, chunks, ..
            } => {
                let Some(old) = current else {
                    return ToolOutput::error(format!(
                        "Patch not applied: cannot read {display}. Use `*** Add File:` for new files."
                    ));
                };
                let new = match apply_chunks(&old, chunks) {
                    Ok(n) => n,
                    Err(e) => {
                        return ToolOutput::error(format!("Patch not applied: {display}, {e}"));
                    }
                };
                match move_to {
                    Some(to) => {
                        let dest = ctx.resolve(to);
                        let dest_display = ctx.display(&dest);
                        let there = match patch_text(&dest, ctx).await {
                            Ok(text) => text,
                            Err(e) => return ToolOutput::error(format!("Patch not applied: {e}")),
                        };
                        plan.push(Planned {
                            path: dest,
                            display: dest_display,
                            old: there,
                            new: Some(new),
                            from: Some((path, display, old)),
                        });
                    }
                    None if new == old => unchanged.push(display),
                    None => plan.push(Planned {
                        path,
                        display,
                        old: Some(old),
                        new: Some(new),
                        from: None,
                    }),
                }
            }
        }
    }
    if plan.is_empty() {
        return ToolOutput::text(format!(
            "No change: the patch is already applied ({}).",
            unchanged.join(", ")
        ))
        .with_details(json!({ "path": unchanged.first(), "unchanged": true }));
    }
    let mut text = String::new();
    let mut diffs = String::new();
    let mut shown = Vec::new();
    let mut verdicts: Vec<Option<bool>> = Vec::new();
    let mut failed = false;
    for p in &plan {
        shown.push(p.display.clone());
        match &p.new {
            None => {
                let _ = ctx.host.store().snapshot_file(
                    &ctx.session_id,
                    ctx.seq,
                    &p.path.to_string_lossy(),
                    p.old.as_deref().map(str::as_bytes),
                );
                if let Err(e) = remove(ctx, &p.path, p.old.as_deref().unwrap_or("")).await {
                    failed = true;
                    text.push_str(&format!("Could not delete {}: {e}\n", p.display));
                    continue;
                }
                let (_, removed, diff) =
                    super::files::diff_stats(p.old.as_deref().unwrap_or(""), "", &p.display);
                diffs.push_str(&diff);
                text.push_str(&format!("Deleted {} (−{removed})\n", p.display));
                verdicts.push(None);
            }
            Some(new) => {
                let c = match commit(
                    ctx,
                    &p.path,
                    &p.display,
                    p.old.as_deref(),
                    new,
                    p.old.is_none(),
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        failed = true;
                        text.push_str(&format!("{e}\n"));
                        continue;
                    }
                };
                diffs.push_str(&c.diff);
                if let Some((from, from_display, from_old)) = &p.from {
                    let _ = ctx.host.store().snapshot_file(
                        &ctx.session_id,
                        ctx.seq,
                        &from.to_string_lossy(),
                        Some(from_old.as_bytes()),
                    );
                    if let Err(e) = remove(ctx, from, from_old).await {
                        failed = true;
                        text.push_str(&format!("Could not remove {from_display}: {e}\n"));
                        continue;
                    }
                    shown.push(from_display.clone());
                    text.push_str(&format!(
                        "[{}#{}] moved from {from_display} (+{} −{})\n{}",
                        p.display, c.tag, c.added, c.removed, c.preview
                    ));
                } else if p.old.is_none() {
                    text.push_str(&format!(
                        "[{}#{}] Created ({} lines)\n",
                        p.display,
                        c.tag,
                        new.lines().count()
                    ));
                } else {
                    text.push_str(&format!(
                        "[{}#{}] edited (+{} −{})\n{}",
                        p.display, c.tag, c.added, c.removed, c.preview
                    ));
                }
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                let after: AfterWrite =
                    post_write(ctx, &p.path, &p.display, p.old.as_deref(), new).await;
                for block in &after.blocks {
                    text.push_str(block);
                    text.push('\n');
                }
                verdicts.push(after.clean);
            }
        }
    }
    if !unchanged.is_empty() {
        text.push_str(&format!(
            "(Already as the patch has it: {}.)\n",
            unchanged.join(", ")
        ));
    }
    let mut details = json!({ "path": plan[0].display, "paths": shown, "diff": diffs });
    // Checked as a whole only when every file was, and cleanly.
    let all_clean = !failed && !verdicts.is_empty() && verdicts.iter().all(|v| *v == Some(true));
    let any_broken = verdicts.contains(&Some(false));
    if all_clean || any_broken {
        AfterWrite {
            blocks: Vec::new(),
            clean: Some(all_clean),
        }
        .mark(&mut details);
    }
    let mut out = ToolOutput::text(text.trim_end().to_string()).with_details(details);
    out.is_error = failed;
    super::attach_late_diagnostics(ctx, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "*** Begin Patch
*** Add File: hello.py
+def hello():
+    return \"hi\"
*** Update File: src/app.py
@@ def main():
-    print(\"old\")
+    print(\"new\")
     return 0
*** Delete File: old.txt
*** End Patch";

    #[test]
    fn parses_add_update_delete() {
        let ops = parse(PATCH).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(
            ops[0],
            Op::Add {
                path: "hello.py".into(),
                content: "def hello():\n    return \"hi\"\n".into()
            }
        );
        let Op::Update {
            path,
            chunks,
            move_to,
        } = &ops[1]
        else {
            panic!()
        };
        assert_eq!(path, "src/app.py");
        assert_eq!(move_to, &None);
        assert_eq!(chunks[0].context.as_deref(), Some("def main():"));
        assert_eq!(chunks[0].old, vec!["    print(\"old\")", "    return 0"]);
        assert_eq!(chunks[0].new, vec!["    print(\"new\")", "    return 0"]);
        assert_eq!(
            ops[2],
            Op::Delete {
                path: "old.txt".into()
            }
        );
        assert_eq!(paths(&ops), vec!["hello.py", "src/app.py", "old.txt"]);
    }

    #[test]
    fn lenient_where_the_intent_is_clear() {
        // No Begin/End, no @@, a numeric unified-diff header, a move.
        let ops = parse(
            "*** Update File: a.rs\n*** Move to: b.rs\n@@ -1,2 +1,2 @@\n fn a() {\n-  1\n+  2\n",
        )
        .unwrap();
        let Op::Update {
            move_to, chunks, ..
        } = &ops[0]
        else {
            panic!()
        };
        assert_eq!(move_to.as_deref(), Some("b.rs"));
        assert_eq!(chunks[0].context, None);
        assert_eq!(chunks[0].old, vec!["fn a() {", "  1"]);
        assert!(parse("*** Begin Patch\n*** End Patch").is_err());
        assert!(parse("hello").unwrap_err().contains("Unexpected line"));
    }

    fn chunk(context: Option<&str>, old: &[&str], new: &[&str]) -> Chunk {
        Chunk {
            context: context.map(str::to_string),
            old: old.iter().map(|s| s.to_string()).collect(),
            new: new.iter().map(|s| s.to_string()).collect(),
            eof: false,
        }
    }

    #[test]
    fn hunks_apply_in_any_order_with_the_tolerant_matcher() {
        let src = "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n";
        // Given out of order: b's hunk first.
        let out = apply_chunks(
            src,
            &[
                chunk(None, &["    two();"], &["    three();"]),
                chunk(None, &["    one();"], &["    uno();"]),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "fn a() {\n    uno();\n}\n\nfn b() {\n    three();\n}\n"
        );
        // Indentation lost in the patch: matched and re-indented.
        let out = apply_chunks(
            src,
            &[chunk(
                Some("fn b() {"),
                &["two();"],
                &["two();", "again();"],
            )],
        )
        .unwrap();
        assert_eq!(
            out,
            "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n    again();\n}\n"
        );
        // CRLF kept; pure additions after the context line.
        let out = apply_chunks("x\r\ny\r\n", &[chunk(Some("x"), &[], &["z"])]).unwrap();
        assert_eq!(out, "x\r\nz\r\ny\r\n");
        // A missing hunk names the lines and the closest block.
        let err = apply_chunks(src, &[chunk(None, &["    tw0();"], &["x"])]).unwrap_err();
        assert!(
            err.contains("aren't in the file") && err.contains("closest"),
            "{err}"
        );
    }

    #[test]
    fn the_family() {
        for m in [
            "gpt-5.2-codex",
            "gpt-5",
            "openai/gpt-4.1",
            "codex-mini-latest",
            "o3",
            "o4-mini",
            "gpt-oss-120b",
        ] {
            assert!(patch_family(m), "{m}");
        }
        for m in [
            "qwen3.8-27b",
            "claude-sonnet-4-5",
            "gemini-2.5-pro",
            "deepseek-v3",
            "glm-4.7",
        ] {
            assert!(!patch_family(m), "{m}");
        }
    }
}
