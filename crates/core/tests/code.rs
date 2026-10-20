//! The edit engine and language servers end to end: diagnostics after
//! edits (with a mock server in Python, and real servers when installed),
//! the `lsp` tool, stale-tag rebase and the seen-lines rule, and the budget
//! on the always-on prompt plus tool specs (plan principle 1, QW11).

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_proto::*;

async fn set_permission(h: &Harness, id: &str, permission: PermissionMode) {
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: id.into(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(permission),
                pinned: None,
                archived: None,
                cwd: None,
                members: None,
                fast: None,
                subagents: None,
            },
            None,
        )
        .await
        .unwrap();
}

/// Estimated tokens of the system prompt plus tool specs in `req`, using the
/// engine's own estimator (`context::estimate_request`).
fn base_prompt_tokens(req: &serde_json::Value) -> (u64, Vec<(String, u64)>) {
    let system = req["messages"][0]["content"].as_str().unwrap_or("");
    let specs: Vec<tessera_core::llm::ToolSpec> = req["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| tessera_core::llm::ToolSpec {
                    name: t["function"]["name"].as_str().unwrap_or("").to_string(),
                    description: t["function"]["description"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    parameters: t["function"]["parameters"].clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let per_tool = specs
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                tessera_core::context::estimate_request("", std::slice::from_ref(s), &[]),
            )
        })
        .collect();
    (
        tessera_core::context::estimate_request(system, &specs, &[]),
        per_tool,
    )
}

/// Plan principle 1 / QW11: the always-on system prompt plus tool specs stay
/// within 5k estimated tokens.
#[tokio::test]
async fn base_prompt_and_tool_specs_fit_the_budget() {
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let (total, per_tool) = base_prompt_tokens(&req);
    for (name, n) in &per_tool {
        let chars = req["tools"]
            .as_array()
            .and_then(|a| a.iter().find(|t| t["function"]["name"] == name.as_str()))
            .and_then(|t| t["function"]["description"].as_str())
            .map(|d| d.chars().count())
            .unwrap_or(0);
        eprintln!("{n:>5} {name} ({chars} chars)");
    }
    eprintln!("{total:>5} total (system prompt + tools)");
    // 4,500 before the `browser` tool (~200 tokens) was added, 4,750
    // before `save_secret` and `settings` (~250), 5,000 before `notify`
    // and `share_file` (~260).
    assert!(
        total <= 5_260,
        "base prompt is {total} estimated tokens (budget 5260)"
    );
    // Descriptions: at most 600 characters (`edit` and `agent` 1,200).
    for t in req["tools"].as_array().unwrap() {
        let name = t["function"]["name"].as_str().unwrap();
        let chars = t["function"]["description"]
            .as_str()
            .unwrap_or("")
            .chars()
            .count();
        let cap = if matches!(name, "edit" | "agent") {
            1200
        } else {
            600
        };
        assert!(
            chars <= cap,
            "`{name}` description is {chars} characters (max {cap})"
        );
    }
}

/// A lean start (`[agent] lean_tools`, `lean_start.rs`): the core tools,
/// shorter descriptions for the largest of them and one line about the
/// others.
#[tokio::test]
async fn a_lean_start_fits_its_own_budget() {
    let h = harness_agent(vec![Reply::Text("ok".into())], 100_000, "lean_tools = true").await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let (total, per_tool) = base_prompt_tokens(&req);
    for (name, n) in &per_tool {
        eprintln!("{n:>5} {name}");
    }
    eprintln!("{total:>5} total at a lean start (system prompt + tools)");
    // 5,252 without it, 3,290 with it (October 2026).
    assert!(
        total <= 3_300,
        "a lean start is {total} estimated tokens (budget 3300)"
    );
    // The shorter descriptions fit the cap every other tool has.
    for t in req["tools"].as_array().unwrap() {
        let name = t["function"]["name"].as_str().unwrap();
        let chars = t["function"]["description"]
            .as_str()
            .unwrap_or("")
            .chars()
            .count();
        let cap = if name == "agent" { 1200 } else { 600 };
        assert!(
            chars <= cap,
            "`{name}` description is {chars} characters at a lean start (max {cap})"
        );
    }
}

/// A terminal chat (`lean`) loads only what coding needs at its first
/// request: the coding tools, short guidelines, the project's instructions
/// and the user profile; no skills index, no memory notes, no tools it
/// can't use. A chat from the apps keeps everything.
#[tokio::test]
async fn terminal_chats_load_the_lean_set_and_little_else() {
    let h = harness(
        vec![Reply::Text("ok".into()), Reply::Text("ok".into())],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("AGENTS.md"), "Use tabs in this project.").unwrap();
    let skill = h.work.join(".tessera/skills/helper");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: helper\ndescription: Helps with builds.\n---\nStep 1: build.",
    )
    .unwrap();
    for (target, content) in [
        (MemoryTarget::User, "Prefers short answers."),
        (MemoryTarget::Memory, "The staging box is called vega."),
    ] {
        h.engine
            .handle(
                Request::SetMemory {
                    target,
                    content: content.into(),
                },
                None,
            )
            .await
            .unwrap();
    }
    let create = |lean: bool| Request::CreateSession {
        cwd: Some(h.work.display().to_string()),
        model: None,
        title: None,
        bot_id: None,
        private: false,
        lean,
    };
    let lean: SessionInfo =
        serde_json::from_value(h.engine.handle(create(true), None).await.unwrap()).unwrap();
    assert!(lean.lean);
    h.send(&lean.id, "hi").await;
    h.settle(&lean.id).await;
    let full: SessionInfo =
        serde_json::from_value(h.engine.handle(create(false), None).await.unwrap()).unwrap();
    assert!(!full.lean);
    h.send(&full.id, "hi").await;
    h.settle(&full.id).await;
    let reqs = h.mock.agent_requests();
    let (lean_req, full_req) = (&reqs[0], &reqs[1]);
    let names = |req: &serde_json::Value| -> Vec<String> {
        req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect()
    };
    let lean_tools = names(lean_req);
    for t in &lean_tools {
        assert!(
            tessera_core::profiles::LEAN_TOOLS.contains(&t.as_str()),
            "a terminal chat was offered `{t}`"
        );
    }
    for t in [
        "read", "write", "edit", "bash", "job", "grep", "glob", "task",
    ] {
        assert!(lean_tools.iter().any(|n| n == t), "`{t}` is missing");
    }
    assert!(names(full_req).len() > lean_tools.len() + 5);
    let system =
        |req: &serde_json::Value| req["messages"][0]["content"].as_str().unwrap().to_string();
    let (ls, fs) = (system(lean_req), system(full_req));
    assert!(ls.contains("coding agent") && ls.contains("Use tabs in this project."));
    assert!(ls.contains("Prefers short answers."), "the profile stays");
    assert!(!ls.contains("vega"), "memory notes stay out");
    assert!(!ls.contains("## Skills") && !ls.contains("`recall`") && !ls.contains("`memory`"));
    assert!(fs.contains("vega") && fs.contains("## Skills"));
    let (lean_total, per_tool) = base_prompt_tokens(lean_req);
    let (full_total, _) = base_prompt_tokens(full_req);
    for (name, n) in &per_tool {
        eprintln!("{n:>5} {name}");
    }
    eprintln!("{lean_total:>5} terminal chat (system prompt + tools); app chat {full_total}");
    assert!(
        lean_total <= 2_300,
        "a terminal chat starts with {lean_total} estimated tokens (budget 2300)"
    );
    // The model naming a tool the terminal doesn't offer gets "unknown tool".
    h.mock.push(Reply::Tools(vec![(
        "memory".into(),
        json!({"action": "add", "content": "x"}),
    )]));
    h.mock.push(Reply::Text("fine".into()));
    h.send(&lean.id, "remember x").await;
    h.settle(&lean.id).await;
    let results = tool_results(&h.entries(&lean.id).await);
    let (_, is_error, text) = results.last().unwrap();
    assert!(
        *is_error && text.contains("Unknown tool `memory`"),
        "{text}"
    );
}

// ---- language servers and the edit engine --------------------------------

/// A language server in Python for `.foo` files (project marker `foo.toml`):
/// an error on every line containing `BAD` (published 2.5 s late when the
/// text contains `SLOW`), references/definition/hover/symbols by word, and
/// renames across the project's `.foo` files.
const MOCK_LSP: &str = r#"
import json, os, re, sys, threading, time
from urllib.parse import unquote, urlparse

lock = threading.Lock()
docs = {}
root = "."

def send(msg):
    body = json.dumps(msg).encode()
    with lock:
        sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        sys.stdout.buffer.flush()

def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        k, v = line.split(":", 1)
        headers[k.lower()] = v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def path(uri): return unquote(urlparse(uri).path)
def uri(p): return "file://" + p
def text_of(u):
    if u in docs: return docs[u]
    with open(path(u)) as f: return f.read()
def rng(i, a, b): return {"start": {"line": i, "character": a}, "end": {"line": i, "character": b}}

def publish(u, version, text):
    def go():
        time.sleep(2.5 if "SLOW" in text else 0.05)
        diags = [{"range": rng(i, l.find("BAD"), l.find("BAD") + 3), "severity": 1, "code": "E1",
                  "source": "mock", "message": "undefined BAD"}
                 for i, l in enumerate(text.split("\n")) if "BAD" in l]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
              "params": {"uri": u, "version": version, "diagnostics": diags}})
    threading.Thread(target=go, daemon=True).start()

def word_at(params):
    line = text_of(params["textDocument"]["uri"]).split("\n")[params["position"]["line"]]
    return re.match(r"\w*", line[params["position"]["character"]:]).group(0)

def occurrences(word):
    out = []
    for name in sorted(os.listdir(root)):
        if name.endswith(".foo"):
            p = os.path.join(root, name)
            for i, l in enumerate(text_of(uri(p)).split("\n")):
                for m in re.finditer(r"\b%s\b" % re.escape(word), l):
                    out.append((p, i, m.start(), m.end()))
    return out

while True:
    msg = read()
    if msg is None:
        break
    m, params, mid = msg.get("method"), msg.get("params") or {}, msg.get("id")
    result = None
    if m == "initialize":
        root = path(params["rootUri"])
        result = {"capabilities": {"textDocumentSync": 1, "definitionProvider": True, "referencesProvider": True,
                  "hoverProvider": True, "documentSymbolProvider": True, "workspaceSymbolProvider": True,
                  "renameProvider": True}}
    elif m in ("textDocument/didOpen", "textDocument/didChange"):
        td = params["textDocument"]
        text = td["text"] if m.endswith("didOpen") else params["contentChanges"][-1]["text"]
        docs[td["uri"]] = text
        publish(td["uri"], td.get("version"), text)
    elif m == "textDocument/references":
        result = [{"uri": uri(p), "range": rng(i, a, b)} for p, i, a, b in occurrences(word_at(params))]
    elif m == "textDocument/definition":
        w = word_at(params)
        result = [{"uri": uri(p), "range": rng(i, a, b)} for p, i, a, b in occurrences(w)
                  if text_of(uri(p)).split("\n")[i].startswith("def ")][:1]
    elif m == "textDocument/hover":
        result = {"contents": {"kind": "plaintext", "value": "word " + word_at(params)}}
    elif m == "textDocument/documentSymbol":
        lines = text_of(params["textDocument"]["uri"]).split("\n")
        result = [{"name": l[4:].strip(), "kind": 12, "range": rng(i, 0, len(l)), "selectionRange": rng(i, 4, len(l))}
                  for i, l in enumerate(lines) if l.startswith("def ")]
    elif m == "textDocument/rename":
        changes = {}
        for p, i, a, b in occurrences(word_at(params)):
            changes.setdefault(uri(p), []).append({"range": rng(i, a, b), "newText": params["newName"]})
        result = {"changes": changes}
    elif m == "exit":
        break
    if mid is not None and m is not None:
        send({"jsonrpc": "2.0", "id": mid, "result": result})
"#;

/// A harness whose config registers the mock server (`extra` is appended
/// to the config), with `foo.toml` in the working directory.
async fn lsp_harness(script: Vec<Reply>, extra: &str) -> Harness {
    let mock = Mock::start(script).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("foo.toml"), "").unwrap();
    // Outside Tessera's home: the server runs in the sandbox, which hides it.
    let server = work_dir.path().join("mock_lsp.py");
    std::fs::write(&server, MOCK_LSP).unwrap();
    let config = format!(
        r#"default_model = "mock/m"
[agent]
review_interval = 0
# The full tool set (small model mode is `small_mode.rs`'s).
[tuning."*"]
small = "off"
check = false
[lsp]
{extra}
[[lsp.server]]
name = "mockls"
command = ["python3", "{}"]
extensions = ["foo"]
markers = ["foo.toml"]
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{}"
discover = false
[[provider.models]]
id = "m"
context_window = 100000
max_output = 2000
"#,
        server.display(),
        mock.url
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    Harness {
        engine,
        mock,
        dir,
        work,
        _work_dir: work_dir,
    }
}

fn offered(req: &serde_json::Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn edits_report_the_errors_a_language_server_says_they_introduced() {
    let h = lsp_harness(vec![], "").await;
    let old = "one\nBAD old\nthree\n";
    std::fs::write(h.work.join("a.foo"), old).unwrap();
    let tag = tessera_core::tools::files::file_tag(old);
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"path": "a.foo"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "edit".into(),
        json!({"path": "a.foo", "tag": tag, "lines": [{"at": ">3", "text": "four BAD new"}]}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "write".into(),
        json!({"path": "conf.json", "content": "{\"a\": 1,}"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "edit a.foo").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let edit = &results[1].2;
    assert!(!results[1].1, "{results:?}");
    assert!(edit.contains("LSP mockls: 1 new error"), "{edit}");
    assert!(edit.contains("a.foo:4:6 error[E1] undefined BAD"), "{edit}");
    assert!(
        !edit.contains("a.foo:2:"),
        "the error that was already there is not repeated: {edit}"
    );
    assert!(
        results[2]
            .2
            .contains("Warning: conf.json doesn't parse as JSON"),
        "{results:?}"
    );
    // The tool is offered because the server matched the directory.
    assert!(offered(&h.mock.agent_requests()[0]).contains(&"lsp".to_string()));
}

#[tokio::test]
async fn late_diagnostics_come_with_the_next_tool_result() {
    // A short budget: the server answers this edit only after 2.5 s.
    let h = lsp_harness(vec![], "wait_ms = 300").await;
    std::fs::write(h.work.join("a.foo"), "one\n").unwrap();
    let tag = tessera_core::tools::files::file_tag("one\n");
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"path": "a.foo"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "edit".into(),
        json!({"path": "a.foo", "tag": tag, "lines": [{"at": ">1", "text": "SLOW BAD"}]}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 3.5"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[1].2.contains("LSP mockls: (diagnostics pending)"),
        "{results:?}"
    );
    assert!(
        results[2]
            .2
            .contains("LSP mockls (late): 1 new error\na.foo:2:6 error[E1] undefined BAD"),
        "{results:?}"
    );
}

#[tokio::test]
async fn lsp_navigation_results_can_be_edited_directly_and_renames_span_files() {
    let h = lsp_harness(vec![], "").await;
    std::fs::write(h.work.join("a.foo"), "def helper\nuse helper\n").unwrap();
    std::fs::write(h.work.join("b.foo"), "x\ncall helper here\n").unwrap();
    let b_tag = tessera_core::tools::files::file_tag("x\ncall helper here\n");
    let lsp = |args: serde_json::Value| Reply::Tools(vec![("lsp".into(), args)]);
    h.mock.push(lsp(
        json!({"action": "references", "path": "a.foo", "line": 2, "symbol": "helper"}),
    ));
    // b.foo was never read, but the references showed its line 2.
    h.mock.push(Reply::Tools(vec![(
        "edit".into(),
        json!({"path": "b.foo", "tag": b_tag, "lines": [{"at": "2", "text": "call helper there"}]}),
    )]));
    h.mock.push(lsp(
        json!({"action": "definition", "path": "b.foo", "line": 2, "symbol": "helper"}),
    ));
    h.mock.push(lsp(
        json!({"action": "hover", "path": "a.foo", "line": 1, "symbol": "helper"}),
    ));
    h.mock
        .push(lsp(json!({"action": "symbols", "path": "a.foo"})));
    h.mock.push(lsp(json!({"action": "rename", "path": "a.foo", "line": 1, "symbol": "helper", "new_name": "assist"})));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "navigate").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let refs = &results[0].2;
    assert!(
        refs.starts_with("3 references to `helper` in 2 files:"),
        "{results:?}"
    );
    assert!(
        refs.contains("[a.foo#") && refs.contains("\n1:def helper\n2:use helper\n"),
        "{refs}"
    );
    assert!(
        refs.contains(&format!("[b.foo#{b_tag}]\n2:call helper here")),
        "{refs}"
    );
    assert!(!results[1].1, "edit from lsp output: {results:?}");
    assert!(
        results[2].2.contains("Definition of `helper`:") && results[2].2.contains("1:def helper"),
        "{results:?}"
    );
    assert_eq!(results[3].2, "word helper");
    assert!(
        results[4].2.contains("1 symbols\n1 function helper"),
        "{results:?}"
    );
    let rename = &results[5].2;
    assert!(
        rename.starts_with("Renamed `helper` to `assist`: 3 edits in 2 files."),
        "{rename}"
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("a.foo")).unwrap(),
        "def assist\nuse assist\n"
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("b.foo")).unwrap(),
        "x\ncall assist there\n"
    );
    // Both files were snapshotted, so the rename can be rewound.
    let snaps = h.engine.store().snapshots_since(&s.id, 0).unwrap();
    for f in ["a.foo", "b.foo"] {
        assert!(snaps.iter().any(|(p, _)| p.ends_with(f)), "{f}: {snaps:?}");
    }
}

#[tokio::test]
async fn lsp_rename_is_refused_in_read_only_sessions_and_the_tool_hides_without_a_server() {
    // No marker in the working directory: no tool.
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    assert!(!offered(&h.mock.agent_requests()[0]).contains(&"lsp".to_string()));

    let h = lsp_harness(vec![], "in_read_only = true").await;
    std::fs::write(h.work.join("a.foo"), "def helper\n").unwrap();
    h.mock.push(Reply::Tools(vec![(
        "lsp".into(),
        json!({"action": "rename", "path": "a.foo", "line": 1, "symbol": "helper", "new_name": "x"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    set_permission(&h, &s.id, PermissionMode::ReadOnly).await;
    h.send(&s.id, "rename").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("read-only"),
        "{results:?}"
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("a.foo")).unwrap(),
        "def helper\n"
    );
}

#[tokio::test]
async fn edits_are_limited_to_lines_the_model_was_shown() {
    let h = harness(vec![], 100_000).await;
    let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
    std::fs::write(h.work.join("big.txt"), &text).unwrap();
    let tag = tessera_core::tools::files::file_tag(&text);
    let edit = Reply::Tools(vec![(
        "edit".into(),
        json!({"path": "big.txt", "tag": tag, "lines": [{"at": "40", "text": "changed"}]}),
    )]);
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"path": "big.txt", "limit": 10}),
    )]));
    h.mock.push(edit.clone());
    h.mock.push(edit);
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "edit").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[1].1 && results[1].2.starts_with("Refused: line 40 of big.txt"),
        "{results:?}"
    );
    assert!(
        results[1].2.contains("40:line 40"),
        "the refusal shows the line: {results:?}"
    );
    assert!(
        !results[2].1,
        "after seeing it the retry works: {results:?}"
    );
    assert!(
        std::fs::read_to_string(h.work.join("big.txt"))
            .unwrap()
            .contains("\nchanged\n")
    );
}

/// `write` replaces an existing file only at a version this chat read or
/// wrote: an unread file is refused (with its tag), a read of part of it is
/// enough, a file the user changed since is refused again (naming both
/// versions), new files and the agent's own just-written ones are free, and
/// a refused write leaves nothing for rewind to restore.
#[tokio::test]
async fn write_replaces_only_files_this_chat_has_seen_as_they_are() {
    let h = harness(vec![], 100_000).await;
    let notes: String = (1..=30).map(|i| format!("note {i}\n")).collect();
    std::fs::write(h.work.join("notes.txt"), &notes).unwrap();
    std::fs::write(h.work.join("empty.txt"), "").unwrap();
    let write = |path: &str, content: &str| {
        Reply::Tools(vec![(
            "write".into(),
            json!({"path": path, "content": content}),
        )])
    };
    for reply in [
        write("notes.txt", "mine\n"),
        Reply::Tools(vec![(
            "read".into(),
            json!({"path": "notes.txt", "limit": 3}),
        )]),
        write("notes.txt", "mine\n"),
        write("new.txt", "one\n"),
        write("new.txt", "two\n"),
        write("empty.txt", "filled\n"),
        Reply::Text("done".into()),
    ] {
        h.mock.push(reply);
    }
    let s = h.session().await;
    h.send(&s.id, "first").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let tag = tessera_core::tools::files::file_tag(&notes);
    assert!(
        results[0].1
            && results[0]
                .2
                .contains("notes.txt already exists (30 lines, #")
            && results[0].2.contains("no record of a read")
            && results[0].2.contains(&tag)
            && results[0].2.contains("Read it first"),
        "{results:?}"
    );
    for (i, r) in results.iter().enumerate().skip(1) {
        assert!(!r.1, "call {i}: {r:?}");
    }
    assert_eq!(
        std::fs::read_to_string(h.work.join("notes.txt")).unwrap(),
        "mine\n"
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("new.txt")).unwrap(),
        "two\n"
    );

    // The user changes the file the agent wrote: the next write is refused
    // and says what changed; after a read it goes through.
    std::fs::write(h.work.join("notes.txt"), "mine\nthe user's line\n").unwrap();
    for reply in [
        write("notes.txt", "mine again\n"),
        Reply::Tools(vec![("read".into(), json!({"path": "notes.txt"}))]),
        write("notes.txt", "mine again\n"),
        Reply::Text("done".into()),
    ] {
        h.mock.push(reply);
    }
    h.send(&s.id, "second").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let refused = &results[6];
    assert!(
        refused.1
            && refused
                .2
                .contains("notes.txt isn't the version you last read or wrote")
            && refused.2.contains("+1 −0 lines"),
        "{refused:?}"
    );
    assert!(!results[7].1 && !results[8].1, "{results:?}");
    assert_eq!(
        std::fs::read_to_string(h.work.join("notes.txt")).unwrap(),
        "mine again\n"
    );

    // A refused write was never snapshotted: rewinding the first turn's
    // files restores notes.txt from the write that happened, once.
    let first = h
        .entries(&s.id)
        .await
        .iter()
        .find(|e| matches!(&e.body, EntryBody::User { .. }))
        .map(|e| e.seq)
        .unwrap();
    let dry: RewindResult = serde_json::from_value(
        h.engine
            .handle(
                Request::Rewind {
                    session_id: s.id.clone(),
                    to_seq: first - 1,
                    files: true,
                    conversation: false,
                    dry_run: true,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(dry.restored, ["empty.txt", "notes.txt"], "{dry:?}");
    assert_eq!(dry.deleted, ["new.txt"], "{dry:?}");
}

/// In "Ask me" mode a `write` the guard would refuse is refused before
/// anyone is asked to approve it (no approval, then a second one after the
/// read).
#[tokio::test]
async fn a_write_that_would_be_refused_asks_nobody_first() {
    for (path, original) in [
        ("notes.txt", "theirs\n".to_string()),
        (".envrc", "a private setting\n".to_string()),
        ("large.txt", "x".repeat(5 * 1024 * 1024)),
    ] {
        let h = harness(
            vec![
                Reply::Tools(vec![(
                    "write".into(),
                    json!({"path": path, "content": "mine\n"}),
                )]),
                Reply::Text("done".into()),
            ],
            100_000,
        )
        .await;
        std::fs::write(h.work.join(path), &original).unwrap();
        let s = h.session().await;
        set_permission(&h, &s.id, PermissionMode::Ask).await;
        h.send(&s.id, "write it").await;
        // Settles without an approval card (one would wait for an answer).
        h.settle(&s.id).await;
        let results = tool_results(&h.entries(&s.id).await);
        assert!(
            results[0].1
                && results[0].2.contains(&format!("{path} already exists"))
                && results[0].2.contains("Read it first"),
            "{results:?}"
        );
        assert_eq!(
            std::fs::read_to_string(h.work.join(path)).unwrap(),
            original
        );
        if path == ".envrc" {
            assert!(!results[0].2.contains("private setting"));
            assert!(
                !results[0]
                    .2
                    .contains(&tessera_core::tools::files::file_tag(&original))
            );
        }
    }
}

#[tokio::test]
async fn base_prompt_with_the_lsp_tool_fits_the_budget_too() {
    let h = lsp_harness(vec![Reply::Text("ok".into())], "").await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    assert!(offered(&req).contains(&"lsp".to_string()));
    let (total, per_tool) = base_prompt_tokens(&req);
    let lsp = per_tool.iter().find(|(n, _)| n == "lsp").unwrap().1;
    eprintln!("{total} total with lsp ({lsp} for lsp)");
    assert!(lsp <= 300, "lsp spec is {lsp} tokens");
    let desc = req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "lsp")
        .and_then(|t| t["function"]["description"].as_str())
        .unwrap();
    assert!(desc.chars().count() <= 600, "{desc}");
    // The base budget plus room for the lsp tool itself.
    // 4,700 before the `browser` tool (~200 tokens) was added, 4,950
    // before `save_secret` and `settings` (~250).
    assert!(
        total <= 5_460,
        "base prompt with lsp is {total} estimated tokens (budget 5460)"
    );
}

// ---- real language servers (not installed by the tests) -------------------
//
// Each runs only when its server is on PATH and is ignored by default. Run:
//   cargo test -p tessera-core --test e2e real_ -- --ignored --nocapture
// Install: rust-analyzer (`rustup component add rust-analyzer`), pyright
// (`npm i -g pyright`) or pylsp (`pip install python-lsp-server`), clangd
// (your distribution's clang/clangd package).

fn installed(bin: &str) -> bool {
    let ok = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()));
    if !ok {
        eprintln!("skipping: `{bin}` is not installed");
    }
    ok
}

/// Read `file`, apply `edit` (a `lines` item), wait, then ask for the file's
/// diagnostics; returns every tool result.
async fn real_server_round(
    h: &Harness,
    file: &str,
    before: &str,
    edit: serde_json::Value,
    warm_secs: u32,
) -> Vec<(String, bool, String)> {
    let tag = tessera_core::tools::files::file_tag(before);
    h.mock
        .push(Reply::Tools(vec![("read".into(), json!({"path": file}))]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": format!("sleep {warm_secs}")}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "edit".into(),
        json!({"path": file, "tag": tag, "lines": [edit]}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 3"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "lsp".into(),
        json!({"action": "diagnostics", "path": file}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "edit").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(240), h.engine.wait_idle(&s.id))
        .await
        .expect("session did not finish");
    let results = tool_results(&h.entries(&s.id).await);
    for (tool, err, text) in &results {
        eprintln!("--- {tool} (error: {err})\n{text}");
    }
    results
}

#[tokio::test]
#[ignore = "needs rust-analyzer; see the comment above"]
async fn real_rust_analyzer_reports_a_type_error_after_an_edit() {
    if !installed("rust-analyzer") {
        return;
    }
    let h = harness(vec![], 100_000).await;
    std::fs::create_dir_all(h.work.join("src")).unwrap();
    std::fs::write(
        h.work.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let before = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    std::fs::write(h.work.join("src/lib.rs"), before).unwrap();
    let results = real_server_round(
        &h,
        "src/lib.rs",
        before,
        json!({"at": "2", "text": "    let s: i32 = \"text\";\n    a + b + s"}),
        25,
    )
    .await;
    let all: String = results
        .iter()
        .map(|r| r.2.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all.contains("mismatched types") || all.contains("E0308"),
        "{all}"
    );
}

#[tokio::test]
#[ignore = "needs pyright-langserver or pylsp; see the comment above"]
async fn real_python_server_reports_an_undefined_name_after_an_edit() {
    if !installed("pyright-langserver")
        && !installed("pylsp")
        && !installed("basedpyright-langserver")
    {
        return;
    }
    let h = harness(vec![], 100_000).await;
    std::fs::write(h.work.join("requirements.txt"), "").unwrap();
    let before = "def add(a, b):\n    return a + b\n";
    std::fs::write(h.work.join("app.py"), before).unwrap();
    let results = real_server_round(
        &h,
        "app.py",
        before,
        json!({"at": "2", "text": "    return a + b + missing_name"}),
        5,
    )
    .await;
    let all: String = results
        .iter()
        .map(|r| r.2.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("missing_name"), "{all}");
    assert!(all.contains("app.py:2:"), "{all}");
}

#[tokio::test]
#[ignore = "needs clangd; see the comment above"]
async fn real_clangd_reports_an_undeclared_identifier_after_an_edit() {
    if !installed("clangd") {
        return;
    }
    let h = harness(vec![], 100_000).await;
    std::fs::write(h.work.join("compile_flags.txt"), "-std=c11\n").unwrap();
    let before = "int add(int a, int b) {\n    return a + b;\n}\n";
    std::fs::write(h.work.join("main.c"), before).unwrap();
    let results = real_server_round(
        &h,
        "main.c",
        before,
        json!({"at": "2", "text": "    return a + b + missing_name;"}),
        2,
    )
    .await;
    let edit = &results[2].2;
    assert!(edit.contains("LSP clangd: 1 new error"), "{edit}");
    assert!(
        edit.contains("main.c:2:20 error") && edit.contains("missing_name"),
        "{edit}"
    );
}

/// A server that tries to read a file in Tessera's home and reports what it
/// found as a diagnostic.
const PEEK_LSP: &str = r#"
import json, sys

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        k, v = line.split(":", 1)
        headers[k.lower()] = v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

while True:
    msg = read()
    if msg is None:
        break
    m = msg.get("method")
    if m == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
    elif m == "textDocument/didOpen":
        try:
            with open(sys.argv[1]) as f:
                seen = "saw " + f.read()
        except OSError as e:
            seen = "hidden " + type(e).__name__
        td = msg["params"]["textDocument"]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
            "uri": td["uri"], "version": td["version"],
            "diagnostics": [{"range": {"start": {"line": 0, "character": 0}}, "severity": 1, "message": seen}]}})
    elif msg.get("id") is not None:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
"#;

#[tokio::test]
async fn language_servers_run_in_the_sandbox_and_cannot_read_tesseras_home() {
    if !tessera_core::sandbox::support().available {
        eprintln!(
            "sandbox unavailable here: {}",
            tessera_core::sandbox::describe()
        );
        return;
    }
    let h = lsp_harness(vec![], "").await;
    let secret = h.dir.path().join("probe-secret.txt");
    std::fs::write(&secret, "sk-lsp-probe-value").unwrap();
    let probe = h.work.parent().unwrap().join("peek_lsp.py");
    std::fs::write(&probe, PEEK_LSP).unwrap();
    // Swap the mock for the probe and reopen.
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    let cfg = cfg.replacen(
        "[[lsp.server]]",
        &format!(
            "[[lsp.server]]\nname = \"peek\"\ncommand = [\"python3\", \"{}\", \"{}\"]\nextensions = [\"peek\"]\nmarkers = [\"foo.toml\"]\n\n[[lsp.server]]",
            probe.display(),
            secret.display()
        ),
        1,
    );
    std::fs::write(h.dir.path().join("config.toml"), cfg).unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let h = Harness { engine, ..h };
    h.mock.push(Reply::Tools(vec![(
        "write".into(),
        json!({"path": "a.peek", "content": "x\n"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "write").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let out = &results[0].2;
    assert!(out.contains("LSP peek: 1 new error"), "{out}");
    assert!(out.contains("hidden"), "{out}");
    assert!(!out.contains("sk-lsp-probe-value"), "{out}");

    // Control: without the sandbox the same probe does see the file.
    let cfg = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    std::fs::write(
        h.dir.path().join("config.toml"),
        format!("{cfg}\n[security]\nsandbox = \"off\"\n"),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        Default::default(),
    )
    .await
    .unwrap();
    let h = Harness { engine, ..h };
    h.mock.push(Reply::Tools(vec![(
        "write".into(),
        json!({"path": "b.peek", "content": "y\n"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "write").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].2.contains("saw sk-lsp-probe-value"),
        "{results:?}"
    );
}

/// Reconstruct the two changed baseline specs against this same fixture.
/// Moving date/model changes position, not system bytes; archive metadata is
/// not a tool spec. New prune arguments must be paid for by shorter prose.
#[tokio::test]
async fn efficiency_specs_are_net_neutral_against_batch_24() {
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let request = h.mock.agent_requests()[0].clone();
    let (after, _) = base_prompt_tokens(&request);
    let mut baseline = request.clone();
    for spec in baseline["tools"].as_array_mut().unwrap() {
        match spec["function"]["name"].as_str().unwrap() {
            "task" => {
                spec["function"]["description"] = json!(
                    "Durable task ledger for multi-step work; survives compaction verbatim. Skip it for quick questions. \
`plan`, `constraints`, `open_questions`, `done_criteria` replace the list; `add_*` append; `update` \
(plan status) and `verify` (criterion + evidence) take 1-based indexes. No args: show it."
                );
                let properties = spec["function"]["parameters"]["properties"]
                    .as_object_mut()
                    .unwrap();
                for key in ["decisions", "artifacts", "notes", "remove"] {
                    properties.remove(key);
                }
            }
            "web_fetch" => {
                spec["function"]["description"] = json!(
                    "Fetch a URL as readable text (HTML → Markdown); page through long documents with character `offset` \
(default `max_chars` 30000). With `prompt` (a question), returns only a focused answer from the page."
                )
            }
            _ => {}
        }
    }
    let (before, _) = base_prompt_tokens(&baseline);
    println!(
        "batch24 before={before}, efficiency after={after} estimated base tokens; spec delta={}",
        after as i64 - before as i64
    );
    assert!(
        after <= before,
        "new pruning schema must not inflate the prompt: {before} -> {after}"
    );
}

/// A provider's filter/refusal/length stop must not complete native or
/// text-form calls by guessing their final brackets. Normal stops still can.
#[tokio::test]
async fn cut_replies_never_close_tool_arguments() {
    for finish in [
        "content_filter",
        "refusal",
        "length",
        "error",
        "cancelled",
        "unknown",
        "stop",
        "tool_calls",
        "function_call",
        "DONE-only",
        "missing",
        "number",
        "object",
        "false",
    ] {
        for text_form in [false, true] {
            let raw = r#"{"path":"new.txt","content":"hello""#;
            let delta = if text_form {
                json!({"content": format!("<tool_call>{{\"name\":\"write\",\"arguments\":{raw}}}</tool_call>")})
            } else {
                json!({"tool_calls": [{"index":0,"id":"cut","type":"function",
                    "function":{"name":"write","arguments":raw}}]})
            };
            let mut chunk = json!({"choices":[{"index":0,"delta":delta}]});
            if finish != "missing" {
                chunk["choices"][0]["finish_reason"] = match finish {
                    "DONE-only" => serde_json::Value::Null,
                    "number" => json!(42),
                    "object" => json!({"reason":"stop"}),
                    "false" => json!(false),
                    _ => json!(finish),
                };
            }
            let h = harness(
                vec![Reply::Raw(vec![chunk]), Reply::Text("done".into())],
                100_000,
            )
            .await;
            let s = h.session().await;
            h.send(&s.id, "make the file").await;
            h.settle(&s.id).await;
            assert_eq!(
                h.work.join("new.txt").exists(),
                matches!(finish, "stop" | "tool_calls" | "function_call"),
                "{finish}, text={text_form}"
            );
        }
    }
}

#[tokio::test]
async fn a_grep_hit_counts_as_a_read_for_write() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "grep".into(),
                json!({"pattern":"first", "path":"notes.txt"}),
            )]),
            Reply::Tools(vec![(
                "write".into(),
                json!({"path":"notes.txt", "content":"replaced\n"}),
            )]),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("notes.txt"), "first\nsecond\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "find and replace it").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(!results[0].1 && !results[1].1, "{results:?}");
    assert_eq!(
        std::fs::read_to_string(h.work.join("notes.txt")).unwrap(),
        "replaced\n"
    );
}

#[tokio::test]
async fn an_oversized_existing_file_is_refused_without_reading_it_or_asking() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "write".into(),
                json!({"path":"huge.txt", "content":"mine\n"}),
            )]),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    let path = h.work.join("huge.txt");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(1 << 30)
        .unwrap();
    let s = h.session().await;
    set_permission(&h, &s.id, PermissionMode::Ask).await;
    h.send(&s.id, "replace it").await;
    h.settle(&s.id).await;
    // An approval would leave the turn waiting; this must settle unasked.
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1 && results[0].2.contains("too large to check safely"));
    assert_eq!(std::fs::metadata(path).unwrap().len(), 1 << 30);
}

/// A mixed batch cannot discard its malformed second item during aliases.
#[tokio::test]
async fn malformed_edit_batches_never_apply_a_valid_subset() {
    let invalid = [
        json!({"edits":[{"old_string":"first", "new_string":"FIRST"}, {"old_string":"second"}]}),
        json!({"replace":[{"old":"first", "new":"FIRST"}, {"old":"second"}]}),
        json!({"lines":[{"at":"1", "text":"FIRST"}, {"at":"2"}]}),
        json!({"old":"first"}),
        json!({"at":"1"}),
        json!({"edits":[{"old":"first", "new":"FIRST"}, null]}),
        json!({"edits":[{"old_string":"first", "old":"other", "new_string":"FIRST"}]}),
    ];
    for mut args in invalid {
        args["path"] = json!("notes.txt");
        for text in [false, true] {
            let edit = if text {
                Reply::Text(format!(
                    "<tool_call>{}</tool_call>",
                    json!({"name":"MultiEdit", "arguments":args})
                ))
            } else {
                Reply::Tools(vec![("MultiEdit".into(), args.clone())])
            };
            let h = harness(vec![edit, Reply::Text("done".into())], 100_000).await;
            std::fs::write(h.work.join("notes.txt"), "first\nsecond\n").unwrap();
            let s = h.session().await;
            h.send(&s.id, "make the edit").await;
            h.settle(&s.id).await;
            assert_eq!(
                std::fs::read_to_string(h.work.join("notes.txt")).unwrap(),
                "first\nsecond\n",
                "text={text} {args}"
            );
            let results = tool_results(&h.entries(&s.id).await);
            assert!(
                results.is_empty() || results.iter().all(|r| r.1),
                "text={text} {args}: {results:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_file_growing_during_approval_is_bounded_again_before_write() {
    let h = harness(
        vec![
            Reply::Tools(vec![("read".into(), json!({"path":"growing.txt"}))]),
            Reply::Tools(vec![(
                "write".into(),
                json!({"path":"growing.txt", "content":"replacement\n"}),
            )]),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    let path = h.work.join("growing.txt");
    // An informed write passes the advisory guard, then Ask waits.
    std::fs::write(&path, "old-content\n").unwrap();
    let s = h.session().await;
    set_permission(&h, &s.id, PermissionMode::Ask).await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "replace it").await;
    let request = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                break request;
            }
        }
    })
    .await
    .expect("write approval");
    let grown = (16 << 20) + 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(grown)
        .unwrap();
    h.engine
        .handle(
            Request::RespondApproval {
                session_id: s.id.clone(),
                request_id: request.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[1].1 && results[1].2.contains("too large"),
        "{results:?}"
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), grown);
}

#[tokio::test]
async fn done_only_text_is_preserved_without_synthesizing_a_tool() {
    let h = harness(
        vec![Reply::Raw(vec![
            json!({"choices":[{"delta":{"content":"ordinary reply"}}]}),
        ])],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hello").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert!(tool_results(&entries).is_empty());
    assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Assistant{content, stop: tessera_proto::StopReason::Stop, ..} if content.iter().any(|p| matches!(p, AssistantPart::Text{text} if text == "ordinary reply")))));
}

/// Terminal provider failures preserve partial text but cannot report success.
#[tokio::test]
async fn provider_error_stops_end_the_engine_as_unsuccessful() {
    use tessera_core::auth::{AuthStore, Credential};
    use tessera_core::config::Paths;
    use tessera_core::{Engine, EngineOptions};
    use tessera_proto::{NoticeLevel, StopReason, UserNotification};
    const ERROR_SENTINEL: &str = "provider-error-sentinel-never-display-97a1";
    for protocol in ["anthropic", "openai_responses", "openai_codex"] {
        for reason in [
            "missing",
            "null",
            "number",
            "unknown",
            "failed",
            "cancelled",
            "repetition",
        ] {
            for with_call in [false, true] {
                let text = if reason == "repetition" {
                    format!(
                        "Partial answer from the provider.\n\n{}",
                        "The configuration file is missing the entry for the linker path. "
                            .repeat(30)
                    )
                } else {
                    "Partial answer from the provider.".into()
                };
                // Provider metadata is not a user-facing failure notice.
                let mut terminal = json!({"error":{"message":ERROR_SENTINEL}});
                if reason != "missing" {
                    terminal[if protocol == "anthropic" {
                        "stop_reason"
                    } else {
                        "status"
                    }] = match reason {
                        "null" => serde_json::Value::Null,
                        "number" => json!(42),
                        "repetition" => json!("failed"),
                        _ => json!(reason),
                    };
                }
                let events = if protocol == "anthropic" {
                    let mut events = vec![
                        json!({"type":"content_block_start", "index":0, "content_block":{"type":"text"}}),
                        json!({"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":text}}),
                    ];
                    if with_call {
                        events.extend([
                            json!({"type":"content_block_start", "index":1, "content_block":{"type":"tool_use", "id":"c1", "name":"write"}}),
                            json!({"type":"content_block_delta", "index":1, "delta":{"type":"input_json_delta", "partial_json":json!({"path":"should-not-exist.txt", "content":"unconfirmed"}).to_string()}}),
                        ]);
                    }
                    events.extend([
                        json!({"type":"message_delta", "delta":terminal}),
                        json!({"type":"message_stop"}),
                    ]);
                    events
                } else {
                    let mut events = vec![
                        json!({"type":"response.output_item.added", "item":{"id":"text1", "type":"message"}}),
                        json!({"type":"response.output_text.delta", "item_id":"text1", "delta":text}),
                    ];
                    if with_call {
                        events.extend([
                            json!({"type":"response.output_item.added", "item":{"id":"tool1", "type":"function_call", "call_id":"c1", "name":"write"}}),
                            json!({"type":"response.function_call_arguments.delta", "item_id":"tool1", "delta":json!({"path":"should-not-exist.txt", "content":"unconfirmed"}).to_string()}),
                        ]);
                    }
                    events.push(json!({"type":"response.done", "response":terminal}));
                    events
                };
                let body: String = events.iter().map(|v| format!("data: {v}\n\n")).collect();
                let outgoing =
                    std::sync::Arc::new(parking_lot::Mutex::new(Vec::<serde_json::Value>::new()));
                let received = outgoing.clone();
                let app = axum::Router::new().route(
                    if protocol == "anthropic" {
                        "/v1/messages"
                    } else {
                        "/v1/responses"
                    },
                    axum::routing::post(
                        move |axum::Json(request): axum::Json<serde_json::Value>| {
                            let body = body.clone();
                            let received = received.clone();
                            async move {
                                received.lock().push(request);
                                ([("content-type", "text/event-stream")], body)
                            }
                        },
                    ),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let root = format!("http://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                let mut h =
                    harness_agent(vec![], 100_000, "notify_after_minutes = 0.0000001").await;
                h.engine.shutdown().await;
                let config_path = h.dir.path().join("config.toml");
                let config = std::fs::read_to_string(&config_path)
                    .unwrap()
                    .replace(
                        "kind = \"openai_chat\"",
                        &format!(
                            "kind = \"{protocol}\"\nheaders = {{x-fixture = \"noncredential\"}}"
                        ),
                    )
                    .replace(
                        &h.mock.url,
                        if protocol == "anthropic" {
                            root.clone()
                        } else {
                            format!("{root}/v1")
                        }
                        .as_str(),
                    );
                std::fs::write(config_path, config).unwrap();
                if protocol == "openai_codex" {
                    AuthStore::load(Paths::at(h.dir.path()).auth_file())
                        .unwrap()
                        .set(
                            "mock",
                            Credential::Oauth {
                                access: "fixture-access-not-real".into(),
                                refresh: "fixture-refresh-not-real".into(),
                                expires: u64::MAX,
                                account_id: None,
                                email: None,
                            },
                        )
                        .unwrap();
                }
                h.engine =
                    Engine::open(Paths::at(h.dir.path()), EngineOptions { scheduler: false })
                        .await
                        .unwrap();
                let session: SessionInfo = serde_json::from_value(
                    h.engine
                        .handle(
                            Request::CreateSession {
                                cwd: Some(h.work.display().to_string()),
                                model: None,
                                title: Some("Provider failure fixture".into()),
                                bot_id: None,
                                private: false,
                                lean: false,
                            },
                            None,
                        )
                        .await
                        .unwrap(),
                )
                .unwrap();
                let mut events = h.engine.subscribe();
                h.send(&session.id, "answer and make the file").await;
                h.settle(&session.id).await;
                let context = format!("{protocol} {reason} call={with_call}");
                assert_eq!(
                    h.info(&session.id).await.status,
                    SessionStatus::Error,
                    "{context}"
                );
                let entries = h.entries(&session.id).await;
                assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Assistant {content, stop:StopReason::Error, error:Some(error),..}
                    if error == "The model did not finish its reply successfully. Partial text was kept. Send a message to try again."
                    && content.iter().any(|p| matches!(p, AssistantPart::Text{text:received} if received == &text)))), "{context}: {entries:?}");
                assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::Notice {level:NoticeLevel::Error,text,..}
                    if text == "The model did not finish its reply successfully. Partial text was kept. Send a message to try again.")), "{context}");
                assert!(!entries.iter().any(|e| matches!(&e.body, EntryBody::User {author:Some(a), content} if a == "harness" && plain_text(content).starts_with("[Repetition]"))), "{context}: must not nudge a failed reply");
                assert!(
                    !serde_json::to_string(&entries)
                        .unwrap()
                        .contains(ERROR_SENTINEL),
                    "{context}: provider error metadata was disclosed"
                );
                assert!(!h.work.join("should-not-exist.txt").exists(), "{context}");
                if with_call {
                    assert!(
                        tool_results(&entries)
                            .iter()
                            .any(|r| r.1 && r.2.contains("Not executed")),
                        "{context}"
                    );
                }
                let notices: Vec<UserNotification> = serde_json::from_value(
                    h.engine
                        .handle(Request::ListNotifications, None)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(notices.len(), 1, "{context}: {notices:?}");
                assert_eq!(notices[0].title, "Stopped by an error", "{context}");
                assert!(
                    !serde_json::to_string(&notices)
                        .unwrap()
                        .contains(ERROR_SENTINEL),
                    "{context}: provider error metadata reached a notification"
                );
                while let Ok(event) = events.try_recv() {
                    assert!(
                        !serde_json::to_string(&event)
                            .unwrap()
                            .contains(ERROR_SENTINEL),
                        "{context}: provider error metadata reached a client event"
                    );
                    if let Event::Notification { notification } = event {
                        assert_ne!(notification.title, "Finished", "{context}");
                    }
                }
                h.send(&session.id, "sentinel follow-up fixture").await;
                h.settle(&session.id).await;
                {
                    let requests = outgoing.lock();
                    assert!(
                        requests
                            .iter()
                            .any(|r| r.to_string().contains("sentinel follow-up fixture")),
                        "{context}: follow-up was not dispatched"
                    );
                    assert!(
                        !serde_json::to_string(&*requests)
                            .unwrap()
                            .contains(ERROR_SENTINEL),
                        "{context}: error metadata leaked into the complete next model request"
                    );
                }
                h.engine.shutdown().await;
                server.abort();
            }
        }
    }
}
