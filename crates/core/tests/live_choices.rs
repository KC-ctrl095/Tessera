//! Live check of what agents reach for (ignored by default): do they open web
//! view tiles only when asked, and do they leave skills to real procedures?
//! The engine's real system prompt and tool specs go to a real model; tools
//! answer with canned text, so nothing is browsed, shown or saved. The
//! `review-*` scenarios give the memory review's own prompt (and, tuned, its
//! tool offer and the checks on a new skill) the scripted chats of
//! `common/skill_cases.rs`.
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_choices -- --ignored --nocapture
//! `PROBE_RUNS=2` repeats each scenario, `PROBE_CASES=a,b` picks some,
//! `PROBE_REPORT_ONLY=1` skips the asserts, `PROBE_LEGACY=1` swaps the
//! tile and skill wording back to what it was before it was tuned.

mod common;

use common::skill_cases::cases as skill_cases;
use common::*;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// No tile: the user didn't ask for one.
    Chat,
    /// A tile: the user asked.
    Tile,
    /// No new skill: nothing here is worth one.
    NoSkill,
    /// A skill would be fine (reported, not asserted).
    MaySkill,
    /// A skill is wanted (a review of a chat that worked one out).
    Skill,
}

struct Scenario {
    name: &'static str,
    /// Messages after the system prompt.
    history: Vec<Value>,
    /// What the information tools answer.
    info: &'static str,
    expect: Expect,
    /// Only these tools are offered (the review's).
    tools: Option<Vec<&'static str>>,
    /// A new skill is checked as Tessera checks it: tool calls in the chat,
    /// and whether the user asked for a skill.
    guard: Option<(usize, bool)>,
}

fn user(t: &str) -> Value {
    json!({"role": "user", "content": t})
}
fn say(t: &str) -> Value {
    json!({"role": "assistant", "content": t})
}
/// An agent tool call and its result, as the model saw them.
fn step(n: usize, tool: &str, args: Value, result: &str) -> Vec<Value> {
    vec![
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": format!("call_{n}"), "type": "function",
            "function": {"name": tool, "arguments": args.to_string()}}]}),
        json!({"role": "tool", "tool_call_id": format!("call_{n}"), "content": result}),
    ]
}
fn bash(n: usize, cmd: &str, out: &str) -> Vec<Value> {
    step(n, "bash", json!({"command": cmd}), out)
}

fn ask(name: &'static str, text: &'static str, info: &'static str, expect: Expect) -> Scenario {
    Scenario {
        name,
        history: vec![user(text)],
        info,
        expect,
        tools: None,
        guard: None,
    }
}

fn scenarios(legacy: bool) -> Vec<Scenario> {
    let hn = "Hacker News front page:\n1. Show HN: A tiny self-hosted RSS reader (412 points)\n2. SQLite 4 design notes (388 points)\n3. Why we left Kubernetes for two VMs (301 points)\n4. Ask HN: What are you working on? (250 points)";
    let boards = "Raspberry Pi 5: Cortex-A76 x4, PCIe 2.0 x1 (via FFC connector), up to 8 GB, gigabit Ethernet, USB 3.0 x2. Orange Pi 5: RK3588S octa-core, M.2 NVMe slot (PCIe 2.0 x1 on some boards), up to 32 GB, gigabit Ethernet, USB 3.0 x1.";
    let hours = "Lincoln Memorial, Washington DC: open 24 hours a day, every day of the year. Rangers are on duty from 9:30 am to 10 pm daily.";
    let weather =
        "Chicago, IL: 11 C, partly cloudy, wind 14 km/h NW, humidity 62%. Tonight: 6 C, clear.";
    let caddy = "Install Caddy on Debian/Ubuntu:\nsudo apt install -y debian-keyring debian-archive-keyring apt-transport-https curl\ncurl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg\ncurl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list\nsudo apt update\nsudo apt install caddy";
    let example = "Example Domain\nThis domain is for use in illustrative examples in documents. You may use this domain in literature without prior coordination or asking for permission.\nMore information...";
    let lasagna = "Classic lasagna: brown 500 g beef with onion and garlic, add 800 g crushed tomatoes and simmer 30 min. Layer sheets, sauce, ricotta and mozzarella three times. Bake 45 min at 180 C, rest 15 min.";
    let app = "My App\nWelcome back, Ken\nProjects (3)  Settings  Sign out";
    let login = "GitHub sign in: Username or email address, Password, Sign in button.";

    let mut list = vec![
        ask(
            "hn-top",
            "What are the top 3 stories on Hacker News right now?",
            hn,
            Expect::Chat,
        ),
        ask(
            "compare-boards",
            "Compare the Raspberry Pi 5 and the Orange Pi 5 for running a home NAS. A table would be nice.",
            boards,
            Expect::Chat,
        ),
        ask(
            "opening-hours",
            "Look up the opening hours of the Lincoln Memorial and tell me whether it is open on Sundays.",
            hours,
            Expect::Chat,
        ),
        ask(
            "weather",
            "What is the weather in Chicago right now?",
            weather,
            Expect::Chat,
        ),
        ask(
            "install-docs",
            "Find the install instructions for Caddy on Debian and give me the commands.",
            caddy,
            Expect::Chat,
        ),
        ask(
            "explain-btree",
            "Explain how a B-tree works and why databases use it.",
            "",
            Expect::Chat,
        ),
        ask(
            "page-contents",
            "What does https://example.com say?",
            example,
            Expect::Chat,
        ),
        ask(
            "recipe",
            "Find me a good lasagna recipe and summarize it.",
            lasagna,
            Expect::Chat,
        ),
        ask(
            "checklist",
            "Make me a packing checklist for a 3-day hiking trip.",
            "",
            Expect::Chat,
        ),
        ask(
            "site-check",
            "Is https://example.com up, and what is its title?",
            example,
            Expect::Chat,
        ),
        ask(
            "own-app-check",
            "My app runs on localhost:5173. Check that the homepage loads and tell me what you see.",
            app,
            Expect::Chat,
        ),
        ask(
            "asked-web-view",
            "Open https://example.com in the web view so I can look at it.",
            example,
            Expect::Tile,
        ),
        ask(
            "asked-whiteboard",
            "Put a diagram of TCP's three-way handshake on a whiteboard for me.",
            "",
            Expect::Tile,
        ),
        ask(
            "asked-login",
            "I want to sign in to my GitHub account in your browser so you can check my notifications afterwards. Open the login page for me.",
            login,
            Expect::Tile,
        ),
        ask(
            "asked-local-app",
            "Show my app from localhost:5173 in a tile beside the chat.",
            app,
            Expect::Tile,
        ),
    ];

    // The moment after a task is done: does the agent save a skill?
    let mut h = vec![user("Rename notes.txt to meeting-notes.txt.")];
    h.extend(bash(
        1,
        "mv notes.txt meeting-notes.txt && ls",
        "meeting-notes.txt",
    ));
    h.push(say("Renamed notes.txt to meeting-notes.txt."));
    h.push(user("Thanks!"));
    list.push(Scenario {
        name: "skill-rename",
        history: h,
        info: "ok",
        expect: Expect::NoSkill,
        tools: None,
        guard: None,
    });

    let mut h = vec![user(
        "Write a Python script squares.py that prints the first 10 squares, and run it.",
    )];
    h.extend(bash(
        1,
        "printf 'for i in range(1, 11):\\n    print(i * i)\\n' > squares.py",
        "",
    ));
    h.extend(bash(
        2,
        "python3 squares.py",
        "1\n4\n9\n16\n25\n36\n49\n64\n81\n100",
    ));
    h.push(say(
        "squares.py prints 1, 4, 9 … 100. I ran it and it works.",
    ));
    h.push(user("Great."));
    list.push(Scenario {
        name: "skill-script",
        history: h,
        info: "ok",
        expect: Expect::NoSkill,
        tools: None,
        guard: None,
    });

    let mut h = vec![user(
        "Set up a virtualenv for this project, install requirements.txt and run the tests.",
    )];
    h.extend(bash(1, "python3 -m venv .venv", ""));
    h.extend(bash(
        2,
        ".venv/bin/pip install -r requirements.txt",
        "Successfully installed flask-3.0.3 pytest-8.2.0",
    ));
    h.extend(bash(3, ".venv/bin/pytest -q", "......\n6 passed in 0.41s"));
    h.push(say(
        "Done: .venv created, requirements installed, 6 tests pass.",
    ));
    h.push(user("Perfect, thanks."));
    list.push(Scenario {
        name: "skill-venv",
        history: h,
        info: "ok",
        expect: Expect::NoSkill,
        tools: None,
        guard: None,
    });

    let mut h = vec![user(
        "Convert all the .heic photos in ~/Pictures/trip to jpg.",
    )];
    h.extend(bash(1, "which heif-convert convert", "/usr/bin/convert"));
    h.extend(bash(
        2,
        "convert ~/Pictures/trip/IMG_001.heic /tmp/x.jpg",
        "convert: no decode delegate for this image format `HEIC'",
    ));
    h.extend(bash(
        3,
        "sudo apt install -y libheif-examples",
        "Setting up libheif-examples",
    ));
    h.extend(bash(
        4,
        "cd ~/Pictures/trip && for f in *.heic; do heif-convert \"$f\" \"${f%.heic}.jpg\"; done",
        "Written to IMG_001.jpg\nWritten to IMG_002.jpg\nWritten to IMG_003.jpg",
    ));
    h.push(say("Converted the 3 HEIC photos to JPG with heif-convert (ImageMagick lacked the HEIC decoder, so I installed libheif-examples)."));
    h.push(user("Nice."));
    list.push(Scenario {
        name: "skill-heic",
        history: h,
        info: "ok",
        expect: Expect::NoSkill,
        tools: None,
        guard: None,
    });

    // A hard-won, repeatable procedure.
    let mut h = vec![user("Deploy the blog to my homelab.")];
    h.extend(bash(
        1,
        "hugo --minify",
        "Wrote public/ (baseURL http://localhost:1313/)",
    ));
    h.extend(bash(
        2,
        "rsync -a --delete public homelab:/srv/blog/",
        "created /srv/blog/public/ (one folder too deep)",
    ));
    h.push(say("Deployed to /srv/blog/public."));
    h.push(user("No. The site is served from /srv/blog itself and links point at localhost. Build with --baseURL https://blog.ken.lan/ and never delete .well-known (my certificates are in there). Reload caddy afterwards and check it answers 200."));
    h.extend(bash(
        3,
        "hugo --minify --baseURL https://blog.ken.lan/",
        "Wrote public/ (baseURL https://blog.ken.lan/)",
    ));
    h.extend(bash(
        4,
        "rsync -a --delete --exclude .well-known public/ homelab:/srv/blog/",
        "14 files updated",
    ));
    h.extend(bash(5, "ssh homelab 'sudo systemctl reload caddy'", ""));
    h.extend(bash(
        6,
        "curl -sI https://blog.ken.lan | head -1",
        "HTTP/2 200",
    ));
    h.push(say("Done: https://blog.ken.lan/ answers 200. Procedure: hugo --minify --baseURL https://blog.ken.lan/, rsync -a --delete --exclude .well-known public/ homelab:/srv/blog/, reload caddy, curl -sI."));
    h.push(user(
        "Perfect, that is exactly how I want it deployed every time.",
    ));
    list.push(Scenario {
        name: "skill-deploy",
        history: h,
        info: "ok",
        expect: Expect::MaySkill,
        tools: None,
        guard: None,
    });

    // The memory review itself, on the scripted chats.
    for case in skill_cases() {
        let asked = case.asked();
        let work = case.work();
        let may_skill = asked || work >= tessera_core::skills::MIN_WORK;
        let prompt = if legacy {
            legacy_review_prompt(&case.transcript())
        } else {
            tessera_core::memory::review_prompt(
                "",
                "",
                may_skill.then_some("(none)"),
                &case.transcript(),
                None,
            )
        };
        list.push(Scenario {
            name: Box::leak(format!("review-{}", case.name).into_boxed_str()),
            history: vec![user(&prompt)],
            info: "ok",
            expect: if case.reusable {
                Expect::Skill
            } else {
                Expect::NoSkill
            },
            tools: Some(if legacy || may_skill {
                vec!["memory", "skill_manage", "skill"]
            } else {
                vec!["memory"]
            }),
            guard: (!legacy).then_some((work, asked)),
        });
    }
    list
}

/// The review's instructions as they were before they were tuned.
fn legacy_review_prompt(transcript: &str) -> String {
    format!(
        "You are reviewing a recent conversation between the user and an AI agent to curate long-term memory and skills. \
         Most conversations teach nothing lasting: saving nothing is a good outcome.\n\n\
         Memory (`memory`): target `user` for durable facts about the user (preferences, environment such as machines, \
         services and paths, conventions, corrections they gave); target `memory` for lessons that will help future work. \
         Save only what the conversation shows (the user said it, or a tool result showed it). A diagnosis or guess, such as \
         \"the API is disabled\" after errors from a wrong endpoint, is not a fact until it was confirmed: leave it out. \
         When the conversation shows a stored entry is wrong or stale, correct it (`replace`) or `remove` it. Keep entries \
         short and declarative, never duplicate, consolidate when space is tight; no task progress or one-off details.\n\n\
         Skills (`skill_manage`): only for a procedure that is non-trivial, likely to recur, took trial and error or the \
         user's corrections to get right, and was shown to work (say, the endpoints, pagination and time zones that finally \
         worked for a service's API). Prefer updating a skill listed below over creating one; never make a skill for a \
         one-off task or for something any capable agent does unaided. If two `user` skills clearly cover the same thing, \
         merge them into one and delete the other.\n\n\
         If nothing is worth saving, reply exactly: Nothing to save.\n\n\
         <current_user_profile>\n\n</current_user_profile>\n<current_memory>\n\n</current_memory>\n\
         <current_skills>\n(none)\n</current_skills>\n\n<conversation>\n{transcript}\n</conversation>"
    )
}

/// Whether a call puts something in front of the user in a tile.
fn opens_tile(name: &str, args: &Value) -> bool {
    match name {
        "whiteboard" => args["action"].as_str().unwrap_or("show") == "show",
        "browser" => {
            args["show"].as_bool() == Some(true) || args["action"].as_str() == Some("show")
        }
        _ => false,
    }
}

fn canned(name: &str, args: &Value, info: &str) -> String {
    match name {
        "web_fetch" | "web_search" => info.to_string(),
        "browser" => match args["action"].as_str().unwrap_or("") {
            "open" | "navigate" | "snapshot" => format!("[t1] opened\n{info}"),
            _ => "ok".into(),
        },
        "whiteboard" => match args["action"].as_str().unwrap_or("show") {
            "show" => "Shown in a tile beside the chat (id wb_1).".into(),
            _ => "ok".into(),
        },
        "skill_manage" | "memory" => "Saved.".into(),
        _ => "ok".into(),
    }
}

/// The tile and skill wording as it was before it was tuned.
fn legacy(system: &mut Value, tools: &mut Value) {
    let text = system["content"].as_str().unwrap().to_string();
    let swap = |text: &str, prefix: &str, with: &str| -> String {
        let mut found = false;
        let out: Vec<String> = text
            .lines()
            .map(|l| {
                if l.starts_with(prefix) {
                    found = true;
                    with.to_string()
                } else {
                    l.to_string()
                }
            })
            .collect();
        assert!(found, "no system prompt line starts with {prefix:?}");
        out.join("\n")
    };
    let text = swap(
        &text,
        "- Save durable facts about the user",
        "- Save durable facts about the user and their environment with `memory` when you learn them (what you observed, not what you inferred); save reusable procedures you worked out with `skill_manage`.",
    );
    let text = swap(
        &text,
        "- Answer in the chat",
        "- To show the user a visual, page, image or web app, or to get a signature or answer from them, use `whiteboard`. For how Tessera itself works, read `docs` instead of guessing.",
    );
    system["content"] = json!(text);
    let old = [
        (
            "whiteboard",
            "Tiles beside this chat. `show` one of: `html` (self-contained page, no network; `three` and d3 bundled, see `docs whiteboards`), `markdown`, `image_path`, `url` or `local_port` (a web app here); an `id` updates that board. Show only when a visual helps or the user asked. `ask` waits for a `signature` (you get a PNG), `text`, `choice` or `confirm`. `read` (`id`/`title`, else the latest) returns a board's content, also pages the user opened; `look` shows you their screen of it. Use them when they mean \"this page\", \"the board\" or what they opened. `list` shows all.",
        ),
        (
            "browser",
            "Real browser shared with the user's web view tiles and logins. `open` a url, `snapshot` for refs to `click`/`type`. Read `docs browser` first.",
        ),
        (
            "skill_manage",
            "Create or improve reusable skills. After working out a non-trivial procedure that is likely to recur (a deployment, a debugging recipe, a project convention), save it as a skill. Actions: `create` (name, description, content = markdown body), `update` (replace `old` with `new` in SKILL.md, or set `content` to rewrite the body), `write_file` (supporting file at `file`), `delete`. `scope`: user (default, available everywhere) or project (this repository's .tessera/skills).",
        ),
    ];
    for t in tools.as_array_mut().unwrap() {
        for (name, desc) in old {
            if t["function"]["name"] == name {
                t["function"]["description"] = json!(desc);
            }
        }
    }
}

#[tokio::test]
#[ignore]
async fn agents_keep_to_the_chat_and_save_skills_rarely() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        return;
    };
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let id = model.split_once('/').unwrap().1.to_string();
    let runs: usize = std::env::var("PROBE_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let only: Vec<String> = std::env::var("PROBE_CASES")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();

    // The engine's own prompt and tools for an ordinary chat.
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let mut system = req["messages"][0].clone();
    let mut tools = req["tools"].clone();
    if std::env::var_os("PROBE_LEGACY").is_some() {
        legacy(&mut system, &mut tools);
        eprintln!("(the wording from before it was tuned)");
    }
    let http = reqwest::Client::new();

    let legacy_mode = std::env::var_os("PROBE_LEGACY").is_some();
    let mut wrong = Vec::new();
    eprintln!(
        "\n{:<26} {:<9} {:<5} {:<6} {:<5} {:<5} calls",
        "scenario", "expect", "tile", "skill", "kept", "asks"
    );
    for sc in scenarios(legacy_mode)
        .into_iter()
        .filter(|s| only.is_empty() || only.iter().any(|o| o == s.name))
    {
        for run in 0..runs {
            let mut messages = vec![system.clone()];
            messages.extend(sc.history.clone());
            let mut calls: Vec<String> = Vec::new();
            let (mut tile, mut skill, mut kept) = (false, false, 0);
            // Whether the agent asks the user about a skill instead of deciding.
            let mut asks = false;
            let mut final_text = String::new();
            let offered: Vec<Value> = tools
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| {
                    sc.tools
                        .as_ref()
                        .is_none_or(|only| only.iter().any(|n| t["function"]["name"] == *n))
                })
                .cloned()
                .collect();
            for _ in 0..6 {
                let body = json!({
                    "model": id, "messages": messages, "tools": offered,
                    "tool_choice": "auto", "stream": false, "max_tokens": 12000,
                });
                // A busy local server sometimes answers with something that
                // isn't JSON: try again.
                let mut resp = Value::Null;
                for attempt in 0..4 {
                    let sent = http
                        .post(format!("{url}/chat/completions"))
                        .json(&body)
                        .send()
                        .await;
                    if let Ok(r) = sent
                        && let Ok(v) = r.json::<Value>().await
                        && v["choices"][0]["message"].is_object()
                    {
                        resp = v;
                        break;
                    }
                    eprintln!("(no usable answer, attempt {})", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                let msg = resp["choices"][0]["message"].clone();
                let tool_calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();
                messages.push(json!({
                    "role": "assistant",
                    "content": msg["content"].clone(),
                    "tool_calls": tool_calls,
                }));
                if resp["choices"][0]["finish_reason"] == "length" {
                    calls.push("[cut off]".into());
                }
                if tool_calls.is_empty() {
                    final_text = msg["content"].as_str().unwrap_or("").to_string();
                    let said = final_text.to_lowercase();
                    // (The review talks to nobody: only an ordinary chat counts.)
                    asks = sc.tools.is_none() && said.contains("skill") && said.contains('?');
                    break;
                }
                for c in &tool_calls {
                    let name = c["function"]["name"].as_str().unwrap_or("");
                    let args: Value =
                        serde_json::from_str(c["function"]["arguments"].as_str().unwrap_or("{}"))
                            .unwrap_or(Value::Null);
                    tile |= opens_tile(name, &args);
                    let creates = name == "skill_manage" && args["action"] == "create";
                    skill |= creates;
                    let mut answer = canned(name, &args, sc.info);
                    if creates {
                        let verdict = match sc.guard {
                            // Not Tessera's own checks (the old behaviour).
                            None => Ok(()),
                            Some(_) if kept > 0 => {
                                Err("Not saved: one new skill per turn is plenty. Carry on.".into())
                            }
                            Some((tool_calls, user_asked)) => tessera_core::skills::worth_saving(
                                args["name"].as_str().unwrap_or(""),
                                args["description"].as_str().unwrap_or(""),
                                args["content"].as_str().unwrap_or(""),
                                &[],
                                "user",
                                tessera_core::skills::Evidence {
                                    tool_calls,
                                    user_asked,
                                },
                            ),
                        };
                        match verdict {
                            Ok(()) => kept += 1,
                            Err(e) => answer = e,
                        }
                    }
                    let act = args["action"]
                        .as_str()
                        .map(|a| format!(":{a}"))
                        .unwrap_or_default();
                    let show = if args["show"].as_bool() == Some(true) {
                        "+show"
                    } else {
                        ""
                    };
                    calls.push(format!("{name}{act}{show}"));
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": c["id"],
                        "content": answer,
                    }));
                }
            }
            let label = match sc.expect {
                Expect::Chat => "chat",
                Expect::Tile => "tile",
                Expect::NoSkill => "no skill",
                Expect::MaySkill => "may skill",
                Expect::Skill => "skill",
            };
            eprintln!(
                "{:<26} {:<9} {:<5} {:<6} {:<5} {:<5} {}",
                format!("{}#{run}", sc.name),
                label,
                tile,
                skill,
                kept,
                asks,
                calls.join(" → ")
            );
            if sc.expect == Expect::Tile && !tile {
                eprintln!(
                    "    it said: {}",
                    tessera_core::util::truncate(&final_text.replace('\n', " "), 160)
                );
            }
            if asks {
                wrong.push(format!("{}#{run} asked about a skill", sc.name));
            }
            let bad = match sc.expect {
                Expect::Chat => tile,
                Expect::Tile => !tile,
                // What gets saved, after Tessera's own checks.
                Expect::NoSkill => kept > 0,
                Expect::MaySkill => false,
                Expect::Skill => kept == 0,
            };
            if bad {
                wrong.push(format!("{}#{run}", sc.name));
            }
        }
    }
    eprintln!("\nwrong: {wrong:?}");
    if std::env::var_os("PROBE_REPORT_ONLY").is_some() {
        return;
    }
    assert!(wrong.is_empty(), "unexpected choices: {wrong:?}");
}
