//! MCP tools within a token budget (`mcp.rs`): descriptions capped like the
//! built-in tools', and a large server's tools left out of the prompt until
//! first used, with one line naming them; calling one turns that server's
//! tools on from the next step (one prompt-cache miss).

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};

/// An MCP server over stdio: `big` lists 100 tools with 5 KB descriptions,
/// `tiny` one tool; every call answers with the tool's name and arguments.
const FIXTURE: &str = r#"
import sys, json, time
kind = sys.argv[1]
# A server that takes a while to start (connects after the first prompt).
if len(sys.argv) > 2:
    time.sleep(float(sys.argv[2]))
if kind == "big":
    tools = [{"name": f"tool_{i}", "description": f"Tool {i}. " + "Long text. " * 470,
              "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
              "annotations": {"readOnlyHint": True}} for i in range(100)]
else:
    tools = [{"name": "ping", "description": "Answer pong.",
              "inputSchema": {"type": "object", "properties": {}},
              "annotations": {"readOnlyHint": True}}]
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m = msg["method"]
    if m == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": kind, "version": "1"}}
    elif m == "tools/list":
        res = {"tools": tools}
    elif m == "tools/call":
        p = msg["params"]
        res = {"content": [{"type": "text", "text": kind + ":" + p["name"] + ":" + json.dumps(p.get("arguments", {}), sort_keys=True)}]}
    else:
        res = {}
    print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
"#;

async fn mcp_harness(script: Vec<Reply>, window: u64, extra_mcp: &str) -> Harness {
    let h = mcp_harness_with(script, window, extra_mcp, "", "0").await;
    wait_for_mcp(&h).await;
    h
}

/// The MCP servers connect in the background: wait for both.
async fn wait_for_mcp(h: &Harness) {
    let mut tries = 0;
    while !(h.engine.has_tool("mcp__tiny__ping") && h.engine.has_tool("mcp__big__tool_99")) {
        tries += 1;
        assert!(tries < 200, "the MCP fixtures didn't connect");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// [`mcp_harness`] with `tail` config at the end and the big server taking
/// `delay` seconds to start, without waiting for it.
async fn mcp_harness_with(
    script: Vec<Reply>,
    window: u64,
    extra_mcp: &str,
    tail: &str,
    delay: &str,
) -> Harness {
    let mock = Mock::start(script).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let fixture = dir.path().join("mcp_fixture.py");
    std::fs::write(&fixture, FIXTURE).unwrap();
    let config = format!(
        r#"default_model = "mock/m"
[agent]
review_interval = 0
[tuning."*"]
small = "off"
check = false
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{url}"
discover = false
parallel = 4
[[provider.models]]
id = "m"
context_window = {window}
max_output = 2000
[[mcp]]
name = "big"
command = "python3"
args = ["{fixture}", "big", "{delay}"]
{extra_mcp}
[[mcp]]
name = "tiny"
command = "python3"
args = ["{fixture}", "tiny"]
{tail}
"#,
        url = mock.url,
        fixture = fixture.display(),
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
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

/// Estimated tokens of a request's tools.
fn tool_tokens(req: &Value, prefix: &str) -> u64 {
    req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["function"]["name"].as_str().unwrap().starts_with(prefix))
        .map(|t| {
            tessera_core::context::estimate_request(
                "",
                &[tessera_core::llm::ToolSpec {
                    name: t["function"]["name"].as_str().unwrap().into(),
                    description: t["function"]["description"].as_str().unwrap_or("").into(),
                    parameters: t["function"]["parameters"].clone(),
                }],
                &[],
            )
        })
        .sum()
}

#[tokio::test]
async fn a_large_mcp_server_waits_until_first_used() {
    let call = || Reply::Tools(vec![("mcp__big__tool_3".into(), json!({"q": "hello"}))]);
    let h = mcp_harness(
        vec![call(), call(), Reply::Text("done".into())],
        100_000,
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "use the big server").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 3);

    // Up front: the small server's tool, none of the big one's, and one
    // line naming them.
    let first = tool_names(&reqs[0]);
    assert!(first.contains(&"mcp__tiny__ping".to_string()), "{first:?}");
    assert!(
        !first.iter().any(|n| n.starts_with("mcp__big__")),
        "{first:?}"
    );
    let system = reqs[0]["messages"][0]["content"].as_str().unwrap();
    let line = system
        .lines()
        .skip_while(|l| *l != "# MCP tools")
        .nth(1)
        .unwrap_or_else(|| panic!("no MCP line in {system}"));
    assert!(
        line.contains("`mcp__big__*` (100 tools: tool_0, tool_1,") && !line.contains("tiny"),
        "{line}"
    );
    let line_tokens = tessera_core::context::estimate_request(line, &[], &[]);
    assert!(line_tokens < 500, "{line_tokens} tokens: {line}");

    // The first call turns the server on; the second runs.
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].1 && results[0].2.contains("was off until first used"),
        "{results:?}"
    );
    assert!(!results[1].1, "{results:?}");
    assert_eq!(results[1].2, r#"big:tool_3:{"q": "hello"}"#);
    let second = tool_names(&reqs[1]);
    assert_eq!(
        second
            .iter()
            .filter(|n| n.starts_with("mcp__big__"))
            .count(),
        100
    );
    // Capped like the built-in tools' descriptions (5 KB as sent).
    for t in reqs[1]["tools"].as_array().unwrap() {
        let name = t["function"]["name"].as_str().unwrap();
        if !name.starts_with("mcp__") {
            continue;
        }
        let d = t["function"]["description"].as_str().unwrap();
        assert!(
            d.chars().count() <= 600,
            "{name}: {} chars",
            d.chars().count()
        );
        if name.starts_with("mcp__big__") {
            assert!(d.starts_with("[big MCP] Tool "), "{d}");
        }
    }
    // What the big server would have cost up front, against the line.
    let big = tool_tokens(&reqs[1], "mcp__big__");
    assert!(big > 10 * line_tokens, "{big} vs {line_tokens}");
    eprintln!("big server: {big} tokens up front vs a {line_tokens}-token line");

    // Exactly one prefix change: the tool list once; the conversation only
    // grows.
    let changes = (1..reqs.len())
        .filter(|&i| {
            let (a, b) = (&reqs[i - 1], &reqs[i]);
            let (ma, mb) = (
                a["messages"].as_array().unwrap(),
                b["messages"].as_array().unwrap(),
            );
            a["tools"] != b["tools"] || mb.len() < ma.len() || mb[..ma.len()] != ma[..]
        })
        .count();
    assert_eq!(changes, 1);
}

/// `defer = false` keeps a server's tools up front whatever their size;
/// reading `docs` topic "tools" turns waiting servers on; a big window has
/// room for them all.
#[tokio::test]
async fn waiting_mcp_tools_have_switches() {
    let h = mcp_harness(vec![Reply::Text("ok".into())], 100_000, "defer = false").await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    assert_eq!(
        tool_names(&req)
            .iter()
            .filter(|n| n.starts_with("mcp__big__"))
            .count(),
        100
    );
    assert!(
        !req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("# MCP tools")
    );

    let h = mcp_harness(
        vec![
            Reply::Tools(vec![("docs".into(), json!({"topic": "tools"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert!(
        !tool_names(&reqs[0])
            .iter()
            .any(|n| n.starts_with("mcp__big__"))
    );
    assert_eq!(
        tool_names(&reqs[1])
            .iter()
            .filter(|n| n.starts_with("mcp__big__"))
            .count(),
        100
    );
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0].2.starts_with("The other tools are on"),
        "{results:?}"
    );

    let h = mcp_harness(vec![Reply::Text("ok".into())], 1_000_000, "").await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    assert_eq!(
        tool_names(&req)
            .iter()
            .filter(|n| n.starts_with("mcp__big__"))
            .count(),
        100
    );
}

async fn set_cwd(h: &Harness, id: &str) {
    h.engine
        .handle(
            tessera_proto::Request::UpdateSession {
                session_id: id.into(),
                title: None,
                model: None,
                thinking: None,
                permission: None,
                pinned: None,
                archived: None,
                cwd: Some(h.work.display().to_string()),
                members: None,
                fast: None,
                subagents: None,
            },
            None,
        )
        .await
        .unwrap();
}

fn big_tools(req: &Value) -> usize {
    tool_names(req)
        .iter()
        .filter(|n| n.starts_with("mcp__big__"))
        .count()
}

fn mcp_line(req: &Value) -> bool {
    req["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("# MCP tools")
}

/// A server turned on stays on when the prompt is built again (here: the
/// chat's folder set twice), not off again with another cache miss.
#[tokio::test]
async fn a_server_turned_on_stays_on_through_prompt_rebuilds() {
    let call = || Reply::Tools(vec![("mcp__big__tool_3".into(), json!({"q": "hello"}))]);
    let h = mcp_harness(
        vec![
            call(),
            call(),
            Reply::Text("done".into()),
            Reply::Text("again".into()),
            Reply::Text("and again".into()),
        ],
        100_000,
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "use it").await;
    h.settle(&s.id).await;
    for turn in ["second", "third"] {
        set_cwd(&h, &s.id).await;
        h.send(&s.id, turn).await;
        h.settle(&s.id).await;
    }
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 5);
    assert_eq!(big_tools(&reqs[0]), 0);
    for (i, r) in reqs.iter().enumerate().skip(1) {
        assert_eq!(big_tools(r), 100, "request {i}");
    }
}

/// The share is of the window the chat works in: a 1M-token model with
/// `[context] max_tokens = 100000` works in 100k, so the big server waits.
#[tokio::test]
async fn the_share_is_of_the_working_window() {
    let h = mcp_harness_with(
        vec![Reply::Text("ok".into())],
        1_000_000,
        "",
        "[context]\nmax_tokens = 100000\n",
        "0",
    )
    .await;
    wait_for_mcp(&h).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    assert_eq!(big_tools(&req), 0);
    assert!(mcp_line(&req));
}

/// A server that connects after a chat built its prompt (just after the
/// start) is taken into the decision at the chat's next step: its tools
/// wait, with the prompt's line, instead of all coming up front.
#[tokio::test]
async fn a_server_that_connects_late_is_decided_when_it_arrives() {
    let h = mcp_harness_with(
        vec![Reply::Text("ok".into()), Reply::Text("ok again".into())],
        100_000,
        "",
        "",
        "2",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    assert!(
        !h.engine.has_tool("mcp__big__tool_99"),
        "connected too soon"
    );
    wait_for_mcp(&h).await;
    h.send(&s.id, "and now").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 2);
    assert!(!mcp_line(&reqs[0]));
    assert_eq!(big_tools(&reqs[1]), 0);
    assert!(mcp_line(&reqs[1]));
    assert!(tool_names(&reqs[1]).contains(&"mcp__tiny__ping".to_string()));
}

/// A saved on-list also survives a rebuild while no MCP tools exist yet
/// (a restart with slow connections), followed by another rebuild at connect.
#[tokio::test]
async fn a_saved_on_list_survives_starting_before_servers_connect() {
    let call = || Reply::Tools(vec![("mcp__big__tool_3".into(), json!({"q":"hello"}))]);
    let mut h = mcp_harness(
        vec![
            call(),
            call(),
            Reply::Text("done".into()),
            Reply::Text("before connect".into()),
            Reply::Text("after connect".into()),
        ],
        100_000,
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "use it").await;
    h.settle(&s.id).await;
    h.engine.shutdown().await;
    let config_path = h.dir.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)
        .unwrap()
        .replace("\"big\", \"0\"", "\"big\", \"2\"");
    std::fs::write(config_path, config).unwrap();
    h.engine = Engine::open(Paths::at(h.dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    set_cwd(&h, &s.id).await;
    h.send(&s.id, "before connect").await;
    h.settle(&s.id).await;
    assert!(!h.engine.has_tool("mcp__big__tool_99"));
    wait_for_mcp(&h).await;
    h.send(&s.id, "after connect").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 5);
    assert_eq!(big_tools(&reqs[4]), 100);
    h.engine.shutdown().await;
}
