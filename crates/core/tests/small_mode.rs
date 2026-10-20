//! Model profiles and small model mode end to end, with the mock model on
//! this computer (as llama.cpp would serve it): the short prompt and its
//! budget, the core tools with flat arguments and the one switch to the
//! rest, worked examples, sampling per model card, one call per reply,
//! Mistral's call ids, plan reminders, the stricter loop limit and the
//! one-call tool check. Request snapshots prove what each mode sends.

mod common;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// A harness whose mock lists `model` the way llama.cpp does, with `extra`
/// config (`[tuning]` tables) after `[agent]`.
async fn small_harness(script: Vec<Reply>, model: &str, extra: &str) -> Harness {
    let mock = Mock::start(script).await;
    mock.state.models.lock().push(model.to_string());
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        r#"default_model = "mock/{model}"
[agent]
review_interval = 0
{extra}
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{url}"
discover = true
parallel = 4
[[provider.models]]
id = "{model}"
context_window = 100000
max_output = 2000
reasoning = true
"#,
        url = mock.url
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

/// No tool check (tests of everything else).
const NO_CHECK: &str = "[tuning.\"*\"]\ncheck = false\n";

/// No tool check, several calls per reply.
const PARALLEL: &str = "[tuning.\"*\"]\ncheck = false\nparallel_tool_calls = true\n";

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

fn tool<'a>(req: &'a Value, name: &str) -> &'a Value {
    req["tools"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["function"]["name"] == name))
        .map(|t| &t["function"])
        .unwrap_or(&Value::Null)
}

/// Estimated tokens of the system prompt plus tool specs (the engine's own
/// estimator, as `code.rs` measures the default mode).
fn prompt_tokens(req: &Value) -> u64 {
    let system = req["messages"][0]["content"].as_str().unwrap_or("");
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
    for s in &specs {
        eprintln!(
            "{:>5} {}",
            tessera_core::context::estimate_request("", std::slice::from_ref(s), &[]),
            s.name
        );
    }
    tessera_core::context::estimate_request(system, &specs, &[])
}

/// The messages before the conversation's first real user message.
fn example_messages(req: &Value) -> Vec<Value> {
    let msgs = req["messages"].as_array().unwrap();
    let real = msgs
        .iter()
        .position(|m| {
            m["role"] == "user" && !m["content"].to_string().contains("[Example of how tools")
        })
        .unwrap();
    msgs[1..real].to_vec()
}

/// Small model mode on a model of the user's own server (Auto): a short
/// prompt within its budget, the core tools only, flat `edit` and `task`,
/// worked examples first, one call per reply, the model card's sampling.
#[tokio::test]
async fn small_mode_prompt_tools_and_requests() {
    let h = small_harness(vec![Reply::Text("Hello!".into())], "qwen3.8-27b", NO_CHECK).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let total = prompt_tokens(&req);
    eprintln!("{total:>5} total, small model mode (system prompt + tools)");
    assert!(
        (1_500..=2_500).contains(&total),
        "small prompt is {total} estimated tokens (budget 2,500)"
    );
    assert_eq!(
        tool_names(&req),
        [
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
            "whiteboard",
            "docs"
        ]
    );
    let edit = tool(&req, "edit");
    assert!(edit["parameters"]["properties"]["old"].is_object());
    assert!(edit["parameters"]["properties"]["at"].is_object());
    assert!(edit["parameters"]["properties"].get("replace").is_none());
    assert!(tool(&req, "task")["parameters"]["properties"]["done"].is_object());
    assert_eq!(
        tool(&req, "whiteboard")["parameters"]["properties"]["action"]["enum"],
        json!(["ask"])
    );
    let system = req["messages"][0]["content"].as_str().unwrap();
    for rule in [
        "Answer directly when no tool is needed",
        "Read a file before you edit it",
        "read several at once with `paths`",
        "One step at a time",
        "done only after you checked",
        "user's language",
        "topic \"tools\"",
    ] {
        assert!(system.contains(rule), "missing `{rule}`:\n{system}");
    }
    assert!(!system.contains("delegate to subagents"));
    // Worked examples open the conversation, then the user's message.
    let examples = example_messages(&req);
    assert_eq!(examples.len(), 12, "{examples:#?}");
    assert!(
        examples
            .iter()
            .any(|m| m["tool_calls"][0]["function"]["name"] == "edit")
    );
    // `read` takes several files at once.
    assert!(tool(&req, "read")["parameters"]["properties"]["paths"].is_object());
    // One call per reply (several made it get written specs wrong, batch
    // 15), and Qwen3.8's thinking-mode values (the mock lists it as
    // llama.cpp: top_k and min_p go too).
    assert_eq!(req["parallel_tool_calls"], false);
    assert_eq!(req["temperature"], 1.0);
    assert_eq!(req["top_p"], 0.95);
    assert_eq!(req["top_k"], 20);
    assert_eq!(req["min_p"], 0.0);
    assert_eq!(req["presence_penalty"], 0.0);
    // The model list says so.
    let providers: Vec<ProviderInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListProviders { refresh: false }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let m = providers
        .iter()
        .flat_map(|p| &p.models)
        .find(|m| m.id == "qwen3.8-27b")
        .unwrap();
    assert_eq!((m.small_mode, m.small), (SmallMode::Auto, true));
}

/// Off: the full prompt and tools, as before, with the card's sampling
/// still sent (its own switch), and the user's values over it.
#[tokio::test]
async fn small_mode_off_and_sampling_switches() {
    let h = small_harness(
        vec![Reply::Text("ok".into()), Reply::Text("ok".into())],
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\n[tuning.\"mock/qwen3.8-27b\"]\nsmall = \"off\"\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    assert!(tool_names(&req).iter().any(|t| t == "browser"));
    assert!(
        req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("# How to work\n- Be direct.")
    );
    assert_eq!(req["parallel_tool_calls"], true);
    assert!(example_messages(&req).is_empty());
    assert_eq!(req["temperature"], 1.0);

    // Sampling off, one value of the user's own, one call per reply.
    let h = small_harness(
        vec![Reply::Text("ok".into())],
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\nsampling = false\ntemperature = 0.3\nparallel_tool_calls = false\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    assert_eq!(req["temperature"], 0.3);
    for k in ["top_p", "top_k", "min_p", "presence_penalty"] {
        assert!(req.get(k).is_none(), "{k} sent");
    }
    assert_eq!(req["parallel_tool_calls"], false);
    assert_eq!(tool_names(&req).len(), 13, "still small");
}

/// An unknown model gets no sampling at all (the server's own defaults),
/// and a Mistral-family model's call ids reach it as 9 letters and digits.
#[tokio::test]
async fn unknown_models_get_no_sampling_and_mistral_ids_fit() {
    let h = small_harness(
        vec![
            Reply::Tools(vec![("glob".into(), json!({"pattern": "*"}))]),
            Reply::Text("done".into()),
        ],
        "devstral-small-2507",
        NO_CHECK,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "list files").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs[0]["temperature"], 0.15);
    assert!(reqs[0].get("top_p").is_none());
    let msgs = reqs[1]["messages"].as_array().unwrap();
    let ids: Vec<&str> = msgs
        .iter()
        .flat_map(|m| {
            m["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["id"].as_str())
                .chain(m["tool_call_id"].as_str())
        })
        .collect();
    assert!(ids.len() >= 2, "{ids:?}");
    for id in ids {
        assert!(
            id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric()),
            "{id}"
        );
    }

    let h = small_harness(vec![Reply::Text("ok".into())], "my-finetune", NO_CHECK).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    for k in ["temperature", "top_p", "top_k", "min_p", "presence_penalty"] {
        assert!(req.get(k).is_none(), "{k} sent for an unknown model");
    }
    // Still small: it runs on the user's own server.
    assert_eq!(tool_names(&req).len(), 13);
}

/// The flat forms run as the tools' own shapes, and the history keeps them
/// as the model sent them (matching the tools it is offered).
#[tokio::test]
async fn flat_edit_and_task_calls_work() {
    let h = small_harness(vec![], "qwen3.8-27b", NO_CHECK).await;
    let path = h.work.join("config.py");
    std::fs::write(&path, "HOST = \"localhost\"\nPORT = 8080\nDEBUG = False\n").unwrap();
    let tag = tessera_core::tools::files::file_tag(&std::fs::read_to_string(&path).unwrap());
    for r in [
        Reply::Tools(vec![(
            "task".into(),
            json!({"goal": "Fix the config", "plan": ["Change the port", "Turn debug on"]}),
        )]),
        Reply::Tools(vec![("read".into(), json!({"path": "config.py"}))]),
        Reply::Tools(vec![(
            "edit".into(),
            json!({"path": "config.py", "old": "PORT = 8080", "new": "PORT = 9090"}),
        )]),
        Reply::Tools(vec![(
            "edit".into(),
            json!({"path": "config.py", "tag": tag, "at": "3", "text": "DEBUG = True"}),
        )]),
        Reply::Tools(vec![("task".into(), json!({"done": [1, 2]}))]),
        Reply::Text("Done.".into()),
    ] {
        h.mock.push(r);
    }
    let s = h.session().await;
    h.send(&s.id, "fix the config").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    for (tool, err, text) in &results {
        assert!(!err, "{tool} failed: {text}");
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "HOST = \"localhost\"\nPORT = 9090\nDEBUG = True\n"
    );
    let info = h.info(&s.id).await;
    let plan = &info.task.unwrap().plan;
    assert!(
        plan.iter().all(|p| p.status == PlanStatus::Done),
        "{plan:?}"
    );
    // Stored as sent.
    let entries = h.entries(&s.id).await;
    let edits: Vec<&Value> = entries
        .iter()
        .flat_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::ToolCall {
                        name, arguments, ..
                    } if name == "edit" => Some(arguments),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect();
    assert_eq!(edits[0]["old"], "PORT = 8080");
    assert!(edits[0].get("replace").is_none());
}

/// One switch: `docs` topic `tools` turns the other tools on from the next
/// step, once (the tool list changes once, so the prompt cache misses once).
#[tokio::test]
async fn the_other_tools_are_one_switch_away() {
    let h = small_harness(
        vec![
            Reply::Tools(vec![("docs".into(), json!({"topic": "tools"}))]),
            Reply::Tools(vec![("docs".into(), json!({"topic": "tools"}))]),
            Reply::Text("ok".into()),
        ],
        "qwen3.8-27b",
        NO_CHECK,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "open a web page for me").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(tool_names(&reqs[0]).len(), 13);
    let more = tool_names(&reqs[1]);
    for t in ["browser", "note", "schedule", "notify", "recall", "skill"] {
        assert!(more.iter().any(|n| n == t), "{t} not on: {more:?}");
    }
    assert_eq!(tool_names(&reqs[2]), more, "turned on once");
    // Flat forms stay, the whole whiteboard comes.
    assert!(tool(&reqs[1], "edit")["parameters"]["properties"]["old"].is_object());
    assert!(tool(&reqs[1], "whiteboard")["parameters"]["properties"]["html"].is_object());
    // The prompt and the examples didn't change (only the tools did).
    assert_eq!(reqs[0]["messages"][0], reqs[1]["messages"][0]);
    assert_eq!(example_messages(&reqs[0]), example_messages(&reqs[1]));
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results[0]
            .2
            .starts_with("The other tools are on from your next step")
    );
    assert!(results[1].2.starts_with("The other tools are already on"));
}

/// Calling one of the tools kept off turns them all on (and says so).
#[tokio::test]
async fn calling_a_tool_kept_off_turns_them_on() {
    let h = small_harness(
        vec![
            Reply::Tools(vec![("note".into(), json!({"action": "list"}))]),
            Reply::Text("ok".into()),
        ],
        "qwen3.8-27b",
        NO_CHECK,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "list my notes").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(results[0].1, "an error first");
    assert!(results[0].2.contains("`note` was off"), "{}", results[0].2);
    let reqs = h.mock.agent_requests();
    assert!(tool_names(&reqs[1]).iter().any(|t| t == "note"));
}

/// The plan's next step every few steps, at the end of a result (never in
/// the prompt's start), and loops refused after 3 identical failures.
#[tokio::test]
async fn plan_reminders_and_the_loop_limit() {
    let h = small_harness(vec![], "qwen3.8-27b", NO_CHECK).await;
    h.mock.push(Reply::Tools(vec![(
        "task".into(),
        json!({"plan": ["Look around", "Write the report"]}),
    )]));
    for _ in 0..5 {
        h.mock.push(Reply::Tools(vec![(
            "read".into(),
            json!({"path": "missing.txt"}),
        )]));
    }
    h.mock
        .push(Reply::Tools(vec![("glob".into(), json!({"pattern": "*"}))]));
    h.mock.push(Reply::Text("Stopping.".into()));
    let s = h.session().await;
    h.send(&s.id, "look around").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    // Reads 1–3 fail as reads, 4–5 are refused.
    let reads: Vec<&(String, bool, String)> = results.iter().filter(|r| r.0 == "read").collect();
    assert!(!reads[2].2.starts_with("Blocked"), "{}", reads[2].2);
    assert!(reads[3].2.starts_with("Blocked"), "{}", reads[3].2);
    // Step 6 (the glob) carries the plan.
    let glob = results.iter().find(|r| r.0 == "glob").unwrap();
    assert!(
        glob.2.ends_with("[Plan: step 1/2 — Look around]"),
        "{}",
        glob.2
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r.2.contains("[Plan: step"))
            .count(),
        1
    );
}

/// The one-call tool check: once per model, a model that answers in text
/// is noted, the chat says so, and Auto turns small model mode on for it.
#[tokio::test]
async fn the_tool_check_runs_once_and_says_so_plainly() {
    // A model the user's server runs that is known to be large: Auto leaves
    // small model mode off, until the check fails.
    let h = small_harness(
        vec![
            Reply::Text("It is about noon.".into()),
            Reply::Text("Hi!".into()),
            Reply::Text("Hi again!".into()),
        ],
        "llama-3.3-70b-instruct",
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 2, "the check, then the chat");
    let check = &reqs[0];
    assert_eq!(tool_names(check), ["get_time"]);
    assert_eq!(
        check["messages"][1]["content"],
        "What time is it? Use the tool."
    );
    assert!(check.to_string().len() < 3_600, "under 1k tokens");
    let entries = h.entries(&s.id).await;
    let notice = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(notice.contains("didn't use a tool"), "{notice}");
    assert!(notice.contains("Small model mode is now on"), "{notice}");
    // This chat's prompt was built after the check: small already.
    assert_eq!(tool_names(&reqs[1]).len(), 13);
    // Never again for that model.
    let s2 = h.session().await;
    h.send(&s2.id, "hello").await;
    h.settle(&s2.id).await;
    assert_eq!(h.mock.agent_requests().len(), 3);

    // A model that calls the tool passes quietly.
    let h = small_harness(
        vec![
            Reply::Tools(vec![("get_time".into(), json!({}))]),
            Reply::Text("Hi!".into()),
        ],
        "qwen3.8-27b",
        "",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    assert_eq!(h.mock.agent_requests().len(), 2);
    assert!(
        !h.entries(&s.id)
            .await
            .iter()
            .any(|e| matches!(e.body, EntryBody::Notice { .. }))
    );
}

/// The per-model setting: saved under `[tuning]`, listed, back to Auto.
#[tokio::test]
async fn the_setting_is_saved_and_listed() {
    let h = small_harness(vec![], "qwen3.8-27b", NO_CHECK).await;
    let set = |mode| Request::SetModelSmall {
        provider: "mock".into(),
        model: "qwen3.8-27b".into(),
        mode,
    };
    h.engine.handle(set(SmallMode::Off), None).await.unwrap();
    let toml = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(toml.contains("[tuning.\"mock/qwen3.8-27b\"]"), "{toml}");
    assert!(toml.contains("small = \"off\""), "{toml}");
    let listed = |h: &Harness| {
        let h = h.engine.clone();
        async move {
            let providers: Vec<ProviderInfo> = serde_json::from_value(
                h.handle(Request::ListProviders { refresh: false }, None)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let m = providers
                .into_iter()
                .flat_map(|p| p.models)
                .find(|m| m.id == "qwen3.8-27b")
                .unwrap();
            (m.small_mode, m.small)
        }
    };
    assert_eq!(listed(&h).await, (SmallMode::Off, false));
    // A new chat starts with the full tools.
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    assert!(tool_names(&h.mock.agent_requests()[0]).len() > 13);
    h.engine.handle(set(SmallMode::Auto), None).await.unwrap();
    assert_eq!(listed(&h).await, (SmallMode::Auto, true));
    let toml = std::fs::read_to_string(h.dir.path().join("config.toml")).unwrap();
    assert!(!toml.contains("[tuning.\"mock/qwen3.8-27b\"]"), "{toml}");
    assert!(
        h.engine
            .handle(
                Request::SetModelSmall {
                    provider: "".into(),
                    model: "x".into(),
                    mode: SmallMode::On
                },
                None
            )
            .await
            .is_err()
    );
}

/// One call per reply in small model mode, whatever the size;
/// `[tuning] parallel_tool_calls` decides over it.
#[tokio::test]
async fn small_mode_makes_one_call_per_reply_unless_set() {
    for (model, extra, parallel) in [
        ("qwen3-4b", NO_CHECK, false),
        ("Qwen3-30B-A3B", NO_CHECK, false),
        ("qwen3.8-27b", NO_CHECK, false),
        (
            "qwen3-4b",
            "[tuning.\"*\"]\ncheck = false\nparallel_tool_calls = true\n",
            true,
        ),
    ] {
        let h = small_harness(vec![Reply::Text("ok".into())], model, extra).await;
        let s = h.session().await;
        h.send(&s.id, "hi").await;
        h.settle(&s.id).await;
        let req = h.mock.agent_requests()[0].clone();
        assert_eq!(tool_names(&req).len(), 13, "{model}: small");
        assert_eq!(req["parallel_tool_calls"], parallel, "{model} {extra}");
    }
}

/// Several calls in one small-mode reply (turned on): a call repeated in it runs once,
/// and after a change that failed, the reply's other changes and commands
/// are held back while its reads still run. `reply_guard = false` runs all.
#[tokio::test]
async fn the_reply_guard_holds_back_what_follows_a_failed_change() {
    let reply = || {
        Reply::Tools(vec![
            ("read".into(), json!({"path": "a.py"})),
            ("read".into(), json!({"path": "a.py"})),
            (
                "edit".into(),
                json!({"path": "a.py", "old": "y = 2", "new": "y = 3"}),
            ),
            ("bash".into(), json!({"command": "touch ran.txt"})),
            ("read".into(), json!({"paths": ["a.py"]})),
        ])
    };
    let h = small_harness(
        vec![reply(), Reply::Text("ok".into())],
        "qwen3.8-27b",
        PARALLEL,
    )
    .await;
    std::fs::write(h.work.join("a.py"), "x = 1\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "change y").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let errs: Vec<bool> = results.iter().map(|r| r.1).collect();
    assert_eq!(errs, [false, true, true, true, false], "{results:#?}");
    assert!(
        results[1].2.contains("same call is earlier"),
        "{}",
        results[1].2
    );
    assert!(
        results[3]
            .2
            .contains("`edit` before it in this reply failed"),
        "{}",
        results[3].2
    );
    assert!(results[4].2.contains("1:x = 1"), "{}", results[4].2);
    assert!(!h.work.join("ran.txt").exists(), "the command ran");
    // Held-back calls aren't failures of their own: no loop note.
    assert!(
        results.iter().all(|r| !r.2.contains("[Note:")),
        "{results:#?}"
    );

    let h = small_harness(
        vec![reply(), Reply::Text("ok".into())],
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\nparallel_tool_calls = true\nreply_guard = false\n",
    )
    .await;
    std::fs::write(h.work.join("a.py"), "x = 1\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "change y").await;
    h.settle(&s.id).await;
    let errs: Vec<bool> = tool_results(&h.entries(&s.id).await)
        .iter()
        .map(|r| r.1)
        .collect();
    assert_eq!(errs, [false, false, true, false, false]);
    assert!(h.work.join("ran.txt").exists());
}

/// `[tuning] read_paths = false`: `read` takes one file, and the short
/// guidelines and the worked example are batch 14's again.
#[tokio::test]
async fn reading_several_files_at_once_has_a_switch() {
    let h = small_harness(
        vec![Reply::Text("ok".into())],
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\nread_paths = false\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let read = tool(&req, "read");
    assert!(read["parameters"]["properties"].get("paths").is_none());
    assert_eq!(read["parameters"]["required"], json!(["path"]));
    assert!(
        read["description"]
            .as_str()
            .unwrap()
            .starts_with("Read a text file as")
    );
    let system = req["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("Read a file before you edit it. Edit with"),
        "{system}"
    );
    // Batch 14's words and worked example, as they were.
    assert!(system.contains("- One step at a time: check each result before the next step."));
    let examples = example_messages(&req);
    let reads: Vec<String> = examples
        .iter()
        .filter(|m| m["tool_calls"][0]["function"]["name"] == "read")
        .map(|m| m["tool_calls"][0]["function"]["arguments"].to_string())
        .collect();
    assert_eq!(reads.len(), 1);
    assert!(
        reads[0].contains("config.py") && !reads[0].contains("paths"),
        "{reads:?}"
    );
}

/// Past the observation budget, old tool output is replaced with
/// references in one batch (the newest stays), once it frees enough; the
/// requests after it start from the trimmed history. 0 turns it off.
#[tokio::test]
async fn old_tool_output_is_trimmed_in_batches_past_the_observation_budget() {
    let script = || {
        let mut s: Vec<Reply> = (0..8)
            .map(|i| {
                Reply::Tools(vec![(
                    "bash".into(),
                    json!({"command": format!("seq {} {}", i * 10_000 + 1, i * 10_000 + 1_500)}),
                )])
            })
            .collect();
        s.push(Reply::Text("done".into()));
        s
    };
    let pruned = |req: &Value| {
        req["messages"]
            .to_string()
            .contains("output pruned from context")
    };
    let h = small_harness(
        script(),
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\nobservation_budget = 12000\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "count").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 9);
    let first = reqs.iter().position(pruned).expect("never trimmed");
    assert!(
        first >= 3,
        "trimmed before it freed enough: request {first}"
    );
    // The newest output is never the one trimmed.
    let last = reqs[first]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    assert!(!last.contains("pruned"), "{last}");
    let entries = h.entries(&s.id).await;
    let trims = entries
        .iter()
        .filter(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text.starts_with("Context trimmed")))
        .count();
    // In batches: far fewer trims than requests over the budget.
    eprintln!("first trimmed request {first}, {trims} trims");
    assert!((1..=2).contains(&trims), "{trims} trims");
    assert!(tool_results(&entries).iter().all(|r| !r.1));

    let h = small_harness(
        script(),
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\nobservation_budget = 0\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "count").await;
    h.settle(&s.id).await;
    assert!(!h.mock.agent_requests().iter().any(pruned));
}

/// Batched trimming keeps the prompt cache: between batches every request
/// starts with the previous one byte for byte (append-only), so the
/// history changes exactly once per batch; the last three replies keep
/// their reasoning and their tool output.
#[tokio::test]
async fn batched_trimming_changes_the_prefix_once_per_batch() {
    let mut script: Vec<Reply> = (0..10)
        .map(|i| {
            Reply::ThinkTools(
                format!("Step {i}: count the next block."),
                vec![(
                    "bash".into(),
                    json!({"command": format!("seq {} {}", i * 10_000 + 1, i * 10_000 + 1_500)}),
                )],
            )
        })
        .collect();
    script.push(Reply::Text("done".into()));
    let h = small_harness(
        script,
        "qwen3.8-27b",
        "[tuning.\"*\"]\ncheck = false\nobservation_budget = 12000\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "count").await;
    h.settle(&s.id).await;
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 11);
    let msgs = |r: &Value| r["messages"].as_array().unwrap().clone();
    // Where a request doesn't start with the one before it.
    let changed: Vec<usize> = (1..reqs.len())
        .filter(|&i| {
            let (a, b) = (msgs(&reqs[i - 1]), msgs(&reqs[i]));
            b.len() < a.len() || b[..a.len()] != a[..]
        })
        .collect();
    let trims = h
        .entries(&s.id)
        .await
        .iter()
        .filter(|e| matches!(&e.body, EntryBody::Notice { text, .. } if text.starts_with("Context trimmed")))
        .count();
    eprintln!("prefix changed at requests {changed:?}; {trims} trims");
    assert!(trims >= 1, "never trimmed");
    assert_eq!(changed.len(), trims, "one prefix change per batch");
    // Right after a batch: the last three replies keep their reasoning and
    // their output.
    for &i in &changed {
        let m = msgs(&reqs[i]);
        let replies: Vec<&Value> = m.iter().filter(|m| m["role"] == "assistant").collect();
        for r in &replies[replies.len() - 3..] {
            assert!(r.get("reasoning_content").is_some(), "request {i}: {r}");
        }
        let outputs: Vec<&Value> = m.iter().filter(|m| m["role"] == "tool").collect();
        for o in &outputs[outputs.len() - 3..] {
            assert!(!o.to_string().contains("pruned"), "request {i}: {o}");
        }
        assert!(
            outputs.iter().any(|o| o.to_string().contains("pruned")),
            "request {i}: older output was trimmed"
        );
    }
}

/// The guidelines ask for plan updates in the same reply as other calls
/// (GPT spent a fifth of its replies on the ledger alone);
/// `plan_alongside = false` leaves that out.
#[tokio::test]
async fn plan_updates_go_with_other_calls_unless_switched_off() {
    for (extra, hint) in [("", true), ("plan_alongside = false\n", false)] {
        let h = small_harness(
            vec![Reply::Text("ok".into())],
            "qwen3.8-27b",
            &format!("[tuning.\"*\"]\ncheck = false\nsmall = \"off\"\n{extra}"),
        )
        .await;
        let s = h.session().await;
        h.send(&s.id, "hi").await;
        h.settle(&s.id).await;
        let req = h.mock.agent_requests()[0].clone();
        let system = req["messages"][0]["content"].as_str().unwrap();
        assert_eq!(
            system.contains("keep plan statuses current, alongside your other calls. The ledger"),
            hint,
            "{system}"
        );
        assert!(system.contains("keep plan statuses current"));
    }
}

/// An `edit` that arrives with only `path` (the server dropped argument
/// names the small form doesn't list), or with text but no place for it,
/// is answered in the small form's words with the call that would work,
/// not the full form's (which the model then sent again until the loop
/// guard stopped it). A six-step run: both answered, then the suggested
/// forms work, and nothing is blocked.
#[tokio::test]
async fn an_empty_edit_is_answered_in_the_small_forms_words() {
    let after = "x = 2\n";
    let tag = tessera_core::tools::files::file_tag(after);
    let h = small_harness(
        vec![
            Reply::Tools(vec![("edit".into(), json!({"path": "a.py"}))]),
            Reply::Tools(vec![(
                "edit".into(),
                json!({"path": "a.py", "text": "x = 2"}),
            )]),
            Reply::Tools(vec![(
                "edit".into(),
                json!({"path": "a.py", "old": "x = 1", "new": "x = 2"}),
            )]),
            Reply::Tools(vec![("read".into(), json!({"path": "a.py"}))]),
            Reply::Tools(vec![(
                "edit".into(),
                json!({"path": "a.py", "tag": tag, "at": ">$", "text": "y = 3"}),
            )]),
            Reply::Text("ok".into()),
        ],
        "qwen3.8-27b",
        NO_CHECK,
    )
    .await;
    std::fs::write(h.work.join("a.py"), "x = 1\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "change x").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let errs: Vec<bool> = results.iter().map(|r| r.1).collect();
    assert_eq!(errs, [true, true, false, false, false], "{results:#?}");
    let (bare, text) = (&results[0].2, &results[1].2);
    assert!(
        bare.starts_with(
            "This `edit` arrived with nothing to change. Send {\"path\": \"a.py\", \"old\""
        ),
        "{bare}"
    );
    assert!(
        text.starts_with("This `edit` has text but nothing that says where it goes.")
            && text.contains("\"at\": \"12-15\"")
            && text.contains("use `write`"),
        "{text}"
    );
    for r in &results {
        assert!(!r.2.contains("`lines` or `replace`"), "{r:?}");
        assert!(!r.2.contains("Blocked"), "{r:?}");
    }
    assert_eq!(
        std::fs::read_to_string(h.work.join("a.py")).unwrap(),
        "x = 2\ny = 3\n"
    );
}
