//! What a model's tool calls become before they are stored and run: names and
//! arguments in the shapes the tools take, and calls a model wrote as text
//! (small local models, or a server whose chat template didn't parse them)
//! turned into real calls. Smart models rarely need any of it; small ones
//! otherwise lose a step (or the whole turn) to a near miss.

use serde_json::{Map, Value};
use tessera_proto::AssistantPart;

use crate::llm::ToolSpec;

/// Bring every tool call in `content` into the shape the offered tools take.
/// Returns how many calls changed.
pub(super) fn normalize(content: &mut [AssistantPart], tools: &[ToolSpec]) -> usize {
    normalize_reply(content, tools, false)
}

/// [`normalize`] for a reply that was `cut_off` at the output limit or not:
/// a cut reply's arguments are never completed by closing their brackets.
pub(super) fn normalize_reply(
    content: &mut [AssistantPart],
    tools: &[ToolSpec],
    cut_off: bool,
) -> usize {
    let mut changed = 0;
    for part in content.iter_mut() {
        let AssistantPart::ToolCall {
            name, arguments, ..
        } = part
        else {
            continue;
        };
        let before = (name.clone(), arguments.clone());
        if let Some(n) = resolve_name(name, tools) {
            *name = n;
        }
        repair_json(arguments);
        if let Some(spec) = tools.iter().find(|t| t.name == *name) {
            repair_leniently(name, arguments, &spec.parameters, !cut_off);
            let empty_flat_edit = name == "edit"
                && spec.parameters["properties"].get("old").is_some()
                && arguments.as_object().is_some_and(|obj| {
                    !EDIT_OLD
                        .iter()
                        .chain(["at", "lines", "replace", "edits"].iter())
                        .any(|key| obj.contains_key(*key))
                });
            // An unlocated small-form draft is refused by empty_small_edit,
            // preserving its useful hint. Never normalize a partial batch.
            if !empty_flat_edit && name == "edit" && !valid_edit_items(arguments) {
                *arguments = serde_json::json!({"__invalid_arguments": "Every edit item must include both sides; no edits were normalized."});
            } else if !empty_flat_edit {
                fix_args(name, arguments, &spec.parameters);
            }
        }
        changed += usize::from((name.clone(), arguments.clone()) != before);
    }
    changed
}

/// The offered tool a near-miss name means: `functions.read`, `Read`,
/// `read_file`, `run_shell_command`… `None` when the name is fine as it is
/// or means nothing offered.
fn resolve_name(name: &str, tools: &[ToolSpec]) -> Option<String> {
    let offered = |n: &str| tools.iter().any(|t| t.name == n);
    if offered(name) {
        return None;
    }
    let mut bare = name.trim();
    for prefix in [
        "functions.",
        "functions:",
        "function.",
        "tools.",
        "tool.",
        "default_api.",
    ] {
        if let Some(rest) = bare.strip_prefix(prefix) {
            bare = rest;
        }
    }
    if let Some(t) = tools.iter().find(|t| t.name.eq_ignore_ascii_case(bare)) {
        return Some(t.name.clone());
    }
    let lower = bare.to_ascii_lowercase().replace('-', "_");
    let alias = match lower.as_str() {
        "read_file" | "readfile" | "view" | "view_file" | "open_file" | "cat" => "read",
        "write_file" | "writefile" | "create_file" | "save_file" => "write",
        "edit_file" | "editfile" | "str_replace" | "str_replace_editor" | "multiedit"
        | "multi_edit" | "replace_in_file" | "apply_edit" => "edit",
        "shell" | "sh" | "terminal" | "run" | "run_command" | "run_shell_command"
        | "execute_command" | "exec" | "execute" | "bash_command" | "command" => "bash",
        "search_files" | "ripgrep" | "rg" | "search_code" | "grep_search" => "grep",
        "find_files" | "file_search" | "glob_files" => "glob",
        "websearch" | "search_web" | "web_search_tool" | "internet_search" => "web_search",
        "webfetch" | "fetch" | "fetch_url" | "open_url" | "read_url" | "fetch_page" => "web_fetch",
        _ => return None,
    };
    offered(alias).then(|| alias.to_string())
}

/// Arguments that weren't a JSON object as sent: in a code fence, encoded
/// twice (a JSON string holding the object), or with trailing commas.
fn repair_json(args: &mut Value) {
    if let Value::String(s) = args {
        if let Some(v) = parse_object(s) {
            *args = v;
        }
        return;
    }
    let Some(raw) = args.get("__invalid_arguments").and_then(Value::as_str) else {
        return;
    };
    if let Some(v) = parse_object(raw) {
        *args = v;
    }
}

/// `raw` as a JSON object, leniently (fences, double encoding, trailing
/// commas).
fn parse_object(raw: &str) -> Option<Value> {
    let mut t = raw.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
        t = rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    let parsed = serde_json::from_str::<Value>(t)
        .ok()
        .or_else(|| serde_json::from_str::<Value>(&drop_trailing_commas(t)).ok())?;
    match parsed {
        v @ Value::Object(_) => Some(v),
        Value::String(inner) => match serde_json::from_str::<Value>(inner.trim()) {
            Ok(v @ Value::Object(_)) => Some(v),
            _ => None,
        },
        _ => None,
    }
}

/// Arguments that still aren't JSON after [`repair_json`], made JSON the way
/// models get it wrong (see [`lenient_object`]); `close`: unclosed brackets
/// at the end may be closed (never for a reply cut off). Taken only when
/// the result names every key the tool requires (after the usual other
/// names), so a repair can't turn a broken call into a different one; and
/// where brackets had to be closed, when it is complete (see
/// [`complete`]): a call that stopped between keys must not become an
/// edit that deletes.
fn repair_leniently(tool: &str, args: &mut Value, schema: &Value, close: bool) {
    let raw = match &*args {
        Value::String(s) => s.clone(),
        v => match v.get("__invalid_arguments").and_then(Value::as_str) {
            Some(raw) => raw.to_string(),
            None => return,
        },
    };
    let Some((candidate, closed)) = lenient_object(&raw, close) else {
        return;
    };
    if acceptable(tool, &candidate, schema, closed) {
        *args = candidate;
    }
}

/// Whether a leniently parsed call may stand (see [`repair_leniently`]).
fn acceptable(tool: &str, candidate: &Value, schema: &Value, closed: bool) -> bool {
    if tool == "edit" && !valid_edit_items(candidate) {
        return false;
    }
    let mut fixed = candidate.clone();
    fix_args(tool, &mut fixed, schema);
    let flat_complete = tool != "edit"
        || PAIRS
            .iter()
            .all(|(a, b)| fixed.get(*a).is_none() || fixed.get(*b).is_some());
    names_required(&fixed, schema) && (!closed || (flat_complete && complete(&fixed, schema)))
}

/// Pairs of arguments where the first without the second means the call
/// stopped half-way: `old` without `new` would delete the text, `at`
/// without `text` the lines (the flat edit forms fill them in as "").
const PAIRS: [(&str, &str); 2] = [("old", "new"), ("at", "text")];

/// `args` names everything the schema requires at every level.
fn complete(args: &Value, schema: &Value) -> bool {
    if let Value::Array(items) = args {
        return schema
            .get("items")
            .is_none_or(|s| items.iter().all(|i| complete(i, s)));
    }
    let Some(obj) = args.as_object() else {
        return schema
            .get("required")
            .is_none_or(|r| r.as_array().is_none_or(Vec::is_empty));
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .all(|k| obj.contains_key(k))
        })
        .unwrap_or(true);
    let props = schema.get("properties").and_then(Value::as_object);
    required
        && obj
            .iter()
            .all(|(k, v)| props.and_then(|p| p.get(k)).is_none_or(|s| complete(v, s)))
}

/// `args` has every key `schema` requires (any key at all when it requires
/// none).
fn names_required(args: &Value, schema: &Value) -> bool {
    let Some(obj) = args.as_object() else {
        return false;
    };
    match schema.get("required").and_then(Value::as_array) {
        Some(req) if !req.is_empty() => req
            .iter()
            .filter_map(Value::as_str)
            .all(|k| obj.contains_key(k)),
        _ => !obj.is_empty(),
    }
}

/// Bounded lenient parsing of a JSON object, for what models get wrong:
/// special tokens (`<|call|>`) or a code fence around it; raw newlines,
/// tabs and other control characters inside strings; single-quoted strings
/// and keys, bare keys, `\'`; Python's `True`/`False`/`None`; trailing
/// commas; and, only when `close`, brackets left open at the end. Never
/// when it ends inside a string (cut off), and `None` unless the result is
/// an object.
/// Returns the object and whether brackets had to be closed.
fn lenient_object(raw: &str, close: bool) -> Option<(Value, bool)> {
    let mut t = raw.trim();
    loop {
        let before = t.len();
        // `<|tool_call_end|>`, `<|call|>`, DeepSeek's `<｜tool▁call▁end｜>`…
        let token = |s: &str| {
            s.len() <= 64 && !s.contains(|c: char| c.is_whitespace() || "<>|".contains(c))
        };
        if let Some(rest) = t.strip_prefix("<|")
            && let Some(end) = rest.find("|>")
            && token(&rest[..end])
        {
            t = rest[end + 2..].trim_start();
        }
        if t.ends_with("|>")
            && let Some(start) = t.rfind("<|")
            && token(&t[start + 2..t.len() - 2])
        {
            t = t[..start].trim_end();
        }
        if let Some(rest) = t.strip_prefix("```") {
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
            t = rest.strip_suffix("```").unwrap_or(rest).trim();
        }
        if t.len() == before {
            break;
        }
    }
    if !t.starts_with('{') || t.len() > 4 * 1024 * 1024 {
        return None;
    }
    let chars: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len() + 16);
    let mut open: Vec<char> = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if escaped {
                escaped = false;
                if c == '\'' {
                    // `\'` isn't a JSON escape: the quote itself.
                    out.pop();
                }
                out.push(c);
            } else if c == '\\' {
                escaped = true;
                out.push(c);
            } else if c == q {
                quote = None;
                out.push('"');
            } else {
                match c {
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push('"');
            }
            '{' => {
                open.push('}');
                out.push(c);
            }
            '[' => {
                open.push(']');
                out.push(c);
            }
            '}' | ']' => {
                if open.pop() != Some(c) {
                    return None;
                }
                out.push(c);
                if open.is_empty() {
                    // Text after the object: nothing to guess about.
                    if chars[i + 1..].iter().any(|c| !c.is_whitespace()) {
                        return None;
                    }
                    break;
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut j = i;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let word: String = chars[i..j].iter().collect();
                let key = chars[j..]
                    .iter()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(|c| *c == ':');
                match word.as_str() {
                    _ if key => out.push_str(&format!("\"{word}\"")),
                    "True" => out.push_str("true"),
                    "False" => out.push_str("false"),
                    "None" => out.push_str("null"),
                    w => out.push_str(w),
                }
                i = j;
                continue;
            }
            c => out.push(c),
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    let closed = !open.is_empty();
    if closed {
        if !close {
            return None;
        }
        while let Some(c) = open.pop() {
            out.push(c);
        }
    }
    match serde_json::from_str::<Value>(&drop_trailing_commas(&out)).ok()? {
        v @ Value::Object(_) => Some((v, closed)),
        _ => None,
    }
}

/// `,` right before a closing bracket, outside strings.
fn drop_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        } else if c == ','
            && matches!(
                chars[i + 1..].iter().find(|x| !x.is_whitespace()),
                Some('}') | Some(']')
            )
        {
            continue;
        }
        out.push(c);
    }
    out
}

const EDIT_OLD: &[&str] = &["old", "old_string", "old_str", "old_text", "search"];
const EDIT_NEW: &[&str] = &[
    "new",
    "new_string",
    "new_str",
    "new_text",
    "replacement",
    "replace_with",
];

/// Validate every raw edit item before aliases or list shapes can be changed.
/// A malformed mixed batch is never normalized into a valid subset.
fn valid_edit_items(args: &Value) -> bool {
    let Some(obj) = args.as_object() else {
        return false;
    };
    let one = |obj: &Map<String, Value>, names: &[&str]| {
        let mut found = names.iter().filter_map(|n| obj.get(*n));
        let value = found.next();
        value.is_some_and(Value::is_string) && found.next().is_none()
    };
    let old = EDIT_OLD;
    let new = EDIT_NEW;
    let flags = |o: &Map<String, Value>| {
        let values: Vec<_> = ["all", "replace_all", "replaceAll"]
            .iter()
            .filter_map(|k| o.get(*k))
            .collect();
        values.len() <= 1
            && values
                .iter()
                .all(|v| v.is_boolean() || matches!(v.as_str(), Some("true" | "false")))
    };
    let replace_item = |item: &Value| {
        item.as_object()
            .is_some_and(|o| one(o, old) && one(o, new) && flags(o))
    };
    let line_item = |item: &Value| {
        item.as_object().is_some_and(|o| {
            o.get("at")
                .is_some_and(|a| a.is_string() || a.as_i64().is_some())
                && o.get("text").is_some_and(Value::is_string)
        })
    };
    let list = |v: &Value, valid: &dyn Fn(&Value) -> bool| match v {
        Value::Array(items) => !items.is_empty() && items.iter().all(valid),
        Value::Object(_) => valid(v),
        _ => false,
    };
    let flat_replace = old.iter().chain(new).any(|k| obj.contains_key(*k));
    let flat_lines = obj.contains_key("at") || obj.contains_key("text");
    let modes = [
        obj.contains_key("replace"),
        obj.contains_key("edits"),
        obj.contains_key("lines"),
        flat_replace,
        flat_lines,
    ];
    if modes.into_iter().filter(|m| *m).count() > 1 {
        return false;
    }
    if !flags(obj) {
        return false;
    }
    if flat_replace && !(one(obj, old) && one(obj, new)) {
        return false;
    }
    if flat_lines && !line_item(args) {
        return false;
    }
    for key in ["replace", "edits"] {
        if let Some(v) = obj.get(key)
            && !list(v, &replace_item)
        {
            return false;
        }
    }
    obj.get("lines").is_none_or(|v| list(v, &line_item))
}
/// Common other names for the tools' arguments, and values of the wrong JSON
/// type that clearly mean the right one ("12" for 12, 12 for "12").
fn fix_args(tool: &str, args: &mut Value, schema: &Value) {
    if tool == "edit" && !valid_edit_items(args) {
        return;
    }
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    let props = schema.get("properties").and_then(Value::as_object);
    let has = |k: &str| props.is_some_and(|p| p.contains_key(k));
    let rename = |obj: &mut Map<String, Value>, to: &str, from: &[&str]| {
        if !has(to) || obj.contains_key(to) {
            return;
        }
        if let Some(k) = from.iter().find(|k| !has(k) && obj.contains_key(**k))
            && let Some(v) = obj.remove(*k)
        {
            obj.insert(to.to_string(), v);
        }
    };
    rename(
        obj,
        "path",
        &[
            "file_path",
            "filepath",
            "file",
            "filename",
            "target_file",
            "absolute_path",
            "dir",
            "directory",
        ],
    );
    rename(
        obj,
        "command",
        &["cmd", "script", "shell_command", "commands"],
    );
    if tool == "read" && has("paths") {
        rename(obj, "paths", &["files", "file_paths", "filepaths"]);
        // Several files given under `path`.
        if !obj.contains_key("paths")
            && obj.get("path").is_some_and(Value::is_array)
            && let Some(v) = obj.remove("path")
        {
            obj.insert("paths".into(), v);
        }
    }
    if tool == "write" {
        rename(
            obj,
            "content",
            &["contents", "file_text", "text", "data", "body"],
        );
    }
    if tool == "edit" {
        edit_shapes(obj);
    }
    if let Some(props) = props {
        coerce(obj, props);
    }
    if tool == "edit" {
        strip_copied_numbers(obj);
    }
}

/// Edits sent the way other harnesses take them: `old_string`/`new_string`
/// (Claude Code), `old_str`/`new_str`, or a MultiEdit `edits` list.
fn edit_shapes(obj: &mut Map<String, Value>) {
    let old = EDIT_OLD;
    let new = EDIT_NEW;
    let pair = |item: &mut Map<String, Value>| {
        let old = old.iter().find_map(|k| item.remove(*k));
        let new = new.iter().find_map(|k| item.remove(*k));
        let all = item
            .remove("all")
            .or_else(|| item.remove("replace_all"))
            .or_else(|| item.remove("replaceAll"));
        let (Some(old), Some(new)) = (old, new) else {
            return None;
        };
        let mut m = Map::new();
        m.insert("old".into(), old);
        m.insert("new".into(), new);
        if let Some(all) = all {
            m.insert("all".into(), all);
        }
        Some(Value::Object(m))
    };
    if !obj.contains_key("replace") && !obj.contains_key("lines") {
        if let Some(edits) = obj.remove("edits") {
            let edits = match edits {
                Value::Array(items) => items,
                v @ Value::Object(_) => vec![v],
                _ => unreachable!("validated edit list"),
            };
            let items: Vec<Value> = edits
                .into_iter()
                .map(|e| match e {
                    Value::Object(mut m) => pair(&mut m).expect("validated edit pair"),
                    _ => unreachable!("validated edit item"),
                })
                .collect();
            obj.insert("replace".into(), Value::Array(items));
        } else if old[1..]
            .iter()
            .chain(&new[1..])
            .any(|k| obj.contains_key(*k))
            && let Some(item) = pair(obj)
        {
            obj.insert("replace".into(), Value::Array(vec![item]));
        }
    }
    // One edit given as an object instead of a list of them.
    for key in ["replace", "lines"] {
        if let Some(v @ Value::Object(_)) = obj.get_mut(key) {
            *v = Value::Array(vec![v.take()]);
        }
    }
    // `all` said once for the whole call (`"all": true`, Claude Code's
    // `replace_all`), or under another name inside an item.
    let all = ["all", "replace_all", "replaceAll"]
        .iter()
        .find_map(|k| obj.remove(*k));
    if let Some(Value::Array(items)) = obj.get_mut("replace") {
        for it in items.iter_mut() {
            if let Value::Object(m) = it {
                *it = pair(m).expect("validated edit pair");
            }
            if let Value::Object(m) = it
                && !m.contains_key("all")
                && let Some(v) = m
                    .remove("replace_all")
                    .or_else(|| m.remove("replaceAll"))
                    .or_else(|| all.clone())
            {
                m.insert("all".into(), v);
            }
        }
    }
}

/// Values of the wrong JSON type, where the schema says what they should be.
fn coerce(obj: &mut Map<String, Value>, props: &Map<String, Value>) {
    for (key, value) in obj.iter_mut() {
        let Some(prop) = props.get(key) else { continue };
        let want = prop.get("type").and_then(Value::as_str).unwrap_or("");
        let fixed = match (want, &*value) {
            ("integer", Value::String(s)) => s.trim().parse::<i64>().ok().map(Value::from),
            ("number", Value::String(s)) => s.trim().parse::<f64>().ok().map(Value::from),
            ("string", Value::Number(n)) => Some(Value::String(n.to_string())),
            ("string", Value::Bool(b)) => Some(Value::String(b.to_string())),
            ("boolean", Value::String(s)) => match s.trim() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            ("array", Value::String(s)) if s.trim_start().starts_with('[') => {
                serde_json::from_str::<Value>(s)
                    .ok()
                    .filter(Value::is_array)
            }
            ("object", Value::String(s)) => parse_object(s),
            _ => None,
        };
        if let Some(v) = fixed {
            *value = v;
        }
        // Lists of objects: their fields too (edit `lines`, `task` `update`).
        if let (Some(item_props), Value::Array(items)) = (
            prop.get("items")
                .and_then(|i| i.get("properties"))
                .and_then(Value::as_object),
            &mut *value,
        ) {
            for it in items.iter_mut() {
                if let Value::Object(m) = it {
                    coerce(m, item_props);
                }
            }
        }
    }
}

/// `read` shows lines as `12:content`; a model that copies them into an
/// edit with the numbers kept would never match. Only when every line
/// carries a number and they count up one by one (two lines or more), or a
/// single line repeats the number its `at` names.
fn strip_copied_numbers(obj: &mut Map<String, Value>) {
    if let Some(Value::Array(items)) = obj.get_mut("replace") {
        for it in items.iter_mut() {
            for key in ["old", "new"] {
                if let Some(Value::String(s)) = it.get_mut(key)
                    && let Some(clean) = unnumbered(s, None)
                {
                    *s = clean;
                }
            }
        }
    }
    if let Some(Value::Array(items)) = obj.get_mut("lines") {
        for it in items.iter_mut() {
            let at = it
                .get("at")
                .and_then(Value::as_str)
                .and_then(|a| a.trim_start_matches(['<', '>']).split('-').next())
                .and_then(|n| n.trim().parse::<u64>().ok());
            if let Some(Value::String(s)) = it.get_mut("text")
                && let Some(clean) = unnumbered(s, at)
            {
                *s = clean;
            }
        }
    }
}

fn unnumbered(s: &str, at: Option<u64>) -> Option<String> {
    let lines: Vec<&str> = s.split('\n').collect();
    let mut nums = Vec::with_capacity(lines.len());
    let mut rest = Vec::with_capacity(lines.len());
    for l in &lines {
        let (n, r) = l.split_once(':')?;
        let n: u64 = n.trim().parse().ok()?;
        nums.push(n);
        rest.push(r);
    }
    let counting = nums.windows(2).all(|w| w[1] == w[0] + 1);
    let ok = match nums.len() {
        0 => false,
        1 => at == Some(nums[0]),
        _ => counting,
    };
    ok.then(|| rest.join("\n"))
}

/// GPT and Codex models' own edit form, for those models only (see
/// `tools/patch.rs`): an `apply_patch` call (made with the patch as its
/// input, or written as a shell command such as `apply_patch <<'EOF' …`),
/// or `edit` with the patch under another key, becomes `edit` with `patch`.
/// Only where `edit` is offered. Returns how many calls changed.
pub(super) fn patch_calls(content: &mut [AssistantPart], tools: &[ToolSpec]) -> usize {
    use crate::tools::patch::looks_like_patch;
    if !tools.iter().any(|t| t.name == "edit") {
        return 0;
    }
    let mut changed = 0;
    for part in content.iter_mut() {
        let AssistantPart::ToolCall {
            name, arguments, ..
        } = part
        else {
            continue;
        };
        let text = |v: &Value, keys: &[&str]| -> Option<String> {
            match v {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => keys
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))
                    .map(str::to_string),
                _ => None,
            }
        };
        let bare = name
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase()
            .replace('-', "_");
        let patch = match name.as_str() {
            "bash" => arguments
                .get("command")
                .and_then(Value::as_str)
                .and_then(patch_in_command),
            "edit" if arguments.get("replace").is_none() && arguments.get("lines").is_none() => {
                text(arguments, &["patch", "input", "diff"])
            }
            _ if matches!(bare.as_str(), "apply_patch" | "applypatch") => text(
                arguments,
                &[
                    "input",
                    "patch",
                    "diff",
                    "content",
                    "text",
                    "__invalid_arguments",
                ],
            ),
            _ => None,
        };
        let Some(patch) = patch.filter(|p| looks_like_patch(p)) else {
            continue;
        };
        let first = crate::tools::patch::parse(&patch)
            .ok()
            .and_then(|ops| crate::tools::patch::paths(&ops).into_iter().next())
            .unwrap_or_default();
        *name = "edit".into();
        *arguments = serde_json::json!({ "path": first, "patch": patch });
        changed += 1;
    }
    changed
}

/// The patch a shell command hands to `apply_patch` (a here-document, a
/// quoted argument, or piped in), when running it is all the command does.
fn patch_in_command(cmd: &str) -> Option<String> {
    let t = cmd.trim();
    let runs = t.starts_with("apply_patch")
        || t.starts_with("applypatch")
        || t.lines()
            .next()
            .is_some_and(|l| l.contains("| apply_patch") || l.contains("|apply_patch"));
    if !runs {
        return None;
    }
    let start = t.find("*** Begin Patch")?;
    let end = t[start..].find("*** End Patch")? + start + "*** End Patch".len();
    Some(t[start..end].to_string())
}

/// Tool calls written into the reply's text instead of made: Hermes/Qwen
/// `<tool_call>{"name": …, "arguments": …}</tool_call>`, Qwen3-Coder
/// `<function=name><parameter=key>value</parameter></function>`, or a reply
/// that is nothing but such a JSON object. Only offered tools count. The
/// calls replace that text; what was said before them stays. Returns how
/// many calls were recovered.
pub(super) fn recover_text_calls(content: &mut Vec<AssistantPart>, tools: &[ToolSpec]) -> usize {
    if tools.is_empty()
        || content
            .iter()
            .any(|p| matches!(p, AssistantPart::ToolCall { .. }))
    {
        return 0;
    }
    let mut found = Vec::new();
    for part in content.iter_mut() {
        if let AssistantPart::Text { text } = part {
            let (kept, calls) = extract(text, tools);
            if !calls.is_empty() {
                *text = kept;
                found.extend(calls);
            }
        }
    }
    // A reply with no text whose reasoning ends in a call (the server took
    // the call for part of the reasoning).
    if found.is_empty()
        && !content
            .iter()
            .any(|p| matches!(p, AssistantPart::Text { text } if !text.trim().is_empty()))
        && let Some(AssistantPart::Thinking { text, .. }) = content
            .iter()
            .rev()
            .find(|p| matches!(p, AssistantPart::Thinking { .. }))
    {
        let t = text.trim_end();
        if t.ends_with("</tool_call>") || t.ends_with("</function>") {
            let start = t.rfind("<tool_call>").or_else(|| t.rfind("<function="));
            if let Some(start) = start {
                let (_, calls) = extract(&t[start..], tools);
                found.extend(calls);
            }
        }
    }
    let n = found.len();
    content.retain(|p| !matches!(p, AssistantPart::Text { text } if text.trim().is_empty()));
    for (name, arguments) in found {
        content.push(AssistantPart::ToolCall {
            id: format!("call_{}", crate::util::random_token(9)),
            name,
            arguments,
        });
    }
    n
}

/// The calls written in `text` and the text without them.
fn extract(text: &str, tools: &[ToolSpec]) -> (String, Vec<(String, Value)>) {
    let mut calls = Vec::new();
    let mut kept = String::new();
    let mut rest = text;
    loop {
        let open = [rest.find("<tool_call>"), rest.find("<function=")]
            .into_iter()
            .flatten()
            .min();
        let Some(open) = open else {
            kept.push_str(rest);
            break;
        };
        let tail = &rest[open..];
        let (block, after) = if let Some(body) = tail.strip_prefix("<tool_call>") {
            match body.find("</tool_call>") {
                Some(end) => (&body[..end], &body[end + "</tool_call>".len()..]),
                // Cut off at the end of the reply: what is there may be whole.
                None => (body, ""),
            }
        } else {
            match tail.find("</function>") {
                Some(end) => (
                    &tail[..end + "</function>".len()],
                    &tail[end + "</function>".len()..],
                ),
                None => (tail, ""),
            }
        };
        // A block that ended with its closing tag wasn't cut off: a bracket
        // it left open may be closed.
        let closed = !after.is_empty() || tail.trim_end().ends_with("</tool_call>");
        match parse_block(block, tools, closed) {
            Some(call) => {
                kept.push_str(&rest[..open]);
                calls.push(call);
            }
            None => kept.push_str(&rest[..open + (tail.len() - after.len())]),
        }
        rest = after;
    }
    if calls.is_empty()
        && let Some(call) = parse_block(text, tools, false).filter(|_| {
            let t = text.trim();
            t.starts_with('{') || t.starts_with("```")
        })
    {
        return (String::new(), vec![call]);
    }
    (kept.trim_end().to_string(), calls)
}

/// One written call: a JSON object with `name` and `arguments` (or
/// `parameters`), or Qwen3-Coder's XML-ish form. JSON that only parses
/// leniently (see [`lenient_object`]; `closed`: the block ended with its
/// closing tag) counts only when its arguments name every key the tool
/// requires.
fn parse_block(block: &str, tools: &[ToolSpec], closed: bool) -> Option<(String, Value)> {
    let b = block.trim();
    if let Some(v) = parse_object(b) {
        return json_call(v, tools);
    }
    if let Some((v, brackets)) = lenient_object(b, closed) {
        let (name, args) = json_call(v, tools)?;
        let spec = tools.iter().find(|t| t.name == name)?;
        return acceptable(&name, &args, &spec.parameters, brackets).then_some((name, args));
    }
    xml_call(b, tools)
}

/// A written call as a JSON object `{name, arguments}`.
fn json_call(v: Value, tools: &[ToolSpec]) -> Option<(String, Value)> {
    let name = v.get("name").and_then(Value::as_str)?;
    let name = resolve_name(name, tools).unwrap_or_else(|| name.to_string());
    tools.iter().any(|t| t.name == name).then_some(())?;
    let args = v
        .get("arguments")
        .or_else(|| v.get("parameters"))
        .or_else(|| v.get("input"))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let args = match args {
        Value::String(s) => parse_object(&s).unwrap_or(Value::Object(Map::new())),
        v @ Value::Object(_) => v,
        _ => return None,
    };
    Some((name, args))
}

/// Qwen3-Coder's `<function=name><parameter=key>value</parameter></function>`.
fn xml_call(b: &str, tools: &[ToolSpec]) -> Option<(String, Value)> {
    let body = b.strip_prefix("<function=")?;
    let (name, mut params) = body.split_once('>')?;
    let name = name.trim();
    let name = resolve_name(name, tools).unwrap_or_else(|| name.to_string());
    let spec = tools.iter().find(|t| t.name == name)?;
    let props = spec.parameters.get("properties").and_then(Value::as_object);
    let mut args = Map::new();
    while let Some(start) = params.find("<parameter=") {
        let p = &params[start + "<parameter=".len()..];
        let (key, value) = p.split_once('>')?;
        let (value, next) = match value.find("</parameter>") {
            Some(end) => (&value[..end], &value[end + "</parameter>".len()..]),
            None => (value.split("</function>").next().unwrap_or(value), ""),
        };
        let key = key.trim().to_string();
        let raw = value.strip_prefix('\n').unwrap_or(value);
        let raw = raw.strip_suffix('\n').unwrap_or(raw);
        let want = props
            .and_then(|p| p.get(&key))
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("string");
        let v = match want {
            "string" => Value::String(raw.to_string()),
            _ => {
                serde_json::from_str(raw.trim()).unwrap_or_else(|_| Value::String(raw.to_string()))
            }
        };
        args.insert(key, v);
        params = next;
    }
    Some((name, Value::Object(args)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<ToolSpec> {
        let spec = |name: &str, parameters: Value| ToolSpec {
            name: name.into(),
            description: String::new(),
            parameters,
        };
        vec![
            spec(
                "read",
                json!({"type": "object", "properties": {"path": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}, "required": ["path"]}),
            ),
            spec(
                "write",
                json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
            ),
            spec(
                "edit",
                json!({"type": "object", "properties": {
                    "path": {"type": "string"}, "tag": {"type": "string"},
                    "lines": {"type": "array", "items": {"type": "object", "properties": {"at": {"type": "string"}, "text": {"type": "string"}}, "required": ["at", "text"]}},
                    "replace": {"type": "array", "items": {"type": "object", "properties": {"old": {"type": "string"}, "new": {"type": "string"}, "all": {"type": "boolean"}}, "required": ["old", "new"]}}
                }, "required": ["path"]}),
            ),
            spec(
                "bash",
                json!({"type": "object", "properties": {"command": {"type": "string"}, "timeout": {"type": "integer"}}, "required": ["command"]}),
            ),
            spec(
                "task",
                json!({"type": "object", "properties": {"update": {"type": "array", "items": {"type": "object", "properties": {"index": {"type": "integer"}, "status": {"type": "string"}}}}}}),
            ),
        ]
    }

    fn call(name: &str, arguments: Value) -> AssistantPart {
        AssistantPart::ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments,
        }
    }

    fn one(name: &str, arguments: Value) -> (String, Value) {
        let mut content = vec![call(name, arguments)];
        normalize(&mut content, &tools());
        match content.remove(0) {
            AssistantPart::ToolCall {
                name, arguments, ..
            } => (name, arguments),
            _ => unreachable!(),
        }
    }

    /// Exercise both provider stream parsers and the exact repair/synthesis
    /// gates used by the engine, for native and text-form partial calls.
    #[tokio::test]
    async fn provider_terminations_require_affirmative_success() {
        use crate::llm::{LlmRequest, Msg};
        use tessera_proto::{ContentPart, StopReason, ThinkingLevel};
        use tokio_util::sync::CancellationToken;
        for protocol in ["anthropic", "responses", "codex"] {
            for reason in [
                "missing",
                "null",
                "number",
                "unknown",
                "failed",
                "cancelled",
                "incomplete",
                "max_tokens",
                "end_turn",
                "stop_sequence",
                "tool_use",
                "completed",
            ] {
                for text_form in [false, true] {
                    let raw = r#"{"command":"printf nope""#;
                    let text =
                        format!("<tool_call>{{\"name\":\"bash\",\"arguments\":{raw}}}</tool_call>");
                    let mut terminal = json!({});
                    let value = match reason {
                        "null" => Value::Null,
                        "number" => json!(3),
                        _ => json!(reason),
                    };
                    if reason != "missing" {
                        terminal[if protocol == "anthropic" {
                            "stop_reason"
                        } else {
                            "status"
                        }] = value;
                    }
                    let events = if protocol == "anthropic" {
                        let (block, delta) = if text_form {
                            (
                                json!({"type":"text"}),
                                json!({"type":"text_delta", "text":text}),
                            )
                        } else {
                            (
                                json!({"type":"tool_use", "id":"c1", "name":"bash"}),
                                json!({"type":"input_json_delta", "partial_json":raw}),
                            )
                        };
                        vec![
                            json!({"type":"content_block_start", "index":0, "content_block":block}),
                            json!({"type":"content_block_delta", "index":0, "delta":delta}),
                            json!({"type":"message_delta", "delta":terminal}),
                            json!({"type":"message_stop"}),
                        ]
                    } else {
                        let (item, delta) = if text_form {
                            (
                                json!({"id":"item", "type":"message"}),
                                json!({"type":"response.output_text.delta", "item_id":"item", "delta":text}),
                            )
                        } else {
                            (
                                json!({"id":"item", "type":"function_call", "call_id":"c1", "name":"bash"}),
                                json!({"type":"response.function_call_arguments.delta", "item_id":"item", "delta":raw}),
                            )
                        };
                        vec![
                            json!({"type":"response.output_item.added", "item":item}),
                            delta,
                            json!({"type":"response.done", "response":terminal}),
                        ]
                    };
                    let body: String = events.iter().map(|v| format!("data: {v}\n\n")).collect();
                    let app = axum::Router::new().route(
                        if protocol == "anthropic" {
                            "/v1/messages"
                        } else {
                            "/v1/responses"
                        },
                        axum::routing::post(move || {
                            let body = body.clone();
                            async move { ([("content-type", "text/event-stream")], body) }
                        }),
                    );
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let base = format!("http://{}", listener.local_addr().unwrap());
                    let server =
                        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                    let http = reqwest::Client::new();
                    let req = LlmRequest {
                        model: "m".into(),
                        system: "s".into(),
                        messages: vec![Msg::User(vec![ContentPart::text("go")])],
                        tools: tools(),
                        thinking: ThinkingLevel::Off,
                        max_output: None,
                        cache_key: "k".into(),
                        fast: false,
                        daybreak: false,
                        no_tool_calls: false,
                    };
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    let cancel = CancellationToken::new();
                    let mut reply = if protocol == "anthropic" {
                        crate::llm::anthropic::stream(
                            &crate::llm::anthropic::Endpoint {
                                http: &http,
                                base_url: &base,
                                headers: &[],
                                max_output: 1024,
                            },
                            &req,
                            &tx,
                            &cancel,
                        )
                        .await
                        .unwrap()
                    } else {
                        let url = format!("{base}/v1");
                        crate::llm::responses::stream(
                            &crate::llm::responses::Endpoint {
                                http: &http,
                                base_url: &url,
                                headers: &[],
                                reasoning: false,
                                vision: false,
                                codex: protocol == "codex",
                                fast: false,
                                cyber: None,
                                limits: None,
                            },
                            &req,
                            &tx,
                            &cancel,
                        )
                        .await
                        .unwrap()
                    };
                    server.abort();
                    let success = if protocol == "anthropic" {
                        matches!(reason, "end_turn" | "stop_sequence" | "tool_use")
                    } else {
                        reason == "completed"
                    };
                    let cut = !reply.allow_tool_repair
                        || !matches!(reply.stop, StopReason::Stop | StopReason::ToolUse);
                    assert_eq!(!cut, success, "{protocol} {reason} text={text_form}");
                    if !cut {
                        recover_text_calls(&mut reply.content, &req.tools);
                    }
                    normalize_reply(&mut reply.content, &req.tools, cut);
                    let calls: Vec<_> = reply
                        .content
                        .iter()
                        .filter_map(|p| {
                            if let AssistantPart::ToolCall { arguments, .. } = p {
                                Some(arguments)
                            } else {
                                None
                            }
                        })
                        .collect();
                    if text_form && !success {
                        assert!(calls.is_empty(), "{protocol} {reason}");
                    } else {
                        assert_eq!(calls.len(), 1);
                        assert_eq!(
                            calls[0].get("command").is_some(),
                            success,
                            "{protocol} {reason}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn near_miss_names_become_the_offered_tool() {
        assert_eq!(one("Read", json!({"path": "a"})).0, "read");
        assert_eq!(one("functions.bash", json!({"command": "ls"})).0, "bash");
        assert_eq!(one("read_file", json!({"path": "a"})).0, "read");
        assert_eq!(one("run_shell_command", json!({"command": "ls"})).0, "bash");
        // Not offered here, so not guessed.
        assert_eq!(one("web_search", json!({})).0, "web_search");
        assert_eq!(one("launch_rockets", json!({})).0, "launch_rockets");
    }

    #[test]
    fn arguments_in_other_shapes_are_taken() {
        // Encoded twice, fenced, trailing comma.
        assert_eq!(
            one("read", json!("{\"path\": \"a.rs\"}")).1,
            json!({"path": "a.rs"})
        );
        assert_eq!(
            one(
                "read",
                json!({"__invalid_arguments": "```json\n{\"path\": \"a.rs\",}\n```"})
            )
            .1,
            json!({"path": "a.rs"})
        );
        // Other harnesses' names.
        assert_eq!(
            one("Read", json!({"file_path": "a.rs", "offset": "20"})).1,
            json!({"path": "a.rs", "offset": 20})
        );
        assert_eq!(
            one("Edit", json!({"file_path": "a.rs", "old_string": "x", "new_string": "y", "replace_all": true})).1,
            json!({"path": "a.rs", "replace": [{"old": "x", "new": "y", "all": true}]})
        );
        assert_eq!(
            one(
                "MultiEdit",
                json!({"file_path": "a.rs", "edits": [{"old_string": "x", "new_string": "y"}]})
            )
            .1,
            json!({"path": "a.rs", "replace": [{"old": "x", "new": "y"}]})
        );
        // One edit as an object; line numbers as numbers; task indexes as strings.
        assert_eq!(
            one(
                "edit",
                json!({"path": "a.rs", "lines": {"at": 12, "text": "x"}})
            )
            .1,
            json!({"path": "a.rs", "lines": [{"at": "12", "text": "x"}]})
        );
        assert_eq!(
            one(
                "task",
                json!({"update": [{"index": "2", "status": "done"}]})
            )
            .1,
            json!({"update": [{"index": 2, "status": "done"}]})
        );
        // `all` for the whole call, or Claude Code's `replace_all` in an item.
        assert_eq!(
            one(
                "edit",
                json!({"path": "a.rs", "all": true, "replace": [{"old": "x", "new": "y"}]})
            )
            .1,
            json!({"path": "a.rs", "replace": [{"old": "x", "new": "y", "all": true}]})
        );
        assert_eq!(
            one(
                "edit",
                json!({"path": "a.rs", "replace_all": "true", "replace": [{"old": "x", "new": "y"}, {"old": "p", "new": "q", "all": false}]})
            )
            .1,
            json!({"path": "a.rs", "replace": [{"old": "x", "new": "y", "all": true}, {"old": "p", "new": "q", "all": false}]})
        );
        assert_eq!(
            one(
                "edit",
                json!({"path": "a.rs", "replace": [{"old": "x", "new": "y", "replace_all": true}]})
            )
            .1,
            json!({"path": "a.rs", "replace": [{"old": "x", "new": "y", "all": true}]})
        );
        // Untouched when right.
        let ok = json!({"path": "a.rs", "replace": [{"old": "x", "new": "y"}]});
        assert_eq!(one("edit", ok.clone()).1, ok);
    }

    #[test]
    fn edit_aliases_validate_the_whole_batch_before_changing_it() {
        for args in [
            json!({"path":"a", "edits":[{"old_string":"x", "new_string":"y"}, {"old_string":"z"}]}),
            json!({"path":"a", "replace":[{"old":"x", "new":"y"}, {"old":"z"}]}),
            json!({"path":"a", "lines":[{"at":"1", "text":"y"}, {"at":"2"}]}),
            json!({"path":"a", "edits":[{"old":"x", "new":"y"}, false]}),
            json!({"path":"a", "replace":[{"old":"x", "old_string":"other", "new":"y"}]}),
            json!({"path":"a", "old":"x", "replace":[{"old":"x", "new":"y"}]}),
            json!({"path":"a", "edits":[{"old":"x", "new":"y", "replace_all":"not true"}]}),
        ] {
            assert!(!valid_edit_items(&args), "{args}");
            let mut unchanged = args.clone();
            fix_args("edit", &mut unchanged, &tools()[2].parameters);
            assert_eq!(unchanged, args, "{args}");
            assert!(
                one("MultiEdit", args.clone())
                    .1
                    .get("__invalid_arguments")
                    .is_some()
            );
            let raw = format!(
                "{{'path':'a','edits':{}",
                args.get("edits").unwrap_or(&json!([{"old":"x"}]))
            );
            assert!(
                repaired("edit", &raw, false)
                    .get("__invalid_arguments")
                    .is_some(),
                "{raw}"
            );
        }
        for (old, new) in EDIT_OLD.iter().zip(EDIT_NEW) {
            let mut item = Map::new();
            item.insert((*old).into(), json!("x"));
            item.insert((*new).into(), json!(""));
            let args = json!({"file_path":"a", "edits":[item.clone(), item]});
            assert_eq!(
                one("MultiEdit", args).1,
                json!({"path":"a", "replace":[{"old":"x", "new":""}, {"old":"x", "new":""}]})
            );
        }
    }
    #[test]
    fn bracket_repair_requires_nested_keys_but_keeps_explicit_deletions() {
        let schema = json!({"type":"object", "required":["changes"], "properties":{
            "changes":{"type":"array", "items":{"type":"array", "items":{
                "type":"object", "required":["value"], "properties":{"value":{"type":"string"}}
            }}}
        }});
        assert!(!acceptable(
            "example",
            &json!({"changes":[[{}]]}),
            &schema,
            true
        ));
        assert!(acceptable(
            "example",
            &json!({"changes":[[{"value":""}]]}),
            &schema,
            true
        ));
        // A non-edit tool may legitimately have an `old` field on its own.
        assert!(acceptable(
            "example",
            &json!({"old":"x"}),
            &json!({"required":["old"]}),
            true
        ));
        for raw in [
            r#"{"path":"a","old":"x","new":"""#,
            r#"{"path":"a","tag":"T","at":"12-40","text":"""#,
            r#"{"path":"a","replace":[{"old":"x","new":""}"#,
            r#"{"path":"a","lines":[{"at":"3","text":""}"#,
        ] {
            assert!(repaired("edit", raw, false).get("path").is_some(), "{raw}");
        }
    }
    /// `raw` as a call's arguments that weren't JSON, normalized (`cut`: the
    /// reply ended at the output limit).
    fn repaired(name: &str, raw: &str, cut: bool) -> Value {
        let mut content = vec![call(name, json!({ "__invalid_arguments": raw }))];
        normalize_reply(&mut content, &tools(), cut);
        match content.remove(0) {
            AssistantPart::ToolCall { arguments, .. } => arguments,
            _ => unreachable!(),
        }
    }

    #[test]
    fn broken_json_is_repaired_only_when_the_tool_gets_what_it_needs() {
        for (name, raw, want) in [
            // Raw newlines and tabs in a string (common in `write` content).
            (
                "write",
                "{\"path\": \"a.py\", \"content\": \"x = 1\n\ty = 2\n\"}",
                json!({"path": "a.py", "content": "x = 1\n\ty = 2\n"}),
            ),
            // The final brace missing, the reply not cut off.
            ("read", "{\"path\": \"a.rs\"", json!({"path": "a.rs"})),
            (
                "edit",
                "{\"path\": \"a\", \"replace\": [{\"old\": \"x\", \"new\": \"y\"}",
                json!({"path": "a", "replace": [{"old": "x", "new": "y"}]}),
            ),
            // Single quotes, an escaped one, double quotes inside them, bare keys.
            (
                "read",
                "{'path': 'a.rs', 'offset': 3}",
                json!({"path": "a.rs", "offset": 3}),
            ),
            (
                "bash",
                "{'command': 'echo \\'hi\\' \"there\"'}",
                json!({"command": "echo 'hi' \"there\""}),
            ),
            ("bash", "{command: \"ls\"}", json!({"command": "ls"})),
            // Python's literals outside strings, kept inside them.
            (
                "edit",
                "{\"path\": \"a\", \"replace\": [{\"old\": \"None\", \"new\": \"True\", \"all\": True}]}",
                json!({"path": "a", "replace": [{"old": "None", "new": "True", "all": true}]}),
            ),
            // Special tokens around it, and a trailing comma.
            (
                "bash",
                "<|tool_call_begin|>{\"command\": \"ls\",}<|tool_call_end|>",
                json!({"command": "ls"}),
            ),
            // A complete flat edit whose last brace is missing.
            (
                "edit",
                "{\"path\": \"a\", \"old\": \"x\", \"new\": \"y\"",
                json!({"path": "a", "old": "x", "new": "y"}),
            ),
            // Another harness's name for a required key still counts.
            ("read", "{'file_path': 'a.rs'}", json!({"path": "a.rs"})),
        ] {
            assert_eq!(repaired(name, raw, false), want, "{raw}");
        }
        // Not repaired: a required key missing after the repair, the reply
        // cut at the output limit (inside a string, or a bracket to close),
        // text after the object, brackets that don't match, not an object.
        for (name, raw, cut) in [
            ("write", "{'content': 'x = 1'}", false),
            ("write", "{\"path\": \"a.py\"\n", false),
            (
                "write",
                "{\"path\": \"a.py\", \"content\": \"x = 1\n",
                false,
            ),
            ("read", "{\"path\": \"a.rs\"", true),
            ("bash", "{\"command\": \"ls\"} and then", false),
            ("bash", "{\"command\": [\"ls\"}", false),
            ("bash", "['ls']", false),
            // Stopped between keys (finish `stop`, no grammar): closing the
            // brackets would make edits that delete, so they stay refused.
            (
                "edit",
                "{\"path\": \"app.py\", \"old\": \"def f():\\n    return 1\"",
                false,
            ),
            (
                "edit",
                "{\"path\": \"a\", \"tag\": \"T\", \"at\": \"12-40\"",
                false,
            ),
            (
                "edit",
                "{\"path\": \"a\", \"replace\": [{\"old\": \"x\"",
                false,
            ),
            (
                "edit",
                "{\"path\": \"a\", \"lines\": [{\"at\": \"3\"}",
                false,
            ),
        ] {
            let got = repaired(name, raw, cut);
            assert_eq!(
                got.get("__invalid_arguments").and_then(Value::as_str),
                Some(raw),
                "{raw}: {got}"
            );
        }
    }

    #[test]
    fn calls_written_as_text_are_repaired_the_same_way() {
        // A raw newline in the content, a missing brace before the closing tag.
        let mut c = vec![AssistantPart::Text {
            text: "<tool_call>\n{\"name\": \"write\", \"arguments\": {\"path\": \"a.py\", \"content\": \"x = 1\ny = 2\"}}\n</tool_call>\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"b.py\"}\n</tool_call>".into(),
        }];
        assert_eq!(recover_text_calls(&mut c, &tools()), 2);
        assert_eq!(
            calls(&c),
            vec![
                (
                    "write".into(),
                    json!({"path": "a.py", "content": "x = 1\ny = 2"})
                ),
                ("read".into(), json!({"path": "b.py"})),
            ]
        );
        // Missing what the tool needs, or cut off before its closing tag:
        // left as text.
        for text in [
            "<tool_call>{'name': 'write', 'arguments': {'content': 'x'}}</tool_call>",
            "<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"b.py\"}",
        ] {
            let mut c = vec![AssistantPart::Text { text: text.into() }];
            assert_eq!(recover_text_calls(&mut c, &tools()), 0, "{text}");
        }
    }

    #[test]
    fn line_numbers_copied_from_a_read_are_dropped_only_when_clearly_copied() {
        assert_eq!(
            one("edit", json!({"path": "a", "replace": [{"old": "12:let a = 1;\n13:let b = 2;", "new": "12:let a = 3;\n13:let b = 2;"}]})).1,
            json!({"path": "a", "replace": [{"old": "let a = 1;\nlet b = 2;", "new": "let a = 3;\nlet b = 2;"}]})
        );
        assert_eq!(
            one(
                "edit",
                json!({"path": "a", "lines": [{"at": "7", "text": "7:    x = 1"}]})
            )
            .1,
            json!({"path": "a", "lines": [{"at": "7", "text": "    x = 1"}]})
        );
        // YAML-ish content that only looks numbered stays.
        for kept in ["1: a", "1: a\n3: b", "port: 80"] {
            let args = json!({"path": "a", "replace": [{"old": kept, "new": "x"}]});
            assert_eq!(one("edit", args.clone()).1, args, "{kept}");
        }
    }

    fn texts(content: &[AssistantPart]) -> Vec<String> {
        content
            .iter()
            .filter_map(|p| match p {
                AssistantPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn calls(content: &[AssistantPart]) -> Vec<(String, Value)> {
        content
            .iter()
            .filter_map(|p| match p {
                AssistantPart::ToolCall {
                    name, arguments, ..
                } => Some((name.clone(), arguments.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn calls_written_as_text_are_made() {
        let mut c = vec![AssistantPart::Text {
            text: "Let me look.\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"src/a.rs\"}}\n</tool_call>".into(),
        }];
        assert_eq!(recover_text_calls(&mut c, &tools()), 1);
        assert_eq!(texts(&c), vec!["Let me look."]);
        assert_eq!(
            calls(&c),
            vec![("read".into(), json!({"path": "src/a.rs"}))]
        );

        // Qwen3-Coder's form, two calls, the last one cut off at the end.
        let mut c = vec![AssistantPart::Text {
            text: "<tool_call>\n<function=bash>\n<parameter=command>\nls -la\n</parameter>\n<parameter=timeout>\n30\n</parameter>\n</function>\n</tool_call>\n<tool_call>\n<function=read>\n<parameter=path>\nREADME.md\n</parameter>\n</function>".into(),
        }];
        assert_eq!(recover_text_calls(&mut c, &tools()), 2);
        assert!(texts(&c).is_empty(), "{c:?}");
        assert_eq!(
            calls(&c),
            vec![
                ("bash".into(), json!({"command": "ls -la", "timeout": 30})),
                ("read".into(), json!({"path": "README.md"}))
            ]
        );

        // A reply that is only the JSON of a call.
        let mut c = vec![AssistantPart::Text {
            text: "```json\n{\"name\": \"bash\", \"arguments\": {\"command\": \"pwd\"}}\n```"
                .into(),
        }];
        assert_eq!(recover_text_calls(&mut c, &tools()), 1);
        assert_eq!(calls(&c), vec![("bash".into(), json!({"command": "pwd"}))]);

        // The call at the end of the reasoning, nothing said.
        let mut c = vec![AssistantPart::Thinking {
            text: "I should list files.\n<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>".into(),
            opaque: None,
        }];
        assert_eq!(recover_text_calls(&mut c, &tools()), 1);
        assert_eq!(calls(&c), vec![("bash".into(), json!({"command": "ls"}))]);
    }

    #[test]
    fn apply_patch_forms_become_edits() {
        let patch = "*** Begin Patch\n*** Update File: src/a.py\n@@\n-x = 1\n+x = 2\n*** End Patch";
        let want = json!({"path": "src/a.py", "patch": patch});
        let conv = |name: &str, args: Value| {
            let mut c = vec![call(name, args)];
            let n = patch_calls(&mut c, &tools());
            (n, calls(&c))
        };
        // The tool GPT models are trained to call, in its shapes.
        for args in [
            json!({"input": patch}),
            json!(patch),
            json!({"__invalid_arguments": patch}),
        ] {
            assert_eq!(
                conv("apply_patch", args),
                (1, vec![("edit".into(), want.clone())])
            );
        }
        assert_eq!(
            conv("functions.apply_patch", json!({"patch": patch})).1,
            vec![("edit".into(), want.clone())]
        );
        // Written as a shell command.
        for cmd in [
            format!("apply_patch <<'EOF'\n{patch}\nEOF"),
            format!("apply_patch \"{patch}\""),
            format!("cat <<'EOF' | apply_patch\n{patch}\nEOF"),
        ] {
            assert_eq!(
                conv("bash", json!({"command": cmd})).1,
                vec![("edit".into(), want.clone())],
                "{cmd}"
            );
        }
        // `edit` with the patch under another key.
        assert_eq!(
            conv("edit", json!({"input": patch})).1,
            vec![("edit".into(), want.clone())]
        );
        // Not patches: untouched.
        for (name, args) in [
            (
                "bash",
                json!({"command": "cat notes.txt | grep '*** Begin Patch'"}),
            ),
            ("bash", json!({"command": "git apply fix.diff"})),
            (
                "edit",
                json!({"path": "a", "replace": [{"old": "x", "new": "y"}]}),
            ),
            ("apply_patch", json!({"input": "--- a/x\n+++ b/x\n"})),
        ] {
            assert_eq!(
                conv(name, args.clone()),
                (0, vec![(name.to_string(), args)])
            );
        }
        // Without `edit` offered, nothing to turn them into.
        let mut c = vec![call("apply_patch", json!({"input": patch}))];
        let no_edit: Vec<ToolSpec> = tools().into_iter().filter(|t| t.name != "edit").collect();
        assert_eq!(patch_calls(&mut c, &no_edit), 0);
    }

    #[test]
    fn text_that_only_mentions_calls_stays_text() {
        for text in [
            "Use `<tool_call>` tags when you call tools.",
            "Here is an example: {\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}} — try it.",
            "<tool_call>{\"name\": \"launch_rockets\", \"arguments\": {}}</tool_call>",
        ] {
            let mut c = vec![AssistantPart::Text { text: text.into() }];
            assert_eq!(recover_text_calls(&mut c, &tools()), 0, "{text}");
            assert_eq!(texts(&c), vec![text.to_string()]);
        }
        // Real calls made: nothing is looked for.
        let mut c = vec![
            AssistantPart::Text {
                text: "<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>".into(),
            },
            call("read", json!({"path": "a"})),
        ];
        assert_eq!(recover_text_calls(&mut c, &tools()), 0);
    }
}
