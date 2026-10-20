//! Quick syntax checks after `write`/`edit`, for files no language server
//! covers, with what is already installed (nothing is ever installed):
//! Python (`compile()`), JavaScript (`node --check`) and shell scripts
//! (`bash -n`). The text goes in on stdin, so nothing is written (no
//! `.pyc`) and no file needs to be readable; the checkers run like other
//! agent commands (in the sandbox when it is on, below the server's
//! priority, without secrets) and get at most two seconds. A broken result
//! is a warning appended to the edit's result; the edit stands.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::ToolCtx;

/// The longest a check may take.
const LIMIT: Duration = Duration::from_secs(2);

/// Python: report the first syntax error as "message (line N, column M)".
/// Works with Python 2 too (no f-strings).
const PY_CHECK: &str = "import sys\nsrc = sys.stdin.read()\ntry:\n    compile(src, sys.argv[1], 'exec')\nexcept SyntaxError as e:\n    print('%s (line %s, column %s)' % (e.msg, e.lineno, e.offset))\n    sys.exit(1)\nexcept ValueError as e:\n    print(e)\n    sys.exit(1)\n";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lang {
    Python,
    /// `module`: ES module syntax (`import`/`export`).
    Js {
        module: bool,
    },
    /// The shell that checks it (`bash`, `sh`).
    Shell(&'static str),
}

impl Lang {
    /// What `path` (with this text) is, if it can be checked here.
    pub fn of(path: &Path, text: &str) -> Option<Lang> {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some("py" | "pyw") => return Some(Lang::Python),
            Some("mjs") => return Some(Lang::Js { module: true }),
            Some("js" | "cjs") => {
                return Some(Lang::Js {
                    module: ext.as_deref() == Some("js") && esm(text),
                });
            }
            Some("sh" | "bash") => return Some(Lang::Shell("bash")),
            Some(_) => return None,
            None => {}
        }
        // No extension: the first line says.
        let first = text.lines().next()?.strip_prefix("#!")?;
        let interp = first
            .split_whitespace()
            .flat_map(|w| w.rsplit('/').next())
            .find(|w| *w != "env" && !w.starts_with('-'))?;
        match interp {
            i if i.starts_with("python") => Some(Lang::Python),
            "node" | "nodejs" => Some(Lang::Js { module: esm(text) }),
            "bash" => Some(Lang::Shell("bash")),
            "sh" | "dash" => Some(Lang::Shell("sh")),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::Js { .. } => "node --check",
            Lang::Shell("sh") => "sh -n",
            Lang::Shell(_) => "bash -n",
        }
    }

    /// The installed program and its arguments, or `None` when there is none.
    fn command(self, display: &str) -> Option<(PathBuf, Vec<String>)> {
        match self {
            Lang::Python => {
                let p = installed("python3").or_else(|| installed("python"))?;
                Some((p, vec!["-c".into(), PY_CHECK.into(), display.into()]))
            }
            Lang::Js { module } => {
                let p = installed("node").or_else(|| installed("nodejs"))?;
                let mut args = Vec::new();
                if module {
                    args.push("--input-type=module".to_string());
                }
                args.extend(["--check".to_string(), "-".to_string()]);
                Some((p, args))
            }
            Lang::Shell(sh) => Some((installed(sh)?, vec!["-n".into()])),
        }
    }
}

/// ES module syntax: a line starting with `import x`/`export`.
fn esm(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start();
        (l.starts_with("import ") && !l.starts_with("import ("))
            || l.starts_with("import{")
            || l.starts_with("export ")
            || l.starts_with("export{")
    })
}

/// A program on PATH, looked up once per name.
fn installed(name: &'static str) -> Option<PathBuf> {
    type Programs = Vec<(&'static str, Option<PathBuf>)>;
    static FOUND: OnceLock<parking_lot::Mutex<Programs>> = OnceLock::new();
    let cache = FOUND.get_or_init(Default::default);
    if let Some((_, p)) = cache.lock().iter().find(|(n, _)| *n == name) {
        return p.clone();
    }
    let p = crate::lsp::servers::which(name);
    cache.lock().push((name, p.clone()));
    p
}

/// What a quick check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Clean,
    /// The warning for the model.
    Broken(String),
}

/// Run the checker on `text`: `Some(None)` clean, `Some(Some(error))`
/// broken, `None` when it couldn't tell (missing, refused, too slow).
async fn run(lang: Lang, display: &str, text: &str, ctx: &ToolCtx) -> Option<Option<String>> {
    let (program, args) = lang.command(display)?;
    let cwd = ctx.host.process_directory(&ctx.session_id, &ctx.cwd).ok()?;
    let sandbox = cwd
        .prepare(ctx.host.session_sandbox(&ctx.session_id).as_ref())
        .ok()?;
    let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    let (program, args) = cwd.argv(sandbox.as_ref(), program.into_os_string(), args);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .kill_on_drop(true);
    super::bash::withhold_env(&mut cmd);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            super::bash::yield_to_server();
            Ok(())
        });
    }
    let held = cwd.apply(&mut cmd, sandbox);
    let mut child = ctx.admission.run(|| cmd.spawn()).ok()?;
    let _held = held.after_spawn();
    let mut stdin = child.stdin.take()?;
    let input = text.as_bytes().to_vec();
    let feed = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    });
    let out = tokio::time::timeout(LIMIT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    feed.abort();
    if out.status.success() {
        return Some(None);
    }
    // Killed by a signal, or the checker itself broke: no verdict.
    let code = out.status.code()?;
    if code > 2 {
        return None;
    }
    let raw = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some(Some(summary(lang, &raw)))
}

/// The error in one line: "invalid syntax (line 3, column 5)".
fn summary(lang: Lang, raw: &str) -> String {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let text = match lang {
        Lang::Python => lines.last().copied().unwrap_or("syntax error").to_string(),
        Lang::Js { .. } => {
            let line = lines
                .iter()
                .find_map(|l| l.strip_prefix("[stdin]:"))
                .and_then(|n| n.trim().parse::<u32>().ok());
            let msg = lines
                .iter()
                .find(|l| l.starts_with("SyntaxError") || l.contains("Error:"))
                .map(|l| l.split_once(": ").map_or(*l, |(_, m)| m))
                .unwrap_or("syntax error");
            match line {
                Some(n) => format!("{msg} (line {n})"),
                None => msg.to_string(),
            }
        }
        Lang::Shell(_) => {
            let first = lines.first().copied().unwrap_or("syntax error");
            // "bash: …", "/usr/bin/bash: …", "sh: …"
            let first = match first.split_once(": ") {
                Some((head, rest))
                    if matches!(
                        head.rsplit('/').next(),
                        Some("bash" | "sh" | "dash" | "zsh")
                    ) =>
                {
                    rest
                }
                _ => first,
            };
            // dash: "3: Syntax error: …"
            match first.split_once(": ") {
                Some((n, rest)) if n.chars().all(|c| c.is_ascii_digit()) => {
                    format!("line {n}: {rest}")
                }
                _ => first.to_string(),
            }
        }
    };
    crate::util::one_line(&text, 300)
}

/// Check what was just written to `path` (`old`: its text before, if it
/// existed). `None` when this file type can't be checked here or the
/// checker gave no verdict.
pub async fn check(
    ctx: &ToolCtx,
    path: &Path,
    display: &str,
    old: Option<&str>,
    new: &str,
) -> Option<Found> {
    if !ctx.host.quick_checks() {
        return None;
    }
    let lang = Lang::of(path, new)?;
    let err = run(lang, display, new, ctx).await?;
    let Some(err) = err else {
        return Some(Found::Clean);
    };
    let how = lang.name();
    // Whether the file was already broken before this change.
    let before = match old {
        Some(o) if !o.trim().is_empty() => run(lang, display, o, ctx).await,
        _ => None,
    };
    Some(Found::Broken(match before {
        Some(Some(_)) => format!(
            "Warning: {display} still has a syntax error (it had one before this edit too; {how}): {err}"
        ),
        Some(None) => format!("Warning: {display} has a syntax error now ({how}): {err}"),
        None => format!("Warning: {display} has a syntax error ({how}): {err}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_kinds() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(Lang::of(&p("a.py"), ""), Some(Lang::Python));
        assert_eq!(Lang::of(&p("a.mjs"), ""), Some(Lang::Js { module: true }));
        assert_eq!(
            Lang::of(&p("a.js"), "const x = require('x');\n"),
            Some(Lang::Js { module: false })
        );
        assert_eq!(
            Lang::of(&p("a.js"), "import fs from 'fs';\n"),
            Some(Lang::Js { module: true })
        );
        assert_eq!(
            Lang::of(&p("a.cjs"), "export default 1;\n"),
            Some(Lang::Js { module: false })
        );
        assert_eq!(Lang::of(&p("run.sh"), ""), Some(Lang::Shell("bash")));
        assert_eq!(
            Lang::of(&p("bin/tool"), "#!/usr/bin/env python3\nprint(1)\n"),
            Some(Lang::Python)
        );
        assert_eq!(
            Lang::of(&p("bin/tool"), "#!/bin/sh\necho\n"),
            Some(Lang::Shell("sh"))
        );
        assert_eq!(Lang::of(&p("a.ts"), ""), None);
        assert_eq!(Lang::of(&p("a.rs"), ""), None);
        assert_eq!(Lang::of(&p("notes"), "just text\n"), None);
    }

    #[test]
    fn error_summaries() {
        assert_eq!(
            summary(Lang::Python, "invalid syntax (line 3, column 5)\n"),
            "invalid syntax (line 3, column 5)"
        );
        let node = "[stdin]:4\nconst x = ;\n          ^\n\nSyntaxError: Unexpected token ';'\n    at wrapSafe (node:internal)\n\nNode.js v22.22.2\n";
        assert_eq!(
            summary(Lang::Js { module: false }, node),
            "Unexpected token ';' (line 4)"
        );
        assert_eq!(
            summary(
                Lang::Shell("bash"),
                "bash: line 3: syntax error near unexpected token `fi'\nbash: line 3: `fi'\n"
            ),
            "line 3: syntax error near unexpected token `fi'"
        );
        assert_eq!(
            summary(
                Lang::Shell("bash"),
                "/usr/bin/bash: line 3: syntax error: unexpected end of file\n"
            ),
            "line 3: syntax error: unexpected end of file"
        );
        assert_eq!(
            summary(
                Lang::Shell("sh"),
                "sh: 3: Syntax error: \"fi\" unexpected\n"
            ),
            "line 3: Syntax error: \"fi\" unexpected"
        );
    }
}
