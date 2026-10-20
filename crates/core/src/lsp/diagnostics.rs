//! Diagnostics: parsing `publishDiagnostics`, working out which ones an edit
//! introduced, and rendering them compactly (`path:line:col severity msg`).

use serde_json::Value;

use crate::tools::linemap::{LineMap, Mapped};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Info = 3,
    Hint = 4,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 0-based line.
    pub line: u32,
    /// 0-based column in the server's position encoding.
    pub col: u32,
    pub severity: Severity,
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn from_lsp(v: &Value) -> Option<Self> {
        let start = v.get("range")?.get("start")?;
        let severity = match v.get("severity").and_then(Value::as_u64) {
            Some(2) => Severity::Warning,
            Some(3) => Severity::Info,
            Some(4) => Severity::Hint,
            // Missing severity is the client's call; treat it as an error.
            _ => Severity::Error,
        };
        let code = match v.get("code") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        Some(Self {
            line: start.get("line")?.as_u64()? as u32,
            col: start.get("character").and_then(Value::as_u64).unwrap_or(0) as u32,
            severity,
            code,
            source: v.get("source").and_then(Value::as_str).map(str::to_string),
            message: v.get("message")?.as_str()?.to_string(),
        })
    }

    /// Errors and warnings; hints and infos are never reported.
    pub fn relevant(&self) -> bool {
        matches!(self.severity, Severity::Error | Severity::Warning)
    }

    fn same_kind(&self, other: &Diagnostic) -> bool {
        self.severity == other.severity
            && self.code == other.code
            && normalize(&self.message) == normalize(&other.message)
    }

    /// `path:line:col severity[code] message` (1-based line and column).
    pub fn render(&self, display: &str) -> String {
        let code = self
            .code
            .as_deref()
            .map(|c| format!("[{c}]"))
            .unwrap_or_default();
        format!(
            "{display}:{}:{} {}{code} {}",
            self.line + 1,
            self.col + 1,
            self.severity.name(),
            short_message(&self.message)
        )
    }
}

fn normalize(msg: &str) -> String {
    msg.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First line, whitespace collapsed, at most 200 characters.
pub fn short_message(msg: &str) -> String {
    let first = msg.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let text = normalize(first);
    if text.chars().count() > 200 {
        let cut: String = text.chars().take(199).collect();
        format!("{cut}…")
    } else {
        text
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Delta {
    /// Errors and warnings present after the change but not before.
    pub new: Vec<Diagnostic>,
    /// Errors and warnings present before but gone after.
    pub fixed: usize,
}

/// What a change introduced. `before` is `None` when the server never
/// reported on the old text; then only diagnostics on the lines the change
/// wrote (and their neighbours) count as new. `map` carries old lines to new
/// ones (`None`: the file's text didn't change).
pub fn delta(before: Option<&[Diagnostic]>, after: &[Diagnostic], map: Option<&LineMap>) -> Delta {
    let after: Vec<&Diagnostic> = after.iter().filter(|d| d.relevant()).collect();
    let Some(before) = before else {
        let near_change = |line: usize| {
            map.is_some_and(|m| {
                m.is_changed(line) || (line > 0 && m.is_changed(line - 1)) || m.is_changed(line + 1)
            })
        };
        return Delta {
            new: after
                .into_iter()
                .filter(|d| near_change(d.line as usize))
                .cloned()
                .collect(),
            fixed: 0,
        };
    };
    let before: Vec<&Diagnostic> = before.iter().filter(|d| d.relevant()).collect();
    let mapped = |d: &Diagnostic| -> Mapped {
        match map {
            Some(m) => m
                .map(d.line as usize)
                .unwrap_or(Mapped::Changed(d.line as usize)),
            None => Mapped::Same(d.line as usize),
        }
    };
    let mut used = vec![false; before.len()];
    let mut matched = vec![false; after.len()];
    // Pass 1: same diagnostic on the line the old one moved to.
    for (i, d) in after.iter().enumerate() {
        if let Some(j) = (0..before.len()).find(|&j| {
            !used[j] && before[j].same_kind(d) && mapped(before[j]).line() == d.line as usize
        }) {
            used[j] = true;
            matched[i] = true;
        }
    }
    // Pass 2: nearby, or both inside the rewritten region.
    for (i, d) in after.iter().enumerate() {
        if matched[i] {
            continue;
        }
        let line = d.line as usize;
        let best = (0..before.len())
            .filter(|&j| !used[j] && before[j].same_kind(d))
            .filter_map(|j| {
                let m = mapped(before[j]);
                let dist = m.line().abs_diff(line);
                let in_edit =
                    matches!(m, Mapped::Changed(_)) && map.is_some_and(|mp| mp.is_changed(line));
                (dist <= 3 || in_edit).then_some((dist, j))
            })
            .min();
        if let Some((_, j)) = best {
            used[j] = true;
            matched[i] = true;
        }
    }
    Delta {
        new: after
            .iter()
            .zip(&matched)
            .filter(|(_, m)| !**m)
            .map(|(d, _)| (*d).clone())
            .collect(),
        fixed: used.iter().filter(|u| !**u).count(),
    }
}

/// Header plus at most `cap` lines, errors first. `groups` are (display
/// path, new diagnostics), the edited file first.
pub fn render_block(
    server: &str,
    late: bool,
    groups: &[(String, Vec<Diagnostic>)],
    fixed: usize,
    cap: usize,
) -> Option<String> {
    let mut all: Vec<(Severity, usize, u32, String)> = groups
        .iter()
        .enumerate()
        .flat_map(|(g, (display, diags))| {
            diags
                .iter()
                .map(move |d| (d.severity, g, d.line, d.render(display)))
        })
        .collect();
    all.sort();
    all.dedup_by(|a, b| a.3 == b.3);
    let errors = all.iter().filter(|x| x.0 == Severity::Error).count();
    let warnings = all.len() - errors;
    if all.is_empty() && fixed == 0 {
        return None;
    }
    let mut counts = Vec::new();
    if errors > 0 {
        counts.push(plural(errors, "new error"));
    }
    if warnings > 0 {
        counts.push(plural(warnings, "new warning"));
    }
    let when = if late { " (late)" } else { "" };
    let mut out = if counts.is_empty() {
        format!(
            "LSP {server}{when}: {} fixed, none new",
            plural(fixed, "problem")
        )
    } else {
        let fixed = if fixed > 0 {
            format!(" ({fixed} fixed)")
        } else {
            String::new()
        };
        format!("LSP {server}{when}: {}{fixed}", counts.join(", "))
    };
    let total = all.len();
    for (_, _, _, line) in all.iter().take(cap) {
        out.push('\n');
        out.push_str(line);
    }
    if total > cap {
        out.push_str(&format!("\n…and {} more", total - cap));
    }
    Some(out)
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(line: u32, sev: Severity, msg: &str) -> Diagnostic {
        Diagnostic {
            line,
            col: 4,
            severity: sev,
            code: None,
            source: None,
            message: msg.into(),
        }
    }

    #[test]
    fn parses_lsp_shapes() {
        let v = json!({"range": {"start": {"line": 41, "character": 4}, "end": {"line": 41, "character": 7}},
            "severity": 1, "code": "E0425", "source": "rustc", "message": "cannot find value `foo`\nin this scope"});
        let x = Diagnostic::from_lsp(&v).unwrap();
        assert_eq!(
            x.render("src/lib.rs"),
            "src/lib.rs:42:5 error[E0425] cannot find value `foo`"
        );
        let n = Diagnostic::from_lsp(&json!({"range": {"start": {"line": 0, "character": 0}}, "code": 7, "severity": 2, "message": "m"})).unwrap();
        assert_eq!(n.code.as_deref(), Some("7"));
        assert_eq!(n.severity, Severity::Warning);
        assert!(Diagnostic::from_lsp(&json!({"message": "no range"})).is_none());
    }

    #[test]
    fn delta_follows_lines_shifted_by_an_insert_above() {
        let old = "fn a() {}\nlet x = 1;\nlet y = bad;\n";
        let new = "// header\n// more\nfn a() {}\nlet x = 1;\nlet y = bad;\nlet z = worse;\n";
        let map = LineMap::new(old, new);
        let before = vec![
            d(2, Severity::Error, "cannot find `bad`"),
            d(1, Severity::Warning, "unused x"),
        ];
        let after = vec![
            d(4, Severity::Error, "cannot find `bad`"),
            d(3, Severity::Warning, "unused   x"),
            d(5, Severity::Error, "cannot find `worse`"),
            d(5, Severity::Hint, "ignored"),
        ];
        let r = delta(Some(&before), &after, Some(&map));
        assert_eq!(r.new, vec![d(5, Severity::Error, "cannot find `worse`")]);
        assert_eq!(r.fixed, 0);
        // A second instance of an existing message is still new.
        let after2 = vec![
            d(4, Severity::Error, "cannot find `bad`"),
            d(0, Severity::Error, "cannot find `bad`"),
        ];
        let r2 = delta(Some(&before), &after2, Some(&map));
        assert_eq!(r2.new, vec![d(0, Severity::Error, "cannot find `bad`")]);
        assert_eq!(r2.fixed, 1, "the warning went away");
    }

    #[test]
    fn delta_inside_a_rewritten_region_and_without_baseline() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB1\nB2\nB3\nB4\nB5\ne\n";
        let map = LineMap::new(old, new);
        // An error that was on the rewritten line 2 and is still somewhere in it.
        let before = vec![d(1, Severity::Error, "type mismatch")];
        let after = vec![d(5, Severity::Error, "type mismatch")];
        assert!(delta(Some(&before), &after, Some(&map)).new.is_empty());
        // No baseline: only errors on or next to written lines count.
        let after = vec![
            d(6, Severity::Error, "near"),
            d(0, Severity::Error, "old stuff"),
        ];
        let far = LineMap::new("a\nb\nc\nd\ne\nf\ng\n", "a\nb\nc\nd\ne\nf\nG\n");
        let r = delta(
            None,
            &[
                d(0, Severity::Error, "old stuff"),
                d(6, Severity::Error, "new"),
            ],
            Some(&far),
        );
        assert_eq!(r.new, vec![d(6, Severity::Error, "new")]);
        let r = delta(None, &after, Some(&map));
        assert_eq!(
            r.new.len(),
            2,
            "line 6 is next to the rewrite, line 0 is next to it too"
        );
    }

    #[test]
    fn render_caps_and_orders() {
        let groups = vec![
            (
                "a.rs".to_string(),
                (0..8)
                    .map(|i| d(i, Severity::Warning, "w"))
                    .collect::<Vec<_>>(),
            ),
            (
                "b.rs".to_string(),
                (0..5).map(|i| d(i, Severity::Error, "e")).collect(),
            ),
        ];
        let out = render_block("rust-analyzer", false, &groups, 2, 10).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "LSP rust-analyzer: 5 new errors, 8 new warnings (2 fixed)"
        );
        assert!(
            lines[1].starts_with("b.rs:1:5 error"),
            "errors first: {out}"
        );
        assert_eq!(lines.len(), 12);
        assert_eq!(lines[11], "…and 3 more");
        assert_eq!(
            render_block("x", true, &[], 1, 10).unwrap(),
            "LSP x (late): 1 problem fixed, none new"
        );
        assert!(render_block("x", false, &[], 0, 10).is_none());
    }
}
