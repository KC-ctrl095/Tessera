//! Skills: what is worth saving (checks on `skill_manage create`, the memory
//! review's offer of skill tools), deleting one from Settings, and the
//! prompt's chat-first wording for tiles. Scripted mock model throughout.
//! (`live_skills.rs` and `live_choices.rs` measure the judgement on a real model.)

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

fn bash(n: u32) -> Reply {
    Reply::Tools(vec![(
        "bash".into(),
        json!({ "command": format!("echo step {n}") }),
    )])
}

fn create(name: &str, description: &str, content: &str) -> Reply {
    Reply::Tools(vec![(
        "skill_manage".into(),
        json!({"action": "create", "name": name, "description": description, "content": content}),
    )])
}

const DEPLOY: &str = "1. `hugo --minify --baseURL https://blog.ken.lan/` (without the base URL the links point at localhost).\n\
2. `rsync -a --delete --exclude .well-known public/ homelab:/srv/blog/` (keep the trailing slash on public/; .well-known holds the certificates).\n\
3. `ssh homelab 'sudo systemctl reload caddy'`.\n\
4. `curl -sI https://blog.ken.lan | head -1` must answer 200.";

fn skill_dirs(h: &Harness) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(h.dir.path().join("skills"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Chats that run without asking: the skill checks alone decide (that a
/// skill change asks otherwise is checked below).
const YOLO: &str = "permission = \"auto\"";

/// What each `skill_manage` call in the chat answered, in order.
async fn skill_answers(h: &Harness, id: &str) -> Vec<(bool, String)> {
    tool_results(&h.entries(id).await)
        .into_iter()
        .filter(|(tool, _, _)| tool == "skill_manage")
        .map(|(_, err, text)| (err, text))
        .collect()
}

#[tokio::test]
async fn a_chat_without_real_work_cannot_make_a_skill() {
    let h = harness_agent(
        vec![
            create("rename-files", "Rename files with mv", DEPLOY),
            Reply::Text("ok".into()),
        ],
        100_000,
        YOLO,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "rename notes.txt to meeting-notes.txt").await;
    h.settle(&s.id).await;
    let answers = skill_answers(&h, &s.id).await;
    assert_eq!(answers.len(), 1);
    assert!(
        answers[0].0 && answers[0].1.contains("real work"),
        "{answers:?}"
    );
    assert!(skill_dirs(&h).is_empty());
}

#[tokio::test]
async fn real_work_earns_one_skill_per_turn_and_near_duplicates_are_refused() {
    let h = harness_agent(
        vec![
            bash(1),
            bash(2),
            bash(3),
            bash(4),
            create(
                "deploy-blog",
                "Deploy the blog to the homelab with hugo and rsync",
                DEPLOY,
            ),
            // A second skill in the same turn.
            create(
                "homelab-cache-flush",
                "Flush the homelab cache after a deploy and check it",
                DEPLOY,
            ),
            Reply::Text("saved".into()),
            // Next turn: the same thing again, in other words.
            create(
                "blog-deploy-homelab",
                "Deploy the blog to the homelab: hugo, rsync, caddy reload",
                DEPLOY,
            ),
            Reply::Text("not saved".into()),
        ],
        100_000,
        YOLO,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "deploy the blog").await;
    h.settle(&s.id).await;
    let answers = skill_answers(&h, &s.id).await;
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert!(!answers[0].0, "the first skill is saved: {answers:?}");
    assert!(
        answers[1].0 && answers[1].1.contains("one new skill per turn"),
        "{answers:?}"
    );
    assert_eq!(skill_dirs(&h), vec!["deploy-blog"]);
    let saved = std::fs::read_to_string(h.dir.path().join("skills/deploy-blog/SKILL.md")).unwrap();
    assert!(saved.contains("name: deploy-blog") && saved.contains("rsync -a --delete"));

    h.send(&s.id, "and again please").await;
    h.settle(&s.id).await;
    let answers = skill_answers(&h, &s.id).await;
    assert_eq!(answers.len(), 3, "{answers:?}");
    assert!(
        answers[2].0 && answers[2].1.contains("`deploy-blog` already covers"),
        "{answers:?}"
    );
    assert_eq!(skill_dirs(&h), vec!["deploy-blog"]);
}

#[tokio::test]
async fn thin_skills_are_refused_unless_the_user_asked_for_one() {
    let h = harness_agent(
        vec![
            bash(1),
            bash(2),
            bash(3),
            bash(4),
            create("ls-things", "List things in a folder", "Run ls."),
            Reply::Text("hm".into()),
            // The user asks outright: no tool calls needed, a short body is fine.
            create(
                "standup-notes",
                "Weekly standup notes from merged pull requests",
                "Group last week's merged PRs by repo, one line each, skip dependabot, newest first.",
            ),
            Reply::Text("saved".into()),
        ],
        100_000,
        YOLO,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "list the files").await;
    h.settle(&s.id).await;
    let answers = skill_answers(&h, &s.id).await;
    assert!(
        answers[0].0 && answers[0].1.contains("too short"),
        "{answers:?}"
    );
    assert!(skill_dirs(&h).is_empty());

    h.send(
        &s.id,
        "Save that as a skill: standup notes the way I described",
    )
    .await;
    h.settle(&s.id).await;
    let answers = skill_answers(&h, &s.id).await;
    assert!(!answers[1].0, "{answers:?}");
    assert_eq!(skill_dirs(&h), vec!["standup-notes"]);
}

#[tokio::test]
async fn skill_requests_are_asked_for_in_plain_words() {
    for yes in [
        "save this as a skill",
        "Can you make a skill out of that?",
        "turn this into a skill",
        "remember how to do this as a skill",
    ] {
        assert!(tessera_core::skills::asks_for_skill(yes), "{yes}");
    }
    for no in [
        "what skills do you have?",
        "my skill at chess is bad",
        "rename the file",
        "make a table of the results",
    ] {
        assert!(!tessera_core::skills::asks_for_skill(no), "{no}");
    }
}

// ---- the memory review -----------------------------------------------------

async fn reviewing_harness() -> Harness {
    let mock = Mock::start(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        "default_model = \"mock/m\"\n[agent]\nreview_interval = 1\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\n",
        mock.url
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

const KEY: &str = "You are reviewing a recent conversation";

/// The review's request: its prompt and the tools it was offered.
async fn review_request(h: &Harness) -> (String, Vec<String>) {
    for _ in 0..200 {
        let found = h
            .mock
            .state
            .requests
            .lock()
            .iter()
            .find(|r| r.to_string().contains(KEY))
            .cloned();
        if let Some(r) = found {
            let tools = r["tools"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            // The review's instructions are its user message (the system
            // prompt is the ordinary agent's).
            let prompt = r["messages"]
                .as_array()
                .and_then(|m| m.iter().find(|m| m["role"] == "user"))
                .map(|m| m["content"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            return (prompt, tools);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the review never ran");
}

#[tokio::test]
async fn a_quick_chat_gets_a_review_without_skill_tools() {
    let h = reviewing_harness().await;
    h.mock.route(KEY, Reply::Text("Nothing to save.".into()));
    h.mock.push(Reply::Text("Renamed.".into()));
    let s = h.session().await;
    h.send(&s.id, "rename notes.txt to meeting-notes.txt").await;
    h.settle(&s.id).await;
    let (prompt, tools) = review_request(&h).await;
    assert_eq!(tools, vec!["memory"], "only memory is offered");
    assert!(prompt.contains("is not a fact until it was confirmed"));
    assert!(
        !prompt.contains("skill_manage") && !prompt.contains("<current_skills>"),
        "{prompt}"
    );
}

/// A skill is instructions later chats load: changing one asks (showing
/// what it would say), as writing into the skills folder does.
#[tokio::test]
async fn changing_a_skill_asks_first() {
    let h = harness(
        vec![
            bash(1),
            bash(2),
            bash(3),
            bash(4),
            create(
                "deploy-blog",
                "Deploy the blog to the homelab with hugo and rsync",
                DEPLOY,
            ),
            Reply::Text("saved".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(&s.id, "deploy the blog").await;
    let request = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("the skill change asks");
    assert_eq!(request.tool, "skill_manage");
    assert!(request.reason.contains("deploy-blog"), "{request:?}");
    assert!(
        request
            .summary
            .starts_with("create skill deploy-blog: Deploy the blog")
            && request.summary.contains("rsync -a --delete"),
        "{request:?}"
    );
    assert!(skill_dirs(&h).is_empty(), "nothing saved before the answer");
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
    assert_eq!(skill_dirs(&h), vec!["deploy-blog"]);
}

/// Text read from the web must not become instructions every later chat
/// loads: a review of a stretch with web results gets no skill tools.
#[tokio::test]
async fn a_review_of_web_reading_gets_no_skill_tools() {
    let h = reviewing_harness().await;
    h.mock.route(KEY, Reply::Text("Nothing to save.".into()));
    for n in 1..=4 {
        h.mock.push(bash(n));
    }
    h.mock.push(Reply::Tools(vec![(
        "web_fetch".into(),
        // Not fetched (no network in tests); its result is still a web one.
        json!({ "url": "ftp://example.com/notes" }),
    )]));
    h.mock.push(Reply::Text("Deployed.".into()));
    let s = h.session().await;
    h.send(&s.id, "deploy the blog to my homelab").await;
    h.settle(&s.id).await;
    let (_, tools) = review_request(&h).await;
    assert_eq!(tools, vec!["memory"], "{tools:?}");
}

#[tokio::test]
async fn a_chat_that_did_real_work_gets_the_skill_rules_and_one_new_skill() {
    let h = reviewing_harness().await;
    // The reviewer tries two skills; the second is one too many.
    h.mock.route(
        KEY,
        create(
            "deploy-blog",
            "Deploy the blog to the homelab with hugo and rsync",
            DEPLOY,
        ),
    );
    h.mock.route(
        KEY,
        create(
            "caddy-reload",
            "Reload caddy on the homelab after changing files",
            DEPLOY,
        ),
    );
    h.mock.route(KEY, Reply::Text("Saved.".into()));
    for n in 1..=4 {
        h.mock.push(bash(n));
    }
    h.mock.push(Reply::Text("Deployed.".into()));
    let s = h.session().await;
    h.send(&s.id, "deploy the blog to my homelab").await;
    h.settle(&s.id).await;
    let notice = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = h
                .entries(&s.id)
                .await
                .into_iter()
                .find_map(|e| match e.body {
                    EntryBody::Notice { learned, .. } if !learned.is_empty() => Some(learned),
                    _ => None,
                });
            if let Some(l) = found {
                return l;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a learned notice");
    assert_eq!(
        notice.len(),
        1,
        "only the accepted skill is reported: {notice:?}"
    );
    assert_eq!(notice[0].text, "deploy-blog");
    assert_eq!(skill_dirs(&h), vec!["deploy-blog"]);
    let (prompt, tools) = review_request(&h).await;
    assert!(tools.contains(&"skill_manage".to_string()), "{tools:?}");
    for want in [
        "the usual answer is none",
        "ALL hold",
        "single fact or preference",
        "At most one new skill per review",
        "Decide yourself: nobody is asked",
        "<current_skills>",
    ] {
        assert!(prompt.contains(want), "{want} missing");
    }
}

/// Every review request so far, in order.
fn review_requests(h: &Harness) -> Vec<serde_json::Value> {
    h.mock
        .state
        .requests
        .lock()
        .iter()
        .filter(|r| r.to_string().contains(KEY))
        .cloned()
        .collect()
}

async fn reviews(h: &Harness, n: usize) -> Vec<serde_json::Value> {
    for _ in 0..200 {
        let r = review_requests(h);
        if r.len() >= n {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("expected {n} reviews, saw {}", review_requests(h).len());
}

#[tokio::test]
async fn a_review_is_lean_and_reads_only_what_is_new() {
    let h = reviewing_harness().await;
    h.engine
        .handle(
            Request::SetMemory {
                target: MemoryTarget::Memory,
                content: "The blog is built with hugo.".into(),
            },
            None,
        )
        .await
        .unwrap();
    for _ in 0..2 {
        h.mock.route(KEY, Reply::Text("Nothing to save.".into()));
    }
    h.mock.push(Reply::Text("Noted.".into()));
    h.mock.push(Reply::Text("You're welcome.".into()));
    h.mock.push(Reply::Text("Noted too.".into()));
    let s = h.session().await;
    h.send(&s.id, "my homelab runs proxmox on 192.168.1.20")
        .await;
    h.settle(&s.id).await;
    let first = reviews(&h, 1).await.remove(0);
    // Its own short system prompt, not the agent's; the memory once, in its message.
    assert_eq!(
        first["messages"][0]["content"],
        tessera_core::memory::REVIEW_SYSTEM
    );
    assert_eq!(
        first
            .to_string()
            .matches("The blog is built with hugo.")
            .count(),
        1,
        "{first}"
    );
    assert!(first.to_string().contains("192.168.1.20"));
    // "thanks" teaches nothing: no review.
    h.send(&s.id, "thanks").await;
    h.settle(&s.id).await;
    h.send(&s.id, "the backup disk is a 4 TB WD Red").await;
    h.settle(&s.id).await;
    let all = reviews(&h, 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(review_requests(&h).len(), 2);
    // The second reads what came after the first, not the whole chat again.
    let second = all[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(second.contains("4 TB WD Red"), "{second}");
    assert!(second.contains("reviewed already"), "{second}");
    assert!(!second.contains("192.168.1.20"), "{second}");
}

#[tokio::test]
async fn a_review_of_a_chat_that_asked_for_a_skill_may_save_it() {
    let h = reviewing_harness().await;
    h.mock.route(
        KEY,
        create(
            "standup-notes",
            "Weekly standup notes from merged pull requests",
            "Group last week's merged PRs by repo, one line each, skip dependabot, newest first.",
        ),
    );
    h.mock.route(KEY, Reply::Text("Saved.".into()));
    h.mock.push(Reply::Text("Will do.".into()));
    let s = h.session().await;
    h.send(
        &s.id,
        "From now on save my standup notes routine as a skill: merged PRs by repo, no dependabot",
    )
    .await;
    h.settle(&s.id).await;
    let (_, tools) = review_request(&h).await;
    assert!(tools.contains(&"skill_manage".to_string()), "{tools:?}");
    for _ in 0..100 {
        if skill_dirs(&h) == vec!["standup-notes"] {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "the skill the user asked for was not saved: {:?}",
        skill_dirs(&h)
    );
}

// ---- deleting a skill (Settings) ----------------------------------------------

fn write_skill(h: &Harness, name: &str) {
    let dir = h.dir.path().join("skills").join(name);
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: Does {name} things.\n---\n1. one\n2. two\n3. three\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("scripts/run.sh"), "echo hi\n").unwrap();
}

/// The user's own skills (a machine can also have project skills in the
/// folders above wherever the tests run).
async fn list_skills(h: &Harness) -> Vec<SkillInfo> {
    let all: Vec<SkillInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSkills { cwd: None }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    all.into_iter().filter(|s| s.source == "user").collect()
}

#[tokio::test]
async fn a_skill_can_be_deleted_from_settings() {
    let h = harness(vec![], 100_000).await;
    write_skill(&h, "alpha");
    write_skill(&h, "beta");
    let listed = list_skills(&h).await;
    assert_eq!(
        listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert!(listed.iter().all(|s| s.updated_at > 0));

    // Settings can read what a skill says before deleting it.
    let text: String = serde_json::from_value(
        h.engine
            .handle(
                Request::ReadSkill {
                    name: "alpha".into(),
                    cwd: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        text.contains("name: alpha") && text.contains("1. one"),
        "{text}"
    );
    let err = h
        .engine
        .handle(
            Request::ReadSkill {
                name: "nope".into(),
                cwd: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    let gone = h
        .engine
        .handle(
            Request::DeleteSkill {
                name: "alpha".into(),
                cwd: None,
            },
            None,
        )
        .await
        .unwrap();
    assert!(gone.is_null());
    assert_eq!(skill_dirs(&h), vec!["beta"], "its whole folder is gone");
    assert_eq!(list_skills(&h).await.len(), 1);

    // Deleting what is not there is an error, not a silent success.
    let err = h
        .engine
        .handle(
            Request::DeleteSkill {
                name: "alpha".into(),
                cwd: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
    let err = h
        .engine
        .handle(
            Request::DeleteSkill {
                name: "../beta".into(),
                cwd: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::NotFound,
        "only listed skills can go: {err:?}"
    );
    assert_eq!(skill_dirs(&h), vec!["beta"]);
}

#[cfg(unix)]
#[tokio::test]
async fn deleting_a_skill_never_follows_a_link_out_of_its_folder() {
    let h = harness(vec![], 100_000).await;
    write_skill(&h, "linked");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep.txt"), "mine").unwrap();
    std::os::unix::fs::symlink(outside.path(), h.dir.path().join("skills/linked/data")).unwrap();
    assert_eq!(list_skills(&h).await.len(), 1);
    h.engine
        .handle(
            Request::DeleteSkill {
                name: "linked".into(),
                cwd: None,
            },
            None,
        )
        .await
        .unwrap();
    assert!(list_skills(&h).await.is_empty());
    assert!(!h.dir.path().join("skills/linked").exists());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("keep.txt")).unwrap(),
        "mine",
        "what the link pointed at is untouched"
    );
}

#[test]
fn the_skill_requests_read_as_the_apps_send_them() {
    let del: Request = serde_json::from_value(
        json!({"method": "delete_skill", "params": {"name": "x", "cwd": null}}),
    )
    .unwrap();
    assert!(matches!(del, Request::DeleteSkill { ref name, cwd: None } if name == "x"));
    let read: Request =
        serde_json::from_value(json!({"method": "read_skill", "params": {"name": "x"}})).unwrap();
    assert!(matches!(read, Request::ReadSkill { cwd: None, .. }));
}

// ---- tiles --------------------------------------------------------------------

#[tokio::test]
async fn the_prompt_and_tools_say_chat_first_and_tiles_when_asked() {
    let h = harness(vec![Reply::Text("ok".into())], 100_000).await;
    let s = h.session().await;
    h.send(&s.id, "hi").await;
    h.settle(&s.id).await;
    let req = h.mock.agent_requests()[0].clone();
    let system = req["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Answer in the chat"), "{system}");
    assert!(
        system.contains("only when the user asks to see it or must act there"),
        "{system}"
    );
    assert!(system.contains("`skill_manage` is rare"));
    let desc = |name: &str| {
        req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == name)
            .and_then(|t| t["function"]["description"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    // Agents' work is cards in the chat; a tile only when asked (AGENT-TILES).
    assert!(
        desc("whiteboard").contains("a card in the chat")
            && desc("whiteboard").contains("`open: true` also opens a tile, only if asked"),
        "{}",
        desc("whiteboard")
    );
    assert!(desc("browser").contains("in background tabs"));
    assert!(desc("browser").contains("`show` a tab only if the user asks to see it"));
    assert!(desc("browser").contains("`needs_user` + `reason` hands it to them"));
    assert!(desc("note").contains("`show: true` opens it on screen, only if asked"));
    assert!(desc("skill_manage").contains("Rare"));
    assert!(desc("skill_manage").contains("Decide yourself, don't ask"));
}
