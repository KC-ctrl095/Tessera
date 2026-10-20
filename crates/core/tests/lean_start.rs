//! A lean start (`[agent] lean_tools`) end to end: the core tools and a
//! one-line list of the others, which turn on when one is called; shorter
//! descriptions for the largest core tools; capped skills and passwords
//! lists with a way to list the rest. Off, every request is what it was.

mod common;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// The tools a lean start keeps off until one is called.
const LATER: &[&str] = &[
    "note",
    "whiteboard",
    "browser",
    "schedule",
    "notify",
    "share_file",
    "save_secret",
    "settings",
    "agents",
    "skill_manage",
];

/// What a lean start offers from the start (on a server with no machine,
/// bot or language server).
const CORE: &[&str] = &[
    "read",
    "write",
    "edit",
    "grep",
    "glob",
    "bash",
    "job",
    "web_fetch",
    "web_search",
    "task",
    "memory",
    "skill",
    "recall",
    "agent",
    "docs",
];

fn tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn description(req: &Value, name: &str) -> String {
    req["tools"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["function"]["name"] == name))
        .and_then(|t| t["function"]["description"].as_str())
        .unwrap_or("")
        .to_string()
}

fn parameters(req: &Value, name: &str) -> Value {
    req["tools"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["function"]["name"] == name))
        .map(|t| t["function"]["parameters"].clone())
        .unwrap_or(Value::Null)
}

fn system(req: &Value) -> String {
    req["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// Estimated tokens of the system prompt plus tool specs (the engine's own
/// estimator, as `code.rs` measures it).
fn prompt_tokens(req: &Value) -> u64 {
    let specs: Vec<tessera_core::llm::ToolSpec> = req["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| tessera_core::llm::ToolSpec {
                    name: t["function"]["name"].as_str().unwrap_or("").into(),
                    description: t["function"]["description"].as_str().unwrap_or("").into(),
                    parameters: t["function"]["parameters"].clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    tessera_core::context::estimate_request(&system(req), &specs, &[])
}

/// The first request of a new chat that says "hi".
async fn first_request(h: &Harness) -> Value {
    let before = h.mock.agent_requests().len();
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    h.mock.agent_requests()[before].clone()
}

/// The same server reopened with `agent` lines in `[agent]` (the working
/// directory, and so the prompt, stay the same).
async fn reopen(h: Harness, agent: &str) -> Harness {
    let path = h.dir.path().join("config.toml");
    let cfg = std::fs::read_to_string(&path).unwrap();
    let cfg = cfg
        .lines()
        .filter(|l| !l.starts_with("lean_tools"))
        .collect::<Vec<_>>()
        .join("\n")
        .replacen("[agent]\n", &format!("[agent]\n{agent}\n"), 1);
    std::fs::write(&path, cfg).unwrap();
    let engine = Engine::open(Paths::at(h.dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Harness { engine, ..h }
}

/// A lean start offers the core tools and lists the others in one line;
/// `edit`, `agent`, `task` and `memory` say the same in fewer words. Prints the
/// tokens a fresh chat sends before the user's first word, off and on.
#[tokio::test]
async fn a_lean_start_offers_the_core_tools_and_lists_the_rest() {
    let h = harness(vec![], 100_000).await;
    h.mock.push(Reply::Text("ok".into()));
    let off = first_request(&h).await;
    let h = reopen(h, "lean_tools = true").await;
    h.mock.push(Reply::Text("ok".into()));
    let on = first_request(&h).await;

    let names = tool_names(&on);
    for t in CORE {
        assert!(names.iter().any(|n| n == t), "{t} missing: {names:?}");
    }
    for t in LATER {
        assert!(!names.iter().any(|n| n == t), "{t} offered: {names:?}");
        assert!(
            tool_names(&off).iter().any(|n| n == t),
            "{t} not off: {names:?}"
        );
    }
    assert_eq!(names.len(), CORE.len(), "{names:?}");
    let prompt = system(&on);
    let line = prompt
        .lines()
        .skip_while(|l| *l != "# More tools")
        .nth(1)
        .unwrap_or_else(|| panic!("no list of the other tools:\n{prompt}"));
    for t in LATER {
        assert!(line.contains(&format!("`{t}` (")), "{t} not listed: {line}");
    }
    // Not mentioned when there is none (no machine, no other bot).
    assert!(!line.contains("machines") && !line.contains("message_bot"));
    assert!(!system(&off).contains("# More tools"));
    // Shorter, with the same rules.
    for t in ["edit", "agent", "task", "memory"] {
        let (short, long) = (description(&on, t), description(&off, t));
        assert!(short.len() < long.len(), "{t}: {short}");
    }
    // Only the words: every tool takes what it took.
    for t in CORE {
        assert_eq!(
            parameters(&on, t),
            parameters(&off, t),
            "{t}'s parameters changed"
        );
    }
    assert!(description(&on, "edit").contains("an older tag works"));
    assert!(description(&on, "agent").contains("self-contained"));
    assert!(
        description(&on, "agent").contains("Types: explore"),
        "the agent types stay"
    );

    let (n_off, n_on) = (prompt_tokens(&off), prompt_tokens(&on));
    eprintln!("before the first word: lean off {n_off}, lean on {n_on} estimated tokens");
    assert!(n_on <= 3_300, "lean start is {n_on} estimated tokens");
    assert!(n_off - n_on >= 1_900, "{n_off} → {n_on}");
}

/// The switch off (or not set) is today's request, byte for byte: every
/// tool with its own description, no list, nothing capped.
#[tokio::test]
async fn lean_tools_off_changes_nothing() {
    let h = harness(vec![], 100_000).await;
    write_skills(&h, 12);
    add_secrets(&h, 12).await;
    h.mock.push(Reply::Text("ok".into()));
    let unset = first_request(&h).await;
    let h = reopen(h, "lean_tools = false").await;
    h.mock.push(Reply::Text("ok".into()));
    let off = first_request(&h).await;
    assert_eq!(system(&unset), system(&off));
    assert_eq!(unset["tools"], off["tools"]);

    let names = tool_names(&off);
    for t in CORE.iter().chain(LATER) {
        assert!(names.iter().any(|n| n == t), "{t} missing: {names:?}");
    }
    use tessera_core::tools::Tool;
    assert_eq!(
        description(&off, "edit"),
        tessera_core::tools::files::Edit.description()
    );
    assert_eq!(
        description(&off, "memory"),
        tessera_core::tools::meta::MemoryTool.description()
    );
    assert!(description(&off, "agent").starts_with(tessera_core::tools::meta::AGENT_INTRO));
    assert!(
        tessera_core::tools::secrets::SaveSecret.schema()
            == off["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["function"]["name"] == "save_secret")
                .unwrap()["function"]["parameters"]
    );
    let prompt = system(&off);
    assert!(!prompt.contains("# More tools"));
    for i in 0..12 {
        assert!(prompt.contains(&format!("$KEY_{i:02}")), "{prompt}");
        assert!(prompt.contains(&format!("- skill-{i:02}: ")), "{prompt}");
    }
    assert!(!prompt.contains("more: `save_secret`") && !prompt.contains("…and"));
}

/// Calling a listed tool turns the others on from the next step, once; the
/// prompt stays the same (only the tool list changes). `docs` topic `tools`
/// does it too.
#[tokio::test]
async fn a_listed_tool_turns_on_when_called() {
    let h = harness_agent(vec![], 100_000, "lean_tools = true").await;
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "docs".into(),
        json!({"topic": "tools"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    h.send(&s.id, "list my notes").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("`note` was off"),
        "{results:?}"
    );
    assert!(!results[1].1, "the second call runs: {results:?}");
    assert!(
        results[2].2.starts_with("The other tools are already on"),
        "{results:?}"
    );
    let reqs = h.mock.agent_requests();
    let first = tool_names(&reqs[0]);
    let after = tool_names(&reqs[1]);
    assert!(!first.iter().any(|t| t == "note"));
    for t in LATER {
        assert!(after.iter().any(|n| n == t), "{t} not on: {after:?}");
    }
    assert_eq!(tool_names(&reqs[3]), after, "turned on once");
    // Turned on, the whiteboard is described in fewer words too.
    use tessera_core::tools::Tool;
    let own = tessera_core::tools::board::Whiteboard.description();
    let short = description(&reqs[1], "whiteboard");
    assert!(
        short.len() < own.len() && short.contains("only if asked"),
        "{short}"
    );
    assert_eq!(reqs[0]["messages"][0], reqs[1]["messages"][0]);

    // A new chat starts lean again; reading the docs topic turns it on.
    h.mock.push(Reply::Tools(vec![(
        "docs".into(),
        json!({"topic": "tools"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    let before = h.mock.agent_requests().len();
    h.send(&s.id, "what else can you do").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert!(!tool_names(&reqs[before]).iter().any(|t| t == "browser"));
    assert!(tool_names(&reqs[before + 1]).iter().any(|t| t == "browser"));
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0]
            .2
            .starts_with("The other tools are on from your next step")
    );
}

/// At a lean start the prompt lists 2,000 characters of skills (the most
/// used first) and 10 saved passwords (the most recently used first);
/// `save_secret` lists every one, names only.
#[tokio::test]
async fn a_lean_start_caps_skills_and_passwords() {
    let h = harness_agent(vec![], 100_000, "lean_tools = true").await;
    write_skills(&h, 12);
    add_secrets(&h, 12).await;
    // The last skill in name order is loaded once, the last password used once.
    h.mock.push(Reply::Tools(vec![(
        "skill".into(),
        json!({"name": "skill-11"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "test -n \"$KEY_11\" && echo set"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    set_auto(&h, &s.id).await;
    h.send(&s.id, "use the last ones").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[1].2.contains("set"), "{results:?}");

    h.mock.push(Reply::Tools(vec![(
        "save_secret".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Tools(vec![(
        "save_secret".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    let before = h.mock.agent_requests().len();
    h.send(&s.id, "which passwords do you have").await;
    h.settle(&s.id).await;
    let req = &h.mock.agent_requests()[before];
    let prompt = system(req);
    let index: String = prompt
        .lines()
        .skip_while(|l| *l != "## Skills")
        .take_while(|l| !l.is_empty() && !l.starts_with('#') || *l == "## Skills")
        .collect::<Vec<_>>()
        .join("\n");
    assert!(index.len() <= 2_100, "{} characters:\n{index}", index.len());
    let first_skill = index.lines().nth(2).unwrap_or("");
    assert!(first_skill.starts_with("- skill-11: "), "{index}");
    assert!(
        index.contains("more (the `skill` tool lists them all)"),
        "{index}"
    );

    let listed: Vec<&str> = prompt
        .lines()
        .filter(|l| l.starts_with("- $KEY_"))
        .collect();
    assert_eq!(listed.len(), 10, "{prompt}");
    assert!(prompt.contains("$KEY_11"), "recently used: {prompt}");
    assert!(
        prompt.contains("- …and 2 more: `save_secret` with `action: \"list\"` lists them all"),
        "{prompt}"
    );
    // Listing: off at first (turned on by the call), then every name.
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1 && results[0].2.contains("`save_secret` was off"));
    let all = &results[1].2;
    assert!(!results[1].1, "{all}");
    for i in 0..12 {
        assert!(all.contains(&format!("$KEY_{i:02}")), "{all}");
        assert!(!all.contains(&secret_value(i)), "a value leaked: {all}");
    }
    let spec = h.mock.agent_requests()[before + 1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "save_secret")
        .cloned()
        .unwrap();
    assert!(
        spec["function"]["parameters"]["properties"]["action"].is_object(),
        "{spec}"
    );
}

/// A set-up server like the user's (two computers, five bots, 25 passwords,
/// ten skills, memory half full): what a fresh chat sends before the user's
/// first word, lean off and on.
#[tokio::test]
async fn a_set_up_server_measured_both_ways() {
    let h = harness(vec![], 100_000).await;
    // Kept open for the test: a machine is offered while it is connected.
    let mut machines = Vec::new();
    for name in ["Desk", "Lab"] {
        let (conn, rx) = h.engine.node_conn(&format!("dev_{name}"));
        h.engine
            .node_request(
                Request::NodeHello {
                    name: name.into(),
                    os: "linux x86_64".into(),
                    version: "0.1.0".into(),
                    scope: MachineScope {
                        folders: vec![FolderAccess {
                            path: "/home/me/code".into(),
                            write: true,
                        }],
                        commands: true,
                        ..Default::default()
                    },
                    generation: 1,
                    sandbox: "on".into(),
                    sandbox_ok: true,
                    home: Some("/home/me".into()),
                    computer: None,
                    host: None,
                    features: None,
                    guard_key: None,
                },
                &conn,
            )
            .await
            .unwrap();
        machines.push((conn, rx));
    }
    for name in ["Nova", "Forge", "Scout", "Sage", "Pip"] {
        h.engine
            .handle(Request::SaveBot { bot: bot(name) }, None)
            .await
            .unwrap();
    }
    add_secrets(&h, 25).await;
    for i in 0..25 {
        h.engine
            .handle(
                Request::SetSecretTitle {
                    name: format!("KEY_{i:02}"),
                    title: format!("API key for service number {i}"),
                },
                None,
            )
            .await
            .unwrap();
    }
    write_skills(&h, 10);
    for (target, chars) in [(MemoryTarget::Memory, 2_000), (MemoryTarget::User, 1_000)] {
        let fact = "The user keeps their projects in ~/code and builds on the desktop. ";
        h.engine
            .handle(
                Request::SetMemory {
                    target,
                    content: fact.repeat(chars / fact.len()),
                },
                None,
            )
            .await
            .unwrap();
    }
    h.mock.push(Reply::Text("ok".into()));
    let off = first_request(&h).await;
    let h = reopen(h, "lean_tools = true").await;
    h.mock.push(Reply::Text("ok".into()));
    let on = first_request(&h).await;
    for t in ["machines", "message_bot"] {
        assert!(tool_names(&off).iter().any(|n| n == t), "{t} missing off");
        assert!(!tool_names(&on).iter().any(|n| n == t), "{t} offered on");
        assert!(system(&on).contains(&format!("`{t}` (")), "{t} not listed");
    }
    let (n_off, n_on) = (prompt_tokens(&off), prompt_tokens(&on));
    eprintln!(
        "set-up server, before the first word: lean off {n_off}, lean on {n_on} estimated tokens"
    );
    assert!(n_off - n_on >= 2_600, "{n_off} → {n_on}");
    drop(machines);
}

fn bot(name: &str) -> Bot {
    Bot {
        id: String::new(),
        name: name.into(),
        handle: String::new(),
        avatar: String::new(),
        color: String::new(),
        role: "helps with the house".into(),
        personality: format!("{name} is calm and brief."),
        instructions: String::new(),
        model: None,
        thinking: None,
        permission: PermissionMode::Default,
        tools: ToolPreset::Coding,
        allowed_tools: vec![],
        cwd: None,
        own_memory: false,
        share_user_profile: true,
        can_message: vec!["*".into()],
        askable: None,
        knowledge: Vec::new(),
        starters: Vec::new(),
        machines: None,
        secrets: None,
        created_at: 0,
        updated_at: 0,
    }
}

fn secret_value(i: usize) -> String {
    format!("value-{i:02}-Zq7d9Kx2Lm4Np6Rs8Tv0")
}

/// `n` tokens in Passwords & keys, `KEY_00`….
async fn add_secrets(h: &Harness, n: usize) {
    for i in 0..n {
        h.engine
            .handle(
                Request::SetSecret {
                    name: format!("KEY_{i:02}"),
                    value: secret_value(i),
                },
                None,
            )
            .await
            .unwrap();
    }
}

/// `n` user skills with long descriptions, `skill-00`….
fn write_skills(h: &Harness, n: usize) {
    for i in 0..n {
        let dir = h.dir.path().join(format!("skills/skill-{i:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: skill-{i:02}\ndescription: Procedure number {i} for a recurring task, with enough words to make the index long enough that a cap matters for it, and a few more words about when it applies and what it needs.\n---\n\nStep one. Step two.\n"
            ),
        )
        .unwrap();
    }
}

async fn set_auto(h: &Harness, id: &str) {
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: id.into(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(PermissionMode::Auto),
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
