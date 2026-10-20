//! End-to-end tests for subagents v2 (agent definitions, structured output,
//! worktree isolation, continuing a subagent) and rewind, against the
//! scripted mock model.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_proto::*;

async fn wait_for(
    h: &Harness,
    id: &str,
    what: &str,
    pred: impl Fn(&[Entry]) -> bool,
) -> Vec<Entry> {
    for _ in 0..300 {
        let entries = h.entries(id).await;
        if pred(&entries) {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}: {:#?}", h.entries(id).await);
}

fn reports(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a.starts_with("subagent:") => Some(plain_text(content)),
            _ => None,
        })
        .collect()
}

fn last_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => content.iter().find_map(|p| match p {
                AssistantPart::Text { text } if !text.is_empty() => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

fn first_user(req: &Value) -> String {
    req["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "user"))
        .map(|m| m["content"].to_string())
        .unwrap_or_default()
}

fn tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn tool_description(req: &Value, name: &str) -> String {
    req["tools"]
        .as_array()
        .and_then(|t| t.iter().find(|t| t["function"]["name"] == name))
        .and_then(|t| t["function"]["description"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The last request a subagent (found by a marker in its task) made.
fn child_request(h: &Harness, marker: &str) -> Value {
    h.mock
        .agent_requests()
        .into_iter()
        .rfind(|r| first_user(r).contains(marker))
        .unwrap_or_else(|| panic!("no request for {marker}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

async fn rewind(
    h: &Harness,
    id: &str,
    to_seq: u64,
    files: bool,
    conversation: bool,
    dry_run: bool,
) -> Result<RewindResult, ApiError> {
    h.engine
        .handle(
            Request::Rewind {
                session_id: id.into(),
                to_seq,
                files,
                conversation,
                dry_run,
            },
            None,
        )
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

fn user_seq(entries: &[Entry], text: &str) -> u64 {
    entries
        .iter()
        .find(|e| matches!(&e.body, EntryBody::User { content, author: None } if plain_text(content) == text))
        .map(|e| e.seq)
        .unwrap_or_else(|| panic!("no user message {text:?}"))
}

#[tokio::test]
async fn helper_cwd_is_scoped_before_any_child_starts() {
    let h = harness(vec![], 100_000).await;
    let repo = h.work.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("app.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let outside = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), h.work.join("escape")).unwrap();
    let s = h.session().await;
    for (tool, cwd, isolate) in [("agent", "repo", false), ("agents", "repo", true)] {
        h.mock.route(
            "<CWD>",
            Reply::Tools(vec![("read".into(), json!({"path":"app.txt"}))]),
        );
        h.mock.route("<CWD>", Reply::Text("read the file".into()));
        let mut args =
            json!({"wait":true,"tasks":[{"name":"folder helper","prompt":"<CWD>","cwd":cwd}]});
        if tool == "agents" {
            args["action"] = json!("start");
        }
        if isolate {
            args["tasks"][0]["isolation"] = json!("worktree");
        }
        h.mock.push(Reply::Tools(vec![(tool.into(), args)]));
        h.mock.push(Reply::Text("done".into()));
        h.send(&s.id, "start a scoped helper").await;
        h.settle(&s.id).await;
        assert!(!tool_results(&h.entries(&s.id).await).last().unwrap().1);
    }
    for cwd in [
        outside.path().display().to_string(),
        "../".into(),
        "missing".into(),
        "escape".into(),
    ] {
        h.mock.push(Reply::Tools(vec![(
            "agent".into(),
            json!({"wait":true,"tasks":[
                {"name":"valid first","prompt":"<CWD>","cwd":"repo"},
                {"name":"invalid second","prompt":"<CWD>","cwd":cwd}
            ]}),
        )]));
        h.mock.push(Reply::Text("refused".into()));
        h.send(&s.id, "check folder scope").await;
        h.settle(&s.id).await;
        let entries = h.entries(&s.id).await;
        let r = tool_results(&entries);
        assert!(r.last().unwrap().1, "{r:?}");
        assert!(r.last().unwrap().2.contains("cwd"), "{r:?}");
    }
    let snapshot: SessionSnapshot = serde_json::from_value(
        h.engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        snapshot.children.len(),
        2,
        "an invalid batch must start no children"
    );
    assert!(
        snapshot
            .children
            .iter()
            .any(|i| i.cwd == repo.display().to_string())
    );
    assert!(
        snapshot
            .children
            .iter()
            .any(|i| i.cwd.contains(".git/tessera/worktrees"))
    );
}

#[tokio::test]
async fn helper_cwd_allowed_folder_keeps_readonly_ceiling() {
    let h = harness(vec![], 100_000).await;
    let allowed = tempfile::tempdir().unwrap();
    std::fs::write(allowed.path().join("app.txt"), "readable\n").unwrap();
    h.engine
        .handle(
            Request::SetSecurity {
                sandbox: None,
                folders: Some(vec![FolderAccess {
                    path: allowed.path().display().to_string(),
                    write: false,
                }]),
                ask_outside: Some(false),
                redact_patterns: None,
                save_secrets_from_chat: None,
                secret_saving: None,
                secret_machines: None,
                secret_machines_picked: None,
                bot_handoff: None,
                default_permission: None,
            },
            None,
        )
        .await
        .unwrap();
    h.mock.route("<RO_CWD>", Reply::Text("read only".into()));
    h.mock.push(Reply::Tools(vec![("agents".into(), json!({"action":"start","wait":true,"tasks":[
        {"name":"read only folder","prompt":"<RO_CWD>","cwd":allowed.path(),"isolation":"worktree"}
    ]}))]));
    h.mock.push(Reply::Text("done".into()));
    let s = h.session().await;
    h.send(&s.id, "read allowed folder").await;
    h.settle(&s.id).await;
    let req = child_request(&h, "<RO_CWD>");
    assert!(!tool_names(&req).contains(&"write".into()));
    assert!(!tool_names(&req).contains(&"bash".into()));
}

#[tokio::test]
async fn agent_definitions_shape_the_subagent_and_are_listed_stably() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [
                    {"name": "scout", "agent": "explore", "prompt": "find the parser <EX1>"},
                    {"name": "linter", "agent": "lint", "prompt": "check style <L1>"}
                ]}),
            )]),
            Reply::Tools(vec![(
                "agent".into(),
                json!({"tasks": [{"name": "x", "agent": "nope", "prompt": "whatever"}]}),
            )]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    // A project definition, narrower than the default tool set.
    let dir = h.work.join(".tessera/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lint.md"),
        "---\ndescription: Checks code style.\ntools: [read, grep]\nreadonly: true\nmax_steps: 3\n---\nYou are LINT-ROLE-MARKER. Report style problems.\n",
    )
    .unwrap();
    h.mock
        .route("<EX1>", Reply::Text("parser is in src/parse.rs:10".into()));
    h.mock.route("<L1>", Reply::Text("style is fine".into()));
    let s = h.session().await;
    h.send(&s.id, "look around").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    assert!(
        results[0].2.contains("src/parse.rs:10") && results[0].2.contains("style is fine"),
        "{results:?}"
    );
    // An unknown type is refused before anything starts, naming the choices.
    assert!(results[1].1, "{results:?}");
    assert!(
        results[1].2.contains("no agent type `nope`") && results[1].2.contains("explore"),
        "{results:?}"
    );

    // The explorer: its role in the system prompt, read-only tools only.
    let scout = child_request(&h, "<EX1>");
    let system = scout["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("# Agent role") && system.contains("read-only code explorer"),
        "{system}"
    );
    let tools = tool_names(&scout);
    assert!(
        tools.contains(&"grep".to_string()) && tools.contains(&"read".to_string()),
        "{tools:?}"
    );
    for t in ["bash", "write", "edit", "agent", "whiteboard"] {
        assert!(
            !tools.contains(&t.to_string()),
            "explore got {t}: {tools:?}"
        );
    }
    // The project's own type: exactly the tools it lists.
    let linter = child_request(&h, "<L1>");
    assert!(
        linter["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("LINT-ROLE-MARKER")
    );
    assert_eq!(tool_names(&linter), ["read", "grep"]);

    // The parent sees every type in the `agent` description, identical at every step.
    let parent: Vec<Value> = h
        .mock
        .agent_requests()
        .into_iter()
        .filter(|r| first_user(r).contains("look around"))
        .collect();
    assert!(parent.len() >= 3, "{}", parent.len());
    let desc = tool_description(&parent[0], "agent");
    for name in [
        "explore (",
        "review (",
        "research (",
        "lint (Checks code style",
    ] {
        assert!(desc.contains(name), "{name} missing: {desc}");
    }
    assert!(desc.len() < 1_300, "{} bytes: {desc}", desc.len());
    for r in parent[1..].iter().filter(|r| r["tools"].is_array()) {
        assert_eq!(tool_description(r, "agent"), desc);
        assert_eq!(
            r["tools"], parent[0]["tools"],
            "tool list changed between steps"
        );
    }
}

#[tokio::test]
async fn structured_output_is_checked_with_one_retry_then_passed_through() {
    let schema = json!({"type": "object", "required": ["files"], "properties": {
        "files": {"type": "array", "items": {"type": "string"}}}});
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [
                    {"name": "lister", "prompt": "list the files <SO1>", "output": schema},
                    {"name": "namer", "prompt": "name them <SO2>", "output": "a JSON list of names"},
                    {"name": "stubborn", "prompt": "refuse <SO3>", "output": schema}
                ]}),
            )]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    // Prose first; the retry gets it right (in a code fence, which is fine).
    h.mock.route(
        "<SO1>",
        Reply::Text("Sure! The files are a.rs and b.rs.".into()),
    );
    h.mock.route(
        "<SO1>",
        Reply::Text("```json\n{\"files\": [\"a.rs\", \"b.rs\"]}\n```".into()),
    );
    h.mock
        .route("<SO2>", Reply::Text("Here they are: [\"x\", \"y\"]".into()));
    h.mock.route("<SO3>", Reply::Text("{\"items\": 3}".into()));
    h.mock
        .route("<SO3>", Reply::Text("I would rather not.".into()));
    let s = h.session().await;
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let out = &results[0].2;
    // Clean, compact JSON for the parent.
    assert!(
        out.contains("## lister") && out.contains("{\"files\":[\"a.rs\",\"b.rs\"]}"),
        "{out}"
    );
    assert!(out.contains("[\"x\",\"y\"]"), "{out}");
    assert!(
        !out.split("## Earlier").next().unwrap().contains("Sure!"),
        "{out}"
    );
    assert!(out.contains("Earlier report (historical)"), "{out}");
    // After one retry the raw text comes through, labelled.
    assert!(
        out.contains("not in the requested format: $.files is missing")
            && out.contains("{\"items\": 3}"),
        "{out}"
    );
    // The task told it the format; the retry asked again, with no tools.
    let first = h
        .mock
        .agent_requests()
        .into_iter()
        .find(|r| first_user(r).contains("<SO1>"))
        .unwrap();
    assert!(
        first_user(&first).contains("reply with only a JSON value"),
        "{}",
        first_user(&first)
    );
    let retry = child_request(&h, "<SO1>");
    assert!(retry.to_string().contains("[Output check]"));
    assert!(
        retry["tools"].as_array().is_none_or(|t| t.is_empty()),
        "{:?}",
        retry["tools"]
    );
    // Exactly one retry each.
    let asked = |m: &str| {
        h.mock
            .agent_requests()
            .iter()
            .filter(|r| first_user(r).contains(m))
            .count()
    };
    assert_eq!((asked("<SO1>"), asked("<SO2>"), asked("<SO3>")), (2, 1, 2));
}

#[tokio::test]
async fn worktree_isolation_keeps_changes_apart_until_applied() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [
                    {"name": "iso", "prompt": "edit things <WT1>", "isolation": "worktree"},
                    {"name": "iso2", "prompt": "edit the same thing <WT2>", "isolation": "worktree"}
                ]}),
            )]),
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "apply", "name": "iso"}),
            )]),
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "apply", "name": "iso2"}),
            )]),
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "discard", "name": "iso2"}),
            )]),
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "apply", "name": "iso2"}),
            )]),
            Reply::Text("all done".into()),
        ],
        100_000,
    )
    .await;
    let repo = &h.work;
    git(repo, &["init", "-q"]);
    std::fs::write(repo.join("app.txt"), "v1\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    // Uncommitted work the subagents must see.
    std::fs::write(repo.join("notes.txt"), "draft\n").unwrap();
    h.mock.route(
        "<WT1>",
        Reply::Tools(vec![
            ("read".into(), json!({"paths": ["notes.txt", "app.txt"]})),
            (
                "write".into(),
                json!({"path": "app.txt", "content": "v2\n"}),
            ),
            (
                "write".into(),
                json!({"path": "new.txt", "content": "fresh\n"}),
            ),
        ]),
    );
    h.mock.route(
        "<WT1>",
        Reply::Text("changed app.txt, added new.txt".into()),
    );
    h.mock.route(
        "<WT2>",
        Reply::Tools(vec![
            ("read".into(), json!({"path": "app.txt"})),
            (
                "write".into(),
                json!({"path": "app.txt", "content": "conflicting\n"}),
            ),
        ]),
    );
    h.mock
        .route("<WT2>", Reply::Text("changed app.txt too".into()));
    let s = h.session().await;
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    let report = &results[0].2;
    assert!(
        report.contains("Changes (in its private worktree, not in your files yet): 2 files"),
        "{report}"
    );
    assert!(report.contains("app.txt, new.txt"), "{report}");
    // Each worked in its own checkout inside the repository's git directory,
    // which is gone again; it saw the parent's uncommitted file there.
    let child = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d), ..
            } => d["subagents"][0]["session_id"].as_str().map(str::to_string),
            _ => None,
        })
        .unwrap();
    let cwd = h.info(&child).await.cwd;
    assert!(cwd.contains(".git/tessera/worktrees/"), "{cwd}");
    assert!(!Path::new(&cwd).exists());
    let child_results = tool_results(&h.entries(&child).await);
    assert!(child_results[0].2.contains("draft"), "{child_results:?}");
    // Applying brings the first one's changes in...
    assert!(!results[1].1, "{results:?}");
    assert!(
        results[1].2.contains("Applied \"iso\"'s changes"),
        "{results:?}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("app.txt")).unwrap(),
        "v2\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("new.txt")).unwrap(),
        "fresh\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "draft\n"
    );
    // ...the second now conflicts and is refused without touching anything...
    assert!(results[2].1, "{results:?}");
    assert!(
        results[2].2.contains("conflict") && results[2].2.contains("Nothing was changed"),
        "{results:?}"
    );
    // ...and can be discarded, after which there is nothing to apply.
    assert!(
        results[3].2.contains("Discarded \"iso2\"'s changes"),
        "{results:?}"
    );
    assert!(
        results[4].1 && results[4].2.contains("discarded"),
        "{results:?}"
    );
    // No worktree is left registered with git.
    assert_eq!(git(repo, &["worktree", "list"]).lines().count(), 1);
    assert!(
        !repo
            .join(".git/tessera/worktrees")
            .read_dir()
            .is_ok_and(|mut d| d.next().is_some())
    );

    // Outside a repository, isolation is refused with the reason.
    let plain = tempfile::tempdir().unwrap();
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"tasks": [{"name": "nope", "prompt": "x", "isolation": "worktree"}]}),
    )]));
    h.mock.push(Reply::Text("understood".into()));
    let other: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateSession {
                    cwd: Some(plain.path().display().to_string()),
                    model: None,
                    title: None,
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
    h.send(&other.id, "try").await;
    h.settle(&other.id).await;
    let results = tool_results(&h.entries(&other.id).await);
    assert!(
        results[0].1 && results[0].2.contains("needs a git repository"),
        "{results:?}"
    );
}

#[tokio::test]
async fn a_finished_subagent_can_be_continued_with_its_context() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"tasks": [{"name": "helper", "prompt": "count the apples <C1>"}]}),
            )]),
            Reply::Text("started the helper".into()),
            // Its report is in: ask it for more.
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "message", "name": "helper", "text": "now double it"}),
            )]),
            Reply::Text("asked for more".into()),
            Reply::Text("final: 6".into()),
        ],
        100_000,
    )
    .await;
    h.mock.route(
        "<C1>",
        Reply::Slow(Duration::from_millis(300), "there are 3 apples".into()),
    );
    // The same session answers the follow-up (its first message is the task).
    h.mock.route(
        "<C1>",
        Reply::Slow(Duration::from_millis(300), "doubled: 6 apples".into()),
    );
    let s = h.session().await;
    h.send(&s.id, "count apples").await;
    let entries = wait_for(&h, &s.id, "both reports and the final answer", |e| {
        reports(e).len() == 2 && last_text(e).contains("final")
    })
    .await;
    let r = reports(&entries);
    assert!(r[0].contains("3 apples"), "{r:?}");
    assert!(
        r[1].contains("Report from subagent \"helper\"") && r[1].contains("doubled: 6"),
        "{r:?}"
    );
    let results = tool_results(&entries);
    assert!(results[1].2.contains("Sent to \"helper\""), "{results:?}");
    // It continued with its earlier context.
    let req = child_request(&h, "<C1>");
    let text = req.to_string();
    assert!(
        text.contains("there are 3 apples") && text.contains("now double it"),
        "{text}"
    );
    // Continuing a running or unknown subagent is refused.
    h.mock.push(Reply::Tools(vec![(
        "agents".into(),
        json!({"action": "message", "name": "ghost", "text": "hi"}),
    )]));
    h.mock.push(Reply::Text("no such one".into()));
    h.send(&s.id, "poke").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results.last().unwrap().1 && results.last().unwrap().2.contains("no subagent `ghost`"),
        "{results:?}"
    );
}

#[tokio::test]
async fn rewind_restores_files_and_keeps_the_abandoned_history_as_a_branch() {
    let h = harness(
        vec![
            // Turn 1.
            Reply::Tools(vec![(
                "write".into(),
                json!({"path": "a.txt", "content": "A1\n"}),
            )]),
            Reply::Text("turn one done".into()),
            // Turn 2: edits, a created file, a subagent's file, a shell command.
            // (`write` replaces only a file read first.)
            Reply::Tools(vec![
                ("read".into(), json!({"path": "keep.txt"})),
                (
                    "write".into(),
                    json!({"path": "keep.txt", "content": "changed\n"}),
                ),
                ("write".into(), json!({"path": "new.txt", "content": "N\n"})),
                (
                    "write".into(),
                    json!({"path": "touched.txt", "content": "agent\n"}),
                ),
                ("write".into(), json!({"path": "a.txt", "content": "A2\n"})),
            ]),
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [{"name": "sub", "prompt": "make a file <SUB1>"}]}),
            )]),
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "echo hi > shell.txt"}),
            )]),
            Reply::Text("turn two done".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("keep.txt"), "orig\n").unwrap();
    h.mock.route(
        "<SUB1>",
        Reply::Tools(vec![(
            "write".into(),
            json!({"path": "sub.txt", "content": "S\n"}),
        )]),
    );
    h.mock.route("<SUB1>", Reply::Text("made sub.txt".into()));
    let s = h.session().await;
    h.send(&s.id, "first").await;
    h.settle(&s.id).await;
    h.send(&s.id, "second").await;
    h.settle(&s.id).await;
    // The user edits a file the agent created.
    std::fs::write(h.work.join("touched.txt"), "user edit\n").unwrap();
    let entries = h.entries(&s.id).await;
    let before = entries.len();
    let to_seq = user_seq(&entries, "second") - 1;

    // A dry run says what would happen and changes nothing.
    let dry = rewind(&h, &s.id, to_seq, true, true, true).await.unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.to_seq, to_seq);
    assert_eq!(dry.restored, ["a.txt", "keep.txt"]);
    assert_eq!(dry.deleted, ["new.txt", "sub.txt"]);
    assert_eq!(dry.skipped.len(), 1);
    assert_eq!(dry.skipped[0].path, "touched.txt");
    assert!(
        dry.skipped[0].reason.contains("changed since it wrote it"),
        "{:?}",
        dry.skipped
    );
    assert_eq!(dry.not_undone, ["echo hi > shell.txt"]);
    assert!(dry.removed_entries > 0 && dry.branch.is_none());
    assert_eq!(
        std::fs::read_to_string(h.work.join("keep.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(h.entries(&s.id).await.len(), before);

    // A point between a tool call and its result moves back before the call.
    let call_seq = entries
        .iter()
        .find(|e| e.seq > to_seq && matches!(&e.body, EntryBody::Assistant { .. }))
        .unwrap()
        .seq;
    let moved = rewind(&h, &s.id, call_seq, true, false, true)
        .await
        .unwrap();
    assert_eq!(moved.to_seq, call_seq - 1);

    let mut events = h.engine.subscribe();
    let done = rewind(&h, &s.id, to_seq, true, true, false).await.unwrap();
    assert_eq!(done.restored, dry.restored);
    assert_eq!(done.deleted, dry.deleted);
    // Files: back as they were after turn one; the user's edit survives.
    let read = |f: &str| std::fs::read_to_string(h.work.join(f)).ok();
    assert_eq!(read("keep.txt").as_deref(), Some("orig\n"));
    assert_eq!(read("a.txt").as_deref(), Some("A1\n"));
    assert_eq!(read("new.txt"), None);
    assert_eq!(read("sub.txt"), None, "a subagent's edits are rewound too");
    assert_eq!(read("touched.txt").as_deref(), Some("user edit\n"));
    assert_eq!(
        read("shell.txt").as_deref(),
        Some("hi\n"),
        "shell changes are not undone"
    );
    // Conversation: cut back, and the full history lives on in a branch.
    let now = h.entries(&s.id).await;
    assert_eq!(now.last().unwrap().seq, to_seq);
    assert_eq!(done.session.last_seq, to_seq);
    let branch = done.branch.clone().unwrap();
    assert_eq!(branch.parent_id.as_deref(), Some(s.id.as_str()));
    assert!(branch.title.ends_with("(rewound)"), "{}", branch.title);
    assert_eq!(h.entries(&branch.id).await.len(), before);
    let mut rewound_event = false;
    while let Ok(ev) = events.try_recv() {
        if let Event::SessionRewound {
            session_id,
            to_seq: t,
        } = ev
        {
            rewound_event = session_id == s.id && t == to_seq;
        }
    }
    assert!(rewound_event);
    // The tree: the branch is listed under the session it came from.
    let list: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        list.iter()
            .any(|i| i.id == branch.id && i.parent_id.as_deref() == Some(s.id.as_str()))
    );

    // The session carries on from the rewound point.
    h.mock.push(Reply::Text("fresh start".into()));
    h.send(&s.id, "third").await;
    h.settle(&s.id).await;
    let now = h.entries(&s.id).await;
    assert_eq!(user_seq(&now, "third"), to_seq + 1);
    assert_eq!(last_text(&now), "fresh start");
    // The model no longer sees the abandoned turn.
    let last = h.mock.agent_requests().last().unwrap().to_string();
    assert!(!last.contains("turn two done") && last.contains("turn one done"));

    // The branch still knows its own file history: rewinding it to the same
    // point finds the same edits (already undone on disk).
    let again = rewind(&h, &branch.id, to_seq, true, false, true)
        .await
        .unwrap();
    assert!(
        again.restored.is_empty() && again.not_undone == ["echo hi > shell.txt"],
        "{again:?}"
    );
}

#[tokio::test]
async fn rewinding_files_only_tells_the_agent_and_is_refused_while_running() {
    let h = harness(
        vec![
            Reply::Tools(vec![
                ("read".into(), json!({"path": "f.txt"})),
                ("write".into(), json!({"path": "f.txt", "content": "new\n"})),
            ]),
            Reply::Text("wrote it".into()),
            Reply::Slow(Duration::from_secs(2), "slow answer".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("f.txt"), "old\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "change f").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let to_seq = user_seq(&entries, "change f");
    let res = rewind(&h, &s.id, to_seq, true, false, false).await.unwrap();
    assert_eq!(res.restored, ["f.txt"]);
    assert_eq!(
        std::fs::read_to_string(h.work.join("f.txt")).unwrap(),
        "old\n"
    );
    // The history stays, with a note so the agent knows its edit is gone.
    let now = h.entries(&s.id).await;
    assert_eq!(now.len(), entries.len() + 1);
    match &now.last().unwrap().body {
        EntryBody::User { content, author } => {
            assert_eq!(author.as_deref(), Some("harness"));
            assert!(
                plain_text(content).contains("Restored: f.txt"),
                "{}",
                plain_text(content)
            );
        }
        other => panic!("{other:?}"),
    }
    // Nothing to choose, or a running session: refused.
    assert!(
        rewind(&h, &s.id, to_seq, false, false, false)
            .await
            .is_err()
    );
    h.send(&s.id, "take your time").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let err = rewind(&h, &s.id, 0, true, true, false).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    h.settle(&s.id).await;
}

#[tokio::test]
async fn settings_next_to_the_tasks_apply_to_each_task() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "agent": "explore", "tasks": [
                    {"name": "a", "prompt": "look <TL1>"},
                    {"name": "b", "prompt": "look <TL2>", "agent": "research"}
                ]}),
            )]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    h.mock.route("<TL1>", Reply::Text("found".into()));
    h.mock.route("<TL2>", Reply::Text("researched".into()));
    let s = h.session().await;
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    let sys = |m: &str| {
        child_request(&h, m)["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(sys("<TL1>").contains("read-only code explorer"));
    // A task's own setting wins.
    assert!(sys("<TL2>").contains("careful researcher"));
}

#[tokio::test]
async fn helpers_working_at_once_is_saved_and_used_without_a_restart() {
    let h = harness_agent(vec![], 100_000, "max_subagents = 3").await;
    let get = |r: Value| serde_json::from_value::<HelperSettings>(r).unwrap();
    let now = get(h.engine.handle(Request::GetHelpers, None).await.unwrap());
    assert_eq!((now.at_once, now.max), (3, u32::MAX));
    assert_eq!((now.steps, now.minutes), (Some(40), Some(15)));
    assert!(now.capacity.is_some_and(|c| c >= 2));
    // Any number, well past the old presets; steps and minutes with it.
    let set = get(h
        .engine
        .handle(
            Request::SetHelpers {
                enabled: None,
                at_once: 33,
                steps: Some(120),
                minutes: None,
            },
            None,
        )
        .await
        .unwrap());
    assert_eq!(
        (set.at_once, set.steps, set.minutes),
        (33, Some(120), Some(15))
    );
    // Kept in config.toml, which a restarted server starts from.
    let toml = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(
        toml.contains("max_subagents = 33") && toml.contains("subagent_steps = 120"),
        "{toml}"
    );
    // No limit is explicit, also for defaults, and survives reopening.
    let no_limit = get(h
        .engine
        .handle(
            Request::SetHelpers {
                enabled: Some(true),
                at_once: 0,
                steps: Some(0),
                minutes: Some(0),
            },
            None,
        )
        .await
        .unwrap());
    assert_eq!(
        (no_limit.at_once, no_limit.steps, no_limit.minutes),
        (0, Some(0), Some(0))
    );
    h.engine.shutdown().await;
    let reopened = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        tessera_core::EngineOptions::default(),
    )
    .await
    .unwrap();
    let restored = get(reopened.handle(Request::GetHelpers, None).await.unwrap());
    assert_eq!(restored, no_limit);
}

#[tokio::test]
async fn stopping_a_chat_names_working_and_waiting_helpers() {
    for wait in [true, false] {
        let h = harness(
            vec![
                Reply::Tools(vec![(
                    "agent".into(),
                    json!({"wait": wait, "tasks": [
                        {"name": "Scout", "prompt": "Check the parser <STOP1>"},
                        {"name": "Reviewer", "prompt": "Check the tests <STOP2>"}
                    ]}),
                )]),
                Reply::Text("Helpers are working.".into()),
            ],
            100_000,
        )
        .await;
        h.engine
            .handle(
                Request::SetHelpers {
                    enabled: None,
                    at_once: 1,
                    steps: None,
                    minutes: None,
                },
                None,
            )
            .await
            .unwrap();
        h.mock.route(
            "<STOP1>",
            Reply::Slow(Duration::from_secs(60), "Parser checked.".into()),
        );
        h.mock.route(
            "<STOP2>",
            Reply::Slow(Duration::from_secs(60), "Tests checked.".into()),
        );
        let chat = h.session().await;
        h.send(&chat.id, "Check this in parallel.").await;
        let children = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let kids = h.engine.store().children_of(&chat.id).unwrap();
                if kids.len() == 2 && !h.mock.agent_requests().is_empty() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    break kids;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        h.engine
            .handle(
                Request::Abort {
                    session_id: chat.id.clone(),
                },
                None,
            )
            .await
            .unwrap();
        let entries = h.entries(&chat.id).await;
        let expected = format!(
            "Stopping this chat, including 2 helpers: {}.",
            children
                .iter()
                .map(|i| i.title.trim_start_matches("↳ "))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert!(expected.contains("Scout") && expected.contains("Reviewer"));
        assert!(
            entries.iter().any(|e| matches!(&e.body,
                EntryBody::Notice { text, .. } if text == &expected
            )),
            "wait {wait}: {entries:?}"
        );
        h.settle(&chat.id).await;
        for kid in children {
            h.settle(&kid.id).await;
            let (info, _) = h.engine.store().get_session(&kid.id).unwrap().unwrap();
            assert!(!matches!(
                info.status,
                SessionStatus::Running | SessionStatus::AwaitingApproval
            ));
            if info.steps == 0 {
                assert_eq!(info.status, SessionStatus::Interrupted);
            }
        }
    }
}

#[tokio::test]
async fn per_task_cwd_isolates_repositories_below_a_nonrepo_chat_folder() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait":true,"tasks":[
                    {"name":"a","prompt":"inspect A","cwd":"repo-a","isolation":"worktree"},
                    {"name":"b","prompt":"inspect B","cwd":"repo-b","isolation":"worktree"}
                ]}),
            )]),
            Reply::Text("repo findings".into()),
            Reply::Text("repo findings".into()),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    for name in ["repo-a", "repo-b"] {
        let repo = h.work.join(name);
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("app.txt"), name).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
    }
    assert!(!h.work.join(".git").exists());
    let chat = h.session().await;
    h.send(&chat.id, "inspect both repos").await;
    h.settle(&chat.id).await;
    let results = tool_results(&h.entries(&chat.id).await);
    assert!(!results[0].1, "{results:?}");
    assert!(results[0].2.contains("worktree:"), "{results:?}");
    assert!(
        results[0].2.contains("repo-a/.git/tessera/worktrees/")
            && results[0].2.contains("repo-b/.git/tessera/worktrees/"),
        "{results:?}"
    );
    for name in ["repo-a", "repo-b"] {
        assert_eq!(
            std::fs::read_to_string(h.work.join(name).join("app.txt")).unwrap(),
            name
        );
        assert_eq!(
            git(&h.work.join(name), &["worktree", "list"])
                .lines()
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn an_empty_native_helper_report_gets_one_cached_same_session_nudge() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait":true,"tasks":[{"name":"fixture","prompt":"EMPTY fixture task"}]}),
            )]),
            Reply::Tools(vec![(
                "write".into(),
                json!({"path":"fixture.txt","content":"uncommitted work"}),
            )]),
            Reply::Text(String::new()),
            Reply::Text("Finished: fixture.txt is changed and uncommitted.".into()),
            Reply::Text("Parent done".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "Run fixture helper").await;
    h.settle(&s.id).await;
    let result = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(
        !result.1 && result.2.contains("fixture.txt is changed and uncommitted"),
        "{result:?}"
    );
    let requests: Vec<_> = h
        .mock
        .agent_requests()
        .into_iter()
        .filter(|r| {
            r["messages"][1]["content"]
                .as_str()
                .is_some_and(|t| t.contains("EMPTY fixture task"))
        })
        .collect();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1]["messages"][0], requests[2]["messages"][0],
        "cached system prefix stays identical"
    );
    assert_eq!(
        requests[1]["tools"], requests[2]["tools"],
        "nudge doesn't change tools"
    );
    assert!(
        requests[2]
            .to_string()
            .contains("You ended without a report")
    );
    let parent_entries = h.entries(&s.id).await;
    let child_id = parent_entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d),
                tool,
                ..
            } if tool == "agent" => d["subagents"][0]["session_id"].as_str(),
            _ => None,
        })
        .unwrap();
    let child = h.info(child_id).await;
    let entries = h.entries(&child.id).await;
    assert_eq!(entries.iter().filter(|e| matches!(&e.body, EntryBody::User {author:Some(a),content} if a == "harness" && plain_text(content).starts_with("You ended without a report"))).count(), 1);
}

#[tokio::test]
async fn twice_empty_or_exhausted_native_helpers_fall_back_without_more_nudges() {
    for limit in [10, 1] {
        let mut replies = vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait":true,"tasks":[{"name":"fixture","prompt":"EMPTY fixture task"}]}),
            )]),
            Reply::Tools(vec![("glob".into(), json!({"pattern":"*"}))]),
            Reply::Text(String::new()),
        ];
        if limit > 1 {
            replies.push(Reply::Text(String::new()));
        }
        replies.push(Reply::Text("Parent done".into()));
        let h = harness_agent(replies, 100_000, &format!("subagent_steps = {limit}")).await;
        let s = h.session().await;
        h.send(&s.id, "Run fixture helper").await;
        h.settle(&s.id).await;
        let result = tool_results(&h.entries(&s.id).await).remove(0);
        assert!(
            result.1 && result.2.contains("without a final answer"),
            "{result:?}"
        );
        let parent_entries = h.entries(&s.id).await;
        let child_id = parent_entries
            .iter()
            .find_map(|e| match &e.body {
                EntryBody::ToolResult {
                    details: Some(d),
                    tool,
                    ..
                } if tool == "agent" => d["subagents"][0]["session_id"].as_str(),
                _ => None,
            })
            .unwrap();
        let child = h.info(child_id).await;
        let entries = h.entries(&child.id).await;
        assert_eq!(entries.iter().filter(|e| matches!(&e.body, EntryBody::User {author:Some(a),content} if a == "harness" && plain_text(content).starts_with("You ended without a report"))).count(), usize::from(limit > 1));
        assert_eq!(child.steps, 1, "budget isn't reset");
    }
}

/// The user's own agent types live in Tessera's settings folder, which
/// tools and commands never open: `docs subagents` sent agents there and a
/// scheduled bot was refused on every run. Agents see and save types
/// through `agents` instead; the folder itself stays closed.
#[tokio::test]
async fn agent_types_are_seen_and_saved_through_the_agents_tool() {
    let def = "---\nname: auditor\ndescription: Audits the lab.\ntools: read, grep\nreadonly: true\nnetwork: true\nmax_steps: 400\nmax_minutes: 180\n---\nYou audit the lab and report findings.";
    let h = harness_agent(
        vec![
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "define", "name": "auditor", "text": def}),
            )]),
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action": "types", "name": "auditor"}),
            )]),
            Reply::Tools(vec![("agents".into(), json!({"action": "types"}))]),
            Reply::Text("done".into()),
        ],
        100_000,
        "subagent_steps = 8\nsubagent_minutes = 3",
    )
    .await;
    let file = h.dir.path().join("agents/auditor.md");
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "save the auditor type").await;
    let request = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("saving a type asks");
    assert_eq!(request.tool, "agents");
    assert!(
        request.summary.contains("max_steps: 400"),
        "{}",
        request.summary
    );
    for want in [
        "effective budget: 8 steps, 3 min",
        "configured limits: 8 steps, 3 min",
        "network: yes",
    ] {
        assert!(request.reason.contains(want), "{want}: {}", request.reason);
    }
    assert!(
        request.summary.contains("You audit the lab"),
        "{}",
        request.summary
    );
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
        !results[0].1 && results[0].2.contains("Saved the agent type `auditor`"),
        "{results:?}"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("You audit the lab")
    );
    let one = &results[1].2;
    for want in [
        "auditor (the user's)",
        "tools: read, grep",
        "read-only: yes",
        "effective budget: 8 steps, 3 min",
        "network: yes",
        "You audit the lab and report findings.",
    ] {
        assert!(one.contains(want), "{want}: {one}");
    }
    let all = &results[2].2;
    assert!(
        all.contains("- review (built in)") && all.contains("- auditor (the user's)"),
        "{all}"
    );
}

/// Full autonomy deliberately saves types without a card, like skills.
/// The acknowledgement still shows limits; saved frontmatter cannot widen them.
#[tokio::test]
async fn auto_saves_agent_types_without_a_card_but_keeps_configured_limits() {
    let def = "---\nname: auditor\ntools: glob\nreadonly: true\nnetwork: true\nmax_steps: 400\nmax_minutes: 180\n---\nReport what you find.";
    let h = harness_agent(
        vec![
            Reply::Tools(vec![("agents".into(), json!({"action":"define","name":"auditor","text":def}))]),
            Reply::Tools(vec![("agent".into(), json!({"wait":true,"tasks":[{"name":"limited","agent":"auditor","prompt":"Look around <LIMITED-TYPE>"}]}))]),
            Reply::Tools(vec![("glob".into(), json!({"pattern":"*"}))]),
            Reply::Text("Found the files.".into()),
            Reply::Text("Parent done.".into()),
        ],
        100_000,
        "permission = \"auto\"\nsubagent_steps = 1\nsubagent_minutes = 2",
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "Save and run the limited type.").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    assert!(
        !results[0].1 && results[0].2.contains("effective budget: 1 steps, 2 min"),
        "{results:?}"
    );
    assert!(results[0].2.contains("network: yes"), "{results:?}");
    assert_eq!(
        std::fs::read_to_string(h.dir.path().join("agents/auditor.md")).unwrap(),
        format!("{def}\n")
    );
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, Event::ApprovalRequested { .. }),
            "Auto must not open a card"
        );
    }
    let child_id = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                details: Some(d),
                ..
            } if tool == "agent" => d["subagents"][0]["session_id"].as_str(),
            _ => None,
        })
        .unwrap();
    assert_eq!(h.info(child_id).await.steps, 1);
    let request = child_request(&h, "<LIMITED-TYPE>");
    assert!(request["messages"].to_string().contains("budget"));
}
/// A read-only reviewer used to have no command runner at all, so parents
/// relayed `git diff` through other bots. Now it gets `bash_readonly`:
/// sandboxed commands where possible, otherwise a disclosed command list.
#[tokio::test]
async fn read_only_helpers_run_restricted_commands_with_honest_disclosure() {
    // The sandbox confines writes only with Landlock ABI 2+ and user
    // namespaces; elsewhere only listed commands run.
    let sandboxed =
        tessera_core::sandbox::support().complete() && tessera_core::sandbox::confines_writes();
    if !sandboxed {
        eprintln!(
            "read_only_helpers…: no write-confining sandbox here ({}); checking the command list only",
            tessera_core::sandbox::describe()
        );
    }
    let mut child = vec![
        ("bash_readonly".into(), json!({"command": "pwd && ls"})),
        (
            "bash_readonly".into(),
            json!({"command": "touch made-by-helper.txt"}),
        ),
    ];
    if sandboxed {
        child.push((
            "bash_readonly".into(),
            json!({"command": "touch \"$TMPDIR/scratch-ok\" && ls \"$TMPDIR\""}),
        ));
        child.push((
            "bash_readonly".into(),
            json!({"command": "(exec 3<>/dev/tcp/127.0.0.1/9) 2>&1; (exec 3<>/dev/tcp/192.0.2.1/9) 2>&1; true"}),
        ));
    }
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [{"name": "rev", "prompt": "Review the change.", "readonly": true}]}),
            )]),
            Reply::Tools(child),
            Reply::Text("reviewed".into()),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("a.txt"), "x").unwrap();
    let s = h.session().await;
    h.send(&s.id, "review it").await;
    h.settle(&s.id).await;
    let child_id = h
        .entries(&s.id)
        .await
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d),
                tool,
                ..
            } if tool == "agent" => d["subagents"][0]["session_id"].as_str().map(str::to_string),
            _ => None,
        })
        .unwrap();
    // It is offered the read-only runner, not `bash` or `job`.
    let tools: Vec<String> = h
        .mock
        .agent_requests()
        .iter()
        .find(|r| r.to_string().contains("Review the change."))
        .unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            t["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(tools.iter().any(|t| t == "bash_readonly"), "{tools:?}");
    let description = tool_description(&child_request(&h, "Review the change."), "bash_readonly");
    assert!(
        description.contains("build scripts and tests unconfined"),
        "{description}"
    );
    assert!(
        !description.contains("can't change anything"),
        "{description}"
    );
    assert!(
        !tools.iter().any(|t| t == "bash" || t == "job"),
        "{tools:?}"
    );
    let results = tool_results(&h.entries(&child_id).await);
    assert!(
        !results[0].1 && results[0].2.contains("a.txt"),
        "{results:?}"
    );
    assert!(results[1].1, "{results:?}");
    assert!(!h.work.join("made-by-helper.txt").exists());
    if sandboxed {
        assert!(
            !results[2].1 && results[2].2.contains("scratch-ok"),
            "{results:?}"
        );
        // Loopback is up (refused), the rest of the network unreachable.
        assert!(results[3].2.contains("Connection refused"), "{results:?}");
        assert!(
            results[3].2.contains("Network is unreachable"),
            "{results:?}"
        );
    } else {
        assert!(results[1].2.contains("only git reads"), "{results:?}");
        let parent_results = tool_results(&h.entries(&s.id).await);
        assert!(
            parent_results[0]
                .2
                .contains("build scripts and tests unconfined"),
            "{parent_results:?}"
        );
        assert!(
            parent_results[0]
                .2
                .contains("change files and use the network"),
            "{parent_results:?}"
        );
    }
    // The scratch folder went with the helper.
    assert!(!tessera_core::tools::bash::scratch_dir(&child_id).exists());
}

/// Failed provider endings stay failures through both helper delivery paths,
/// including the fallback after a structured-output retry.
#[tokio::test]
async fn failed_helpers_keep_error_precedence_over_partial_notes_and_format_fallback() {
    const NOTICE: &str = "The model did not finish its reply successfully. Partial text was kept. Send a message to try again.";
    for wait in [true, false] {
        for structured_retry in [false, true] {
            for partial in ["", "Useful but unfinished helper notes."] {
                let mut task = json!({"name":"failed helper", "prompt":"CHILD failure fixture"});
                if structured_retry {
                    task["output"] = json!({"type":"object", "required":["files"]});
                }
                let h = harness(vec![], 100_000).await;
                h.mock.route(
                    "PARENT failure fixture",
                    Reply::Tools(vec![("agent".into(), json!({"wait":wait,"tasks":[task]}))]),
                );
                for _ in 0..3 {
                    h.mock.route(
                        "PARENT failure fixture",
                        Reply::Text("Parent saw the helper result.".into()),
                    );
                }
                if structured_retry {
                    h.mock.route(
                        "CHILD failure fixture",
                        Reply::Text("First unformatted report.".into()),
                    );
                }
                h.mock.route(
                    "CHILD failure fixture",
                    Reply::Raw(vec![json!({"choices":[{
                        "index":0,"delta":{"content":partial},"finish_reason":"content_filter", "error":{"message":"provider-error-sentinel-never-display-97a1"}
                    }]})]),
                );
                let s = h.session().await;
                h.send(&s.id, "PARENT failure fixture").await;
                h.settle(&s.id).await;
                let entries = wait_for(&h, &s.id, "the failed helper result", |entries| {
                    if wait {
                        !tool_results(entries).is_empty()
                    } else {
                        !reports(entries).is_empty()
                    }
                })
                .await;
                let child_id = entries
                    .iter()
                    .find_map(|e| match &e.body {
                        EntryBody::ToolResult {
                            details: Some(d),
                            tool,
                            ..
                        } if tool == "agent" => d["subagents"][0]["session_id"].as_str(),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(h.info(child_id).await.status, SessionStatus::Error);
                let result = if wait {
                    let result = tool_results(&entries).remove(0);
                    assert!(result.1, "{result:?}");
                    assert!(result.2.contains("failed helper (failed)"), "{result:?}");
                    result.2
                } else {
                    let result = reports(&entries).remove(0);
                    assert!(result.contains("The subagent did not finish"), "{result}");
                    result
                };
                assert!(result.contains(NOTICE), "{result}");
                assert!(
                    !result.contains("This report is not in the requested format"),
                    "{result}"
                );
                let expected_notes = partial;
                if structured_retry {
                    assert!(result.contains("Earlier report (historical)"), "{result}");
                    assert!(result.contains("First unformatted report."), "{result}");
                    if partial.is_empty() {
                        assert!(
                            !result
                                .split("## Earlier")
                                .next()
                                .unwrap()
                                .contains("First unformatted report."),
                            "{result}"
                        );
                    }
                }
                if !expected_notes.is_empty() {
                    assert!(
                        result.contains("Partial notes, not a final reply or report:"),
                        "{result}"
                    );
                    assert!(result.contains(expected_notes), "{result}");
                }
                h.send(&s.id, "sentinel follow-up fixture").await;
                h.settle(&s.id).await;
                let outgoing = h.mock.agent_requests();
                assert!(
                    outgoing
                        .iter()
                        .any(|r| r.to_string().contains("sentinel follow-up fixture")
                            && r.to_string().contains(NOTICE)),
                    "parent must receive labelled helper result in its follow-up"
                );
                assert!(
                    !serde_json::to_string(&outgoing)
                        .unwrap()
                        .contains("provider-error-sentinel-never-display-97a1")
                );
                let requests: Vec<_> = h
                    .mock
                    .agent_requests()
                    .into_iter()
                    .filter(|r| first_user(r).contains("CHILD failure fixture"))
                    .collect();
                assert_eq!(requests.len(), if structured_retry { 2 } else { 1 });
                if structured_retry {
                    assert!(requests[1].to_string().contains("[Output check]"));
                    assert!(requests[1]["tools"].as_array().is_none_or(|t| t.is_empty()));
                }
                h.engine.shutdown().await;
            }
        }
    }
}

#[tokio::test]
async fn failed_helper_readback_marks_old_success_as_historical() {
    const NOTICE: &str = "The model did not finish its reply successfully. Partial text was kept. Send a message to try again.";
    for structured_retry in [false, true] {
        for partial in ["", "Latest failed attempt notes."] {
            let h = harness(vec![], 100_000).await;
            let mut task = json!({"name":"readback helper", "prompt":"CHILD readback fixture"});
            if structured_retry {
                task["output"] = json!({"type":"object", "required":["files"]});
            }
            h.mock.route(
                "PARENT readback fixture",
                Reply::Tools(vec![("agent".into(), json!({"wait":true,"tasks":[task]}))]),
            );
            h.mock.route(
                "PARENT readback fixture",
                Reply::Text("Initial helper result received.".into()),
            );
            h.mock.route(
                "CHILD readback fixture",
                Reply::Text("Older successful report.".into()),
            );
            if structured_retry {
                h.mock.route(
                    "CHILD readback fixture",
                    Reply::Raw(vec![json!({"choices":[{
                        "index":0,"delta":{"content":partial},"finish_reason":"content_filter", "error":{"message":"provider-error-sentinel-never-display-97a1"}
                    }]})]),
                );
            }
            let s = h.session().await;
            h.send(&s.id, "PARENT readback fixture").await;
            h.settle(&s.id).await;
            if !structured_retry {
                h.mock.route("PARENT readback fixture", Reply::Tools(vec![(
                    "agents".into(), json!({"action":"message", "name":"readback helper", "text":"Continue the task."}),
                )]));
                h.mock.route(
                    "PARENT readback fixture",
                    Reply::Text("Continuation requested.".into()),
                );
                h.mock.route(
                    "PARENT readback fixture",
                    Reply::Text("Failed continuation received.".into()),
                );
                h.mock.route(
                    "CHILD readback fixture",
                    Reply::Raw(vec![json!({"choices":[{
                        "index":0,"delta":{"content":partial},"finish_reason":"content_filter", "error":{"message":"provider-error-sentinel-never-display-97a1"}
                    }]})]),
                );
                h.send(&s.id, "continue the helper").await;
                wait_for(&h, &s.id, "the failed continuation", |e| {
                    !reports(e).is_empty()
                })
                .await;
                h.settle(&s.id).await;
            }
            h.mock.route(
                "PARENT readback fixture",
                Reply::Tools(vec![(
                    "agents".into(),
                    json!({"action":"report", "name":"readback helper"}),
                )]),
            );
            h.mock.route(
                "PARENT readback fixture",
                Reply::Text("Readback received.".into()),
            );
            h.send(&s.id, "read the full helper report").await;
            h.settle(&s.id).await;
            let result = tool_results(&h.entries(&s.id).await).pop().unwrap();
            assert!(result.1, "{result:?}");
            assert!(result.2.starts_with(NOTICE), "{result:?}");
            assert!(
                result.2.contains("Earlier report (historical)"),
                "{result:?}"
            );
            assert!(result.2.contains("Older successful report."), "{result:?}");
            let latest = result.2.split("## Earlier").next().unwrap();
            assert!(!latest.contains("Older successful report."), "{result:?}");
            if partial.is_empty() {
                assert!(!latest.contains("Partial notes,"), "{result:?}");
            } else {
                assert!(
                    latest.contains("Partial notes, not a final reply or report:"),
                    "{result:?}"
                );
                assert!(latest.contains(partial), "{result:?}");
            }
            h.engine.shutdown().await;
        }
    }
}

#[tokio::test]
async fn stopping_a_structured_retry_keeps_partial_json_and_prose_labelled() {
    for wait in [true, false] {
        for partial in [r#"{"files":["partial.rs"]}"#, "Unfinished retry notes."] {
            let h = harness(vec![], 100_000).await;
            h.mock.route("PARENT stopped retry fixture", Reply::Tools(vec![(
                "agent".into(), json!({"wait":wait,"tasks":[{
                    "name":"stopped helper","prompt":"CHILD stopped retry fixture",
                    "output":{"type":"object","required":["files"],"properties":{"files":{"type":"array"}}}
                }]}),
            )]));
            h.mock.route(
                "PARENT stopped retry fixture",
                Reply::Text("Helper result received.".into()),
            );
            h.mock.route(
                "CHILD stopped retry fixture",
                Reply::Text("First unformatted report.".into()),
            );
            h.mock
                .route("CHILD stopped retry fixture", Reply::Hang(partial.into()));
            let s = h.session().await;
            let mut events = h.engine.subscribe();
            h.send(&s.id, "PARENT stopped retry fixture").await;
            let child = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Event::StreamDelta {
                        session_id,
                        delta: StreamDelta::Text { text, .. },
                    } = events.recv().await.unwrap()
                        && session_id != s.id
                        && text.contains(partial)
                    {
                        break session_id;
                    }
                }
            })
            .await
            .expect("the format retry did not stream its partial notes");
            h.engine
                .handle(
                    Request::Abort {
                        session_id: child.clone(),
                    },
                    None,
                )
                .await
                .unwrap();
            h.settle(&s.id).await;
            let entries = wait_for(&h, &s.id, "the stopped retry result", |e| {
                if wait {
                    !tool_results(e).is_empty()
                } else {
                    !reports(e).is_empty()
                }
            })
            .await;
            let result = if wait {
                tool_results(&entries).remove(0).2
            } else {
                reports(&entries).remove(0)
            };
            assert!(result.contains("stopped before it finished"), "{result}");
            assert!(
                result.contains("last notes, not a final report"),
                "{result}"
            );
            assert!(result.contains(partial), "{result}");
            assert!(
                !result.contains("This report is not in the requested format"),
                "{result}"
            );
            assert!(
                !result
                    .split("## Earlier")
                    .next()
                    .unwrap()
                    .contains("First unformatted report."),
                "{result}"
            );
            assert!(result.contains("Earlier report (historical)"), "{result}");
            h.mock.route(
                "PARENT stopped retry fixture",
                Reply::Tools(vec![(
                    "agents".into(),
                    json!({"action":"report","name":"stopped helper"}),
                )]),
            );
            h.mock.route(
                "PARENT stopped retry fixture",
                Reply::Text("Stopped readback received.".into()),
            );
            h.send(&s.id, "read back the stopped retry").await;
            h.settle(&s.id).await;
            let readback = tool_results(&h.entries(&s.id).await).pop().unwrap();
            assert!(
                readback.1 && readback.2.contains("stopped before it finished"),
                "{readback:?}"
            );
            assert!(readback.2.contains(partial), "{readback:?}");
            assert!(
                readback.2.contains("Earlier report (historical)"),
                "{readback:?}"
            );
            assert_eq!(h.info(&child).await.status, SessionStatus::Interrupted);
            let requests: Vec<_> = h
                .mock
                .agent_requests()
                .into_iter()
                .filter(|r| first_user(r).contains("CHILD stopped retry fixture"))
                .collect();
            assert_eq!(requests.len(), 2);
            assert!(requests[1].to_string().contains("[Output check]"));
            h.engine.shutdown().await;
        }
    }
}

#[tokio::test]
async fn stopped_and_exhausted_continuations_never_read_back_an_old_success_as_current() {
    for stopped in [true, false] {
        let h = harness_agent(vec![], 100_000, "subagent_steps = 1").await;
        std::fs::write(h.work.join("fixture.txt"), "readable").unwrap();
        h.mock.route("PARENT terminal fixture", Reply::Tools(vec![("agent".into(), json!({"wait":true,"tasks":[{"name":"terminal helper","prompt":"CHILD terminal fixture","max_steps":1}]}))]));
        h.mock.route(
            "PARENT terminal fixture",
            Reply::Text("First result received.".into()),
        );
        h.mock.route(
            "CHILD terminal fixture",
            Reply::Text("Older successful report.".into()),
        );
        let s = h.session().await;
        h.send(&s.id, "PARENT terminal fixture").await;
        h.settle(&s.id).await;
        h.mock.route(
            "PARENT terminal fixture",
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action":"message","name":"terminal helper","text":"Continue."}),
            )]),
        );
        h.mock.route(
            "PARENT terminal fixture",
            Reply::Text("Continuation queued.".into()),
        );
        h.mock.route(
            "PARENT terminal fixture",
            Reply::Text("Continuation result received.".into()),
        );
        if stopped {
            h.mock.route(
                "CHILD terminal fixture",
                Reply::Hang("Current stopped notes.".into()),
            );
        } else {
            h.mock.route(
                "CHILD terminal fixture",
                Reply::Tools(vec![("read".into(), json!({"path":"fixture.txt"}))]),
            );
            h.mock
                .route("CHILD terminal fixture", Reply::Text("".into()));
        }
        let previous_reports = reports(&h.entries(&s.id).await).len();
        let mut events = h.engine.subscribe();
        h.send(&s.id, "continue").await;
        if stopped {
            let child = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Event::StreamDelta {
                        session_id,
                        delta: StreamDelta::Text { text, .. },
                    } = events.recv().await.unwrap()
                        && session_id != s.id
                        && text.contains("Current stopped notes.")
                    {
                        break session_id;
                    }
                }
            })
            .await
            .unwrap();
            h.engine
                .handle(Request::Abort { session_id: child }, None)
                .await
                .unwrap();
        }
        wait_for(&h, &s.id, "the terminal continuation", |e| {
            reports(e).len() > previous_reports
        })
        .await;
        h.settle(&s.id).await;
        // Automatic follow-up is not guaranteed after a stopped attempt.
        // Retire its unused mock reply before the explicit readback turn.
        h.mock.state.routes.lock().retain(|(key, reply)| {
            !(key == "PARENT terminal fixture"
                && matches!(reply, Reply::Text(text) if text == "Continuation result received."))
        });
        h.mock.route(
            "PARENT terminal fixture",
            Reply::Tools(vec![(
                "agents".into(),
                json!({"action":"report","name":"terminal helper"}),
            )]),
        );
        h.mock.route(
            "PARENT terminal fixture",
            Reply::Text("Readback received.".into()),
        );
        let previous_results = tool_results(&h.entries(&s.id).await).len();
        h.send(&s.id, "read back").await;
        wait_for(&h, &s.id, "the requested report readback", |e| {
            tool_results(e).len() > previous_results
        })
        .await;
        h.settle(&s.id).await;
        let entries = h.entries(&s.id).await;
        let result = tool_results(&entries).pop().unwrap();
        assert!(result.1, "{result:?}; history {entries:?}");
        let current = result.2.split("## Earlier").next().unwrap();
        assert!(!current.contains("Older successful report."), "{result:?}");
        assert!(
            result.2.contains("Earlier report (historical)"),
            "{result:?}"
        );
        if stopped {
            assert!(current.contains("stopped before it finished"), "{result:?}");
            assert!(current.contains("Current stopped notes."), "{result:?}");
        } else {
            assert!(
                current.contains("exhausted its budget without a final answer"),
                "{result:?}"
            );
        }
        h.engine.shutdown().await;
    }
}
