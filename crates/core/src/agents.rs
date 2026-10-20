//! Agent definitions and structured subagent output.
//!
//! A definition is a named kind of subagent: a Markdown file whose
//! frontmatter sets `name`, `description`, `tools`, `model`, `readonly`,
//! `max_steps` and `max_minutes`, and whose body becomes the `# Agent role`
//! section of the subagent's system prompt. They come from the project
//! (`.tessera/agents/*.md`, from the working directory up to the repository
//! root), the user (`~/.config/tessera/agents/*.md`) and the built-ins, in
//! that order of precedence.
//!
//! A project's definitions come from whatever repository the agent is
//! working in, so they get the same trust as its `AGENTS.md`: their text is
//! used, but they can only narrow what a subagent may do (tools, read-only,
//! a smaller budget), never widen it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::skills::{field, frontmatter};

/// Where a definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Builtin,
    User,
    Project,
}

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    /// One line: what it's for.
    pub description: String,
    /// Tool allowlist (tool names; `prefix*` matches MCP tools). `None`
    /// keeps the full set.
    pub tools: Option<Vec<String>>,
    pub readonly: bool,
    /// Read-only helpers of this type may use the network in their
    /// sandboxed commands (`network: true`; never from a project's file).
    pub network: bool,
    /// `provider/model`, part of a model's name, or `@role`.
    pub model: Option<String>,
    pub max_steps: Option<u32>,
    pub max_minutes: Option<u32>,
    /// Instructions added to the system prompt.
    pub prompt: String,
    pub file: Option<PathBuf>,
    pub source: Source,
}

/// Engine-private record of how a subagent was started (persisted with its
/// session), so it can be continued the same way.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SubagentSpec {
    /// The label its parent gave it.
    pub name: String,
    /// Definition it runs as.
    pub agent: Option<String>,
    /// The definition's instructions (system prompt `# Agent role`).
    pub role: Option<String>,
    pub tools: Option<Vec<String>>,
    pub max_steps: Option<u32>,
    pub max_minutes: Option<u32>,
    /// Requested report format: a JSON schema (object) or a short
    /// description of the JSON wanted (string).
    pub output: Option<Value>,
    pub worktree: Option<crate::worktree::WorktreeState>,
    /// Original selected folder, bound before the helper can queue.
    pub cwd_scope: Option<crate::tools::file_object::DirectoryScope>,
    /// Waiting for a provider lane or a helper slot (also on continuations).
    pub queued: bool,
    /// A restart cut off this run; kept until an explicit continuation.
    pub interrupted_by_restart: bool,
}

/// Persisted accounting for an admitted helper run. Only explicit
/// continuation resets it; automatic restart keeps its units and deadline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperRunBudget {
    pub max_steps: u32,
    pub base: u32,
    pub used: u32,
    pub calls: std::collections::HashMap<String, HelperStep>,
    pub soft_deadline_ms: Option<u64>,
    pub wrapped: bool,
    /// Terminal run; its aggregate remains, but late callbacks cannot act.
    #[serde(default)]
    pub closed: bool,
}

/// Identity-only accounting, never another persisted copy of tool inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperStep {
    pub name: String,
    pub args: String,
    /// Released to execute (including CLI hook/permission allow).
    pub dispatched: bool,
    /// Native tool.run crossed; a hook release permits this once.
    #[serde(default)]
    pub executed: bool,
    pub pending: bool,
}
/// Hard ceilings for any definition's budget.
const MAX_STEPS: u32 = 400;
const MAX_MINUTES: u32 = 180;
/// A definition's instructions go into a system prompt; keep them bounded.
pub const MAX_PROMPT: usize = 8_000;

const EXPLORE: &str = r#"---
name: explore
description: Read-only codebase search; answers with file:line references.
tools: read, grep, glob, lsp, recall, docs
readonly: true
max_steps: 30
---
You are a read-only code explorer. Answer the question with evidence from the code, fast.
- Start broad and in parallel: glob for likely file names, grep for symbols, strings and error messages. Then read only the parts that matter.
- Follow the thread (definition, callers, configuration, tests) and stop once the question is answered.
- Every claim cites `path:line`. Never guess; if something isn't there, say where you looked.
- Report the direct answer first, then the key locations as `path:line — what is there`. No file dumps, and no advice unless asked."#;

const REVIEW: &str = r#"---
name: review
description: Code review of a diff, commits or files; ranked findings with file:line, no edits.
tools: read, grep, glob, bash, lsp, recall, docs
max_steps: 40
---
You are a senior code reviewer. Find the problems that matter; never change anything.
- Get the change: `git diff` (with `--staged` or `<base>...HEAD` if the task says so), or the files named. Read the surrounding code, callers and tests, not just the hunks.
- Look for wrong logic, unhandled errors and edge cases, races, security holes (injection, leaked secrets, unchecked input), data loss, broken contracts, and missing tests on risky paths. Skip style nits.
- Run the build or tests only when that settles a question. Never edit files, stage, commit or switch branches.
- Report findings ranked P0 (must fix) to P3 (minor), each as `path:line` — problem — why it matters — fix. Say plainly if nothing serious turned up, and end with what you did not check."#;

const RESEARCH: &str = r#"---
name: research
description: Web research from primary sources; every claim cited.
tools: web_search, web_fetch, read, grep, glob, recall
readonly: true
max_steps: 30
---
You are a careful researcher. Answer from sources, not from memory.
- Search several phrasings. Prefer primary sources: official docs, specifications, changelogs, papers, maintainers' own posts.
- Open and read the pages that matter; search snippets are not evidence. Note versions and dates, and prefer the newest authoritative source.
- Cross-check every claim the answer depends on. When sources disagree, say so and why.
- Report the answer first, then the key facts, each with its source URL. Say what stayed uncertain."#;

/// The built-in definitions.
pub fn builtins() -> Vec<AgentDef> {
    [EXPLORE, REVIEW, RESEARCH]
        .iter()
        .filter_map(|t| parse(t, "", Source::Builtin, None))
        .collect()
}

/// Every definition available in `cwd`: project, then user, then built-in
/// (earlier wins on a name clash).
pub fn discover(cwd: &Path, user_dir: &Path, protected: &crate::tools::Protected) -> Vec<AgentDef> {
    let protected = protected.canonical();
    let mut dirs = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let p = d.join(".tessera/agents");
        if p.is_dir() {
            dirs.push((p, Source::Project));
        }
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    dirs.push((user_dir.to_path_buf(), Source::User));
    let mut out: Vec<AgentDef> = Vec::new();
    for (root, source) in dirs {
        let Ok(read) = std::fs::read_dir(&root) else {
            continue;
        };
        let real_root = crate::tools::canonical(&root);
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        for path in files {
            // A repository's definition that links elsewhere (say, to
            // Tessera's settings) would put that file into a system prompt.
            let Ok(opened) = crate::tools::file_object::Opened::open(&path) else {
                continue;
            };
            let real = &opened.path;
            let untrusted = source == Source::Project
                && (!real.starts_with(&real_root)
                    || protected.covers(real)
                    || crate::tools::credential_file(real));
            if untrusted
                || crate::tools::claude_state_path(real)
                || crate::tools::credential_file(real)
            {
                continue;
            }
            let Ok(text) = opened
                .read_bounded(16 * 1024 * 1024)
                .and_then(|b| String::from_utf8(b).map_err(std::io::Error::other))
            else {
                continue;
            };
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Some(def) = parse(&text, &stem, source, Some(path.clone()))
                && !out.iter().any(|d| d.name == def.name)
            {
                out.push(def);
            }
        }
    }
    for def in builtins() {
        if !out.iter().any(|d| d.name == def.name) {
            out.push(def);
        }
    }
    out
}

/// `lowercase-with-dashes`, or `None` if nothing usable is left.
pub fn normalize_name(s: &str) -> Option<String> {
    let n: String = s
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect();
    (!n.is_empty()
        && n.len() <= 40
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
    .then_some(n)
}

/// Parse a definition file (`stem` names it when the frontmatter doesn't).
pub fn parse(text: &str, stem: &str, source: Source, file: Option<PathBuf>) -> Option<AgentDef> {
    let (fm, body) = frontmatter(text);
    let name = normalize_name(&field(&fm, "name").unwrap_or_else(|| stem.to_string()))?;
    let body = body.trim();
    let description = field(&fm, "description")
        .unwrap_or_else(|| {
            body.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .unwrap_or("")
                .to_string()
        })
        .replace('\n', " ");
    let num = |k: &str| {
        field(&fm, k)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
    };
    let prompt = if body.len() > MAX_PROMPT {
        crate::util::truncate(body, MAX_PROMPT)
    } else {
        body.to_string()
    };
    Some(AgentDef {
        name,
        description: crate::util::truncate(description.trim(), 200),
        tools: list_field(&fm, "tools").filter(|t| !t.is_empty()),
        readonly: field(&fm, "readonly").is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
        }),
        // A repository's definition may narrow, never widen.
        network: source != Source::Project
            && field(&fm, "network").is_some_and(|v| {
                matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
            }),
        model: field(&fm, "model").filter(|m| {
            let m = m.trim();
            !m.is_empty() && !m.eq_ignore_ascii_case("inherit")
        }),
        max_steps: num("max_steps").map(|n| n.min(MAX_STEPS)),
        max_minutes: num("max_minutes").map(|n| n.min(MAX_MINUTES)),
        prompt,
        file,
        source,
    })
}

/// `key: [a, b]`, `key: a, b` or a block of `- item` lines.
fn list_field(fm: &str, key: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = fm.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        let v = rest.trim();
        let items: Vec<String> = if v.is_empty() {
            lines[i + 1..]
                .iter()
                .map(|l| l.trim())
                .take_while(|l| l.starts_with('-') || l.is_empty())
                .filter_map(|l| l.strip_prefix('-'))
                .map(str::to_string)
                .collect()
        } else {
            v.trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::to_string)
                .collect()
        };
        return Some(
            items
                .iter()
                .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        );
    }
    None
}

impl AgentDef {
    /// Every definition may only narrow the user's current allowance.
    pub fn budget(&self, steps: u32, minutes: u32) -> (Option<u32>, Option<u32>) {
        (
            self.max_steps.map(|s| s.min(steps)),
            self.max_minutes.map(|m| m.min(minutes)),
        )
    }
}

/// Whether tool `name` is on an allowlist (case and `_` are ignored, so
/// `WebFetch` means `web_fetch`; `prefix*` matches a family).
pub fn tool_allowed(allow: &[String], name: &str) -> bool {
    // A read-only helper's command runner stands in for `bash`.
    if name == crate::tools::bash::READ_ONLY_BASH && tool_allowed(allow, "bash") {
        return true;
    }
    let norm = |s: &str| s.to_ascii_lowercase().replace('_', "");
    let n = norm(name);
    allow.iter().any(|a| match a.strip_suffix('*') {
        Some(prefix) => n.starts_with(&norm(prefix)),
        None => norm(a) == n,
    })
}

// ---- structured output -------------------------------------------------------

/// A requested report format from the `output` argument: a JSON schema
/// (object, or a string holding one) or a short description (string).
pub fn parse_output(v: &Value) -> Option<Value> {
    match v {
        Value::Object(o) if !o.is_empty() => Some(v.clone()),
        Value::String(s) if !s.trim().is_empty() => {
            let s = s.trim();
            match serde_json::from_str::<Value>(s) {
                Ok(schema @ Value::Object(_))
                    if schema.get("type").is_some() || schema.get("properties").is_some() =>
                {
                    Some(schema)
                }
                _ => Some(Value::String(crate::util::truncate(s, 2_000))),
            }
        }
        _ => None,
    }
}

fn describe_format(fmt: &Value) -> String {
    match fmt {
        Value::String(s) => format!("in this format: {s}"),
        schema => format!("matching this JSON schema: {schema}"),
    }
}

/// Appended to a subagent's task when a report format was requested.
pub fn output_instruction(fmt: &Value) -> String {
    format!(
        "When you are done, reply with only a JSON value {}. No prose and no code fences around it: your reply is parsed as JSON.",
        describe_format(fmt)
    )
}

/// The harness's one retry request after an unusable answer.
pub fn output_nudge(fmt: &Value, problem: &str) -> String {
    format!(
        "[Output check] Your reply could not be used: {problem}. Reply again with only the JSON value {}, and nothing else.",
        describe_format(fmt)
    )
}

/// Check a subagent's final answer against the requested format; the parsed
/// JSON on success.
pub fn check_output(fmt: &Value, answer: &str) -> Result<Value, String> {
    let v = extract_json(answer).ok_or("it contains no JSON value")?;
    if fmt.is_object() {
        validate(fmt, &v)?;
    }
    Ok(v)
}

/// The JSON object or array in a model's reply: the whole reply, a fenced
/// block, or the last complete JSON value in the text.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    let unfenced = t
        .strip_prefix("```")
        .and_then(|r| r.split_once('\n'))
        .and_then(|(_, r)| r.trim_end().strip_suffix("```"))
        .unwrap_or(t);
    if let Ok(v) = serde_json::from_str::<Value>(unfenced.trim())
        && (v.is_object() || v.is_array())
    {
        return Some(v);
    }
    let mut last = None;
    let mut i = 0;
    let mut tries = 0;
    while i < text.len() && tries < 400 {
        let Some(off) = text[i..].find(['{', '[']) else {
            break;
        };
        let start = i + off;
        tries += 1;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(v)) => {
                last = Some(v);
                i = start + stream.byte_offset().max(1);
            }
            _ => i = start + 1,
        }
    }
    last
}

/// Check `v` against a JSON-schema subset: `type`, `enum`, `const`,
/// `properties`, `required`, `additionalProperties: false` and `items`.
pub fn validate(schema: &Value, v: &Value) -> Result<(), String> {
    let mut errs = Vec::new();
    validate_at(schema, v, "$", &mut errs);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        "number" => v.is_number(),
        other => kind(v) == other,
    }
}

fn validate_at(schema: &Value, v: &Value, path: &str, errs: &mut Vec<String>) {
    if errs.len() >= 6 {
        return;
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(v)
    {
        let list: Vec<String> = options.iter().map(Value::to_string).collect();
        errs.push(format!("{path} must be one of {}", list.join(", ")));
        return;
    }
    if let Some(c) = schema.get("const")
        && c != v
    {
        errs.push(format!("{path} must be {c}"));
        return;
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !types.is_empty() && !types.iter().any(|t| type_matches(t, v)) {
        errs.push(format!(
            "{path} should be {}, not {}",
            types.join(" or "),
            kind(v)
        ));
        return;
    }
    match v {
        Value::Object(o) => {
            if let Some(req) = schema.get("required").and_then(Value::as_array) {
                for k in req.iter().filter_map(Value::as_str) {
                    if !o.contains_key(k) {
                        errs.push(format!("{path}.{k} is missing"));
                    }
                }
            }
            let props = schema.get("properties").and_then(Value::as_object);
            if let Some(props) = props {
                for (k, s) in props {
                    if let Some(x) = o.get(k) {
                        validate_at(s, x, &format!("{path}.{k}"), errs);
                    }
                }
            }
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                for k in o.keys() {
                    if props.is_none_or(|p| !p.contains_key(k)) {
                        errs.push(format!("{path}.{k} is not allowed"));
                    }
                }
            }
        }
        Value::Array(a) => {
            if let Some(items) = schema.get("items").filter(|i| i.is_object()) {
                for (i, x) in a.iter().enumerate() {
                    validate_at(items, x, &format!("{path}[{i}]"), errs);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtins_parse_with_their_settings() {
        let b = builtins();
        let names: Vec<&str> = b.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["explore", "review", "research"]);
        let explore = &b[0];
        assert!(explore.readonly);
        assert_eq!(explore.max_steps, Some(30));
        assert!(
            explore
                .tools
                .as_ref()
                .unwrap()
                .contains(&"grep".to_string())
        );
        assert!(
            explore
                .prompt
                .starts_with("You are a read-only code explorer")
        );
        assert!(!b[1].readonly, "review runs git");
        assert!(
            !b[1]
                .tools
                .as_ref()
                .unwrap()
                .iter()
                .any(|t| t == "edit" || t == "write")
        );
        for d in &b {
            assert!(d.description.len() < 120, "{}", d.description);
            assert!(
                d.prompt.len() < 1_000,
                "{} prompt is {} bytes",
                d.name,
                d.prompt.len()
            );
        }
    }

    #[test]
    fn parses_frontmatter_lists_and_numbers() {
        let text = "---\nname: Doc Writer\ndescription: Writes docs.\ntools:\n  - read\n  - \"write\"\nmodel: inherit\nreadonly: yes\nmax_steps: 5000\nmax_minutes: 7\n---\n\nBe terse.\n";
        let d = parse(text, "x", Source::User, None).unwrap();
        assert_eq!(d.name, "doc-writer");
        assert_eq!(d.tools, Some(vec!["read".into(), "write".into()]));
        assert_eq!(d.model, None);
        assert!(d.readonly);
        assert_eq!(d.max_steps, Some(MAX_STEPS));
        assert_eq!(d.max_minutes, Some(7));
        assert_eq!(d.prompt, "Be terse.");
        let d = parse(
            "---\ntools: [read, WebFetch]\nmodel: \"@fast\"\n---\nFirst line.\n",
            "helper",
            Source::Project,
            None,
        )
        .unwrap();
        assert_eq!(d.name, "helper");
        assert_eq!(d.description, "First line.");
        assert_eq!(d.model.as_deref(), Some("@fast"));
        assert!(tool_allowed(d.tools.as_ref().unwrap(), "web_fetch"));
        assert!(!tool_allowed(d.tools.as_ref().unwrap(), "bash"));
        assert!(tool_allowed(&["mcp__gh__*".into()], "mcp__gh__issues"));
        assert!(parse("---\nname: ../evil\n---\n", "", Source::User, None).is_none());
    }

    #[test]
    fn every_definition_respects_configured_helper_limits() {
        for source in [Source::Project, Source::User, Source::Builtin] {
            let d = parse(
                "---\nname: a\nmax_steps: 90\nmax_minutes: 20\n---\n",
                "",
                source,
                None,
            )
            .unwrap();
            assert_eq!(d.budget(40, 15), (Some(40), Some(15)));
            assert_eq!(d.budget(100, 30), (Some(90), Some(20)));
            assert_eq!(d.budget(0, 0), (Some(0), Some(0)));
            let d = parse("---\nname: a\n---\n", "", source, None).unwrap();
            assert_eq!(d.budget(40, 15), (None, None));
        }
    }

    #[test]
    fn project_definitions_can_only_lower_budgets() {
        let mut d = parse(
            "---\nname: a\nmax_steps: 90\nmax_minutes: 2\n---\n",
            "",
            Source::Project,
            None,
        )
        .unwrap();
        assert_eq!(d.budget(40, 15), (Some(40), Some(2)));
        d.source = Source::User;
        assert_eq!(d.budget(40, 15), (Some(40), Some(2)));
    }

    #[test]
    fn discovery_prefers_the_project_and_ignores_links_out() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let agents = repo.path().join(".tessera/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("explore.md"),
            "---\ndescription: Project explorer.\n---\nProject rules.",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("tester.md"),
            "---\ndescription: Runs tests.\n---\nTest it.",
        )
        .unwrap();
        let secret = tempfile::tempdir().unwrap();
        std::fs::write(
            secret.path().join("s.md"),
            "---\nname: leak\n---\nTOPSECRET",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(secret.path().join("s.md"), agents.join("leak.md")).unwrap();
        let sub = repo.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        let defs = discover(&sub, user.path(), &Default::default());
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["explore", "tester", "review", "research"]);
        assert_eq!(defs[0].source, Source::Project);
        assert_eq!(defs[0].prompt, "Project rules.");
        assert!(!defs.iter().any(|d| d.prompt.contains("TOPSECRET")));
    }

    #[test]
    fn json_is_found_in_replies() {
        assert_eq!(extract_json("{\"a\": 1}"), Some(json!({"a": 1})));
        assert_eq!(extract_json("```json\n[1, 2]\n```"), Some(json!([1, 2])));
        assert_eq!(
            extract_json("Here you go:\n{\"x\": {\"y\": [1]}}\nThanks {not json}"),
            Some(json!({"x": {"y": [1]}}))
        );
        assert_eq!(extract_json("no json here"), None);
        assert_eq!(extract_json("just 42"), None);
    }

    #[test]
    fn schemas_are_checked() {
        let schema = json!({"type": "object", "required": ["files"], "properties": {
            "files": {"type": "array", "items": {"type": "object", "required": ["path", "line"],
                "properties": {"path": {"type": "string"}, "line": {"type": "integer"}, "sev": {"enum": ["high", "low"]}}}}
        }, "additionalProperties": false});
        assert!(
            validate(
                &schema,
                &json!({"files": [{"path": "a.rs", "line": 3, "sev": "low"}]})
            )
            .is_ok()
        );
        let err = validate(
            &schema,
            &json!({"files": [{"path": 1, "sev": "mid"}], "extra": true}),
        )
        .unwrap_err();
        assert!(err.contains("$.files[0].line is missing"), "{err}");
        assert!(err.contains("$.files[0].path should be string"), "{err}");
        assert!(err.contains("must be one of"), "{err}");
        assert!(err.contains("$.extra is not allowed"), "{err}");
        assert!(check_output(&schema, "I found nothing").is_err());
        assert_eq!(
            check_output(&json!("list of names"), "```json\n[\"a\"]\n```").unwrap(),
            json!(["a"])
        );
        assert_eq!(
            parse_output(&json!("{\"type\": \"array\"}")),
            Some(json!({"type": "array"}))
        );
        assert_eq!(
            parse_output(&json!("names as a list")),
            Some(json!("names as a list"))
        );
        assert_eq!(parse_output(&json!("")), None);
    }
}
