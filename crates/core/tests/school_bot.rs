//! A bot doing a real job end to end: a study partner for school with its own
//! memory and a syllabus as knowledge. Plus channel turn-taking limits.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

const REVIEW: &str = "You are reviewing a recent conversation";

async fn call<T: serde::de::DeserializeOwned>(h: &Harness, req: Request) -> T {
    serde_json::from_value(h.engine.handle(req, None).await.unwrap()).unwrap()
}

/// Chats are reviewed after every turn.
async fn reviewing_harness() -> Harness {
    let mock = Mock::start(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "default_model = \"mock/m\"\n[agent]\nreview_interval = 1\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\n",
            mock.url
        ),
    )
    .unwrap();
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

fn study_bot(knowledge: Vec<String>) -> Bot {
    serde_json::from_value(json!({
        "id": "", "name": "Study buddy", "handle": "study", "avatar": "", "color": "",
        "role": "school study partner",
        "personality": "Patient and encouraging; explains step by step.",
        "instructions": "Help me prepare for exams: quiz me, explain what I get wrong, keep track of deadlines.",
        "permission": "default", "tools": "assistant", "own_memory": true, "share_user_profile": true,
        "knowledge": knowledge,
        "starters": ["Quiz me on chapter 3", "  ", "What's due this week?", "Explain osmosis simply",
                     "Make me a study plan", "One too many"]
    }))
    .unwrap()
}

async fn chat(h: &Harness, bot: Option<&Bot>) -> SessionInfo {
    call(
        h,
        Request::CreateSession {
            cwd: Some(h.work.display().to_string()),
            model: None,
            title: None,
            bot_id: bot.map(|b| b.id.clone()),
            private: false,
            lean: false,
        },
    )
    .await
}

/// Wait until a review of a conversation mentioning `marker` has finished
/// (its requests stop coming).
async fn review_done(h: &Harness, marker: &str, requests: usize) {
    for _ in 0..200 {
        let n = h
            .mock
            .state
            .requests
            .lock()
            .iter()
            .filter(|r| {
                let t = r.to_string();
                t.contains(REVIEW) && t.contains(marker)
            })
            .count();
        if n >= requests {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the review of `{marker}` didn't run");
}

fn system(req: &Value) -> String {
    req["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn a_study_bot_remembers_reads_its_knowledge_and_keeps_to_its_own_conversations() {
    let h = reviewing_harness().await;
    let syllabus: Note = call(
        &h,
        Request::SaveNote {
            id: None,
            title: "Biology 101 syllabus".into(),
            content: "# Biology 101 syllabus\nExam 1: chapters 3-5 (cell respiration, photosynthesis).\nLab reports are due on Fridays.".into(),
            base_version: None,
            session_id: None,
        },
    )
    .await;
    let taxes: Note = call(
        &h,
        Request::SaveNote {
            id: None,
            title: "Tax return".into(),
            content: "refund details".into(),
            base_version: None,
            session_id: None,
        },
    )
    .await;
    // Every agent and bot reads the user's notes unless the user keeps one from them (batch 17).
    let _: Note = call(
        &h,
        Request::SetNoteAgents {
            id: taxes.id.clone(),
            keep_from_agents: true,
        },
    )
    .await;
    // Knowledge keeps only notes that exist.
    let bot: Bot = call(
        &h,
        Request::SaveBot {
            bot: study_bot(vec![syllabus.id.clone(), "nt_missing".into()]),
        },
    )
    .await;
    assert_eq!(bot.knowledge, vec![syllabus.id.clone()]);
    // Starters: at most four, blanks dropped; for the apps only.
    assert_eq!(
        bot.starters,
        [
            "Quiz me on chapter 3",
            "What's due this week?",
            "Explain osmosis simply",
            "Make me a study plan"
        ]
    );

    // Session 1: the student shares the exam date; the bot reads its syllabus.
    let s1 = chat(&h, Some(&bot)).await;
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "read", "id": syllabus.id}),
    )]));
    h.mock.push(Reply::Text(
        "Exam 1 on Oct 14 covers chapters 3-5. Let's start with cell respiration: what does glycolysis produce?"
            .into(),
    ));
    h.mock.route(
        REVIEW,
        Reply::Tools(vec![(
            "memory".into(),
            json!({"action": "add", "target": "memory", "content": "Biology exam 1 is on Oct 14 (chapters 3-5); the student finds cell respiration hard."}),
        )]),
    );
    h.mock.route(REVIEW, Reply::Text("Saved.".into()));
    h.send(
        &s1.id,
        "My first biology exam is on Oct 14. I struggle with cell respiration.",
    )
    .await;
    h.settle(&s1.id).await;
    let first = h.mock.agent_requests()[0].clone();
    let prompt = system(&first);
    assert!(
        prompt.starts_with("You are Study buddy (@study)"),
        "{prompt}"
    );
    for want in [
        "`docs bots`",
        "# Knowledge",
        &format!("{} \"Biology 101 syllabus\"", syllabus.id),
        "# Your responsibilities",
    ] {
        assert!(prompt.contains(want), "{want} missing");
    }
    assert!(!prompt.contains("Tax return"));
    assert!(
        !prompt.contains("Quiz me on chapter 3"),
        "starters stay out of the prompt"
    );
    let tools: Vec<String> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    for t in ["note", "recall", "whiteboard", "memory"] {
        assert!(tools.contains(&t.to_string()), "{t} missing: {tools:?}");
    }
    assert!(
        !tools.contains(&"bash".to_string()),
        "assistants have no shell"
    );
    let r = tool_results(&h.entries(&s1.id).await);
    assert!(r[0].2.contains("Lab reports are due on Fridays"), "{r:?}");
    review_done(&h, "I struggle with cell respiration", 2).await;
    // It learned into its own memory, and said so in the chat.
    let notice = h
        .entries(&s1.id)
        .await
        .into_iter()
        .find_map(|e| match e.body {
            EntryBody::Notice { text, learned, .. } if !learned.is_empty() => Some(text),
            _ => None,
        });
    assert!(notice.unwrap_or_default().contains("cell respiration"));
    let shared: MemoryState = call(&h, Request::GetMemory).await;
    assert!(!shared.memory.contains("cell respiration"));
    let own = std::fs::read_to_string(
        h.dir
            .path()
            .join("bots")
            .join(&bot.id)
            .join("memory/MEMORY.md"),
    )
    .unwrap();
    assert!(own.contains("Oct 14"));

    // An unrelated chat of the user's (not the bot's).
    let other = chat(&h, None).await;
    h.mock
        .push(Reply::Text("Twelve chapters is a good length.".into()));
    h.mock.route(REVIEW, Reply::Text("Nothing to save.".into()));
    h.send(
        &other.id,
        "My novel draft has twelve chapters about cell respiration.",
    )
    .await;
    h.settle(&other.id).await;
    review_done(&h, "novel draft", 1).await;

    // Session 2: the bot remembers, finds its own earlier session, and keeps
    // out of the user's other chats and notes.
    let s2 = chat(&h, Some(&bot)).await;
    h.mock.push(Reply::Tools(vec![
        (
            "recall".into(),
            json!({"query": "respiration", "scope": "all"}),
        ),
        ("recall".into(), json!({"session_id": other.id, "seq": 1})),
        ("note".into(), json!({"action": "list"})),
        ("note".into(), json!({"action": "read", "id": taxes.id})),
    ]));
    h.mock.push(Reply::Text(
        "Quiz time: what are the products of glycolysis?".into(),
    ));
    h.mock.route(REVIEW, Reply::Text("Nothing to save.".into()));
    h.send(&s2.id, "Quiz me for my exam").await;
    h.settle(&s2.id).await;
    let req = h
        .mock
        .agent_requests()
        .into_iter()
        .find(|r| r.to_string().contains("Quiz me for my exam"))
        .unwrap();
    assert!(system(&req).contains("cell respiration hard"), "remembers");
    let r = tool_results(&h.entries(&s2.id).await);
    assert!(
        r[0].2.contains(&format!("· {}]", s1.id)) && !r[0].2.contains(&format!("· {}]", other.id)),
        "only its own conversations ({} / {}): {r:?}",
        s1.id,
        other.id
    );
    assert!(r[1].1, "not the user's other chats: {r:?}");
    // The apps get the hits as data, to link them to their chats.
    let details = h
        .entries(&s2.id)
        .await
        .into_iter()
        .find_map(|e| match e.body {
            EntryBody::ToolResult { tool, details, .. } if tool == "recall" => details,
            _ => None,
        })
        .unwrap();
    let recall = &details["recall"];
    assert_eq!(
        (recall["query"].as_str(), recall["scope"].as_str()),
        (Some("respiration"), Some("all"))
    );
    let hits = recall["hits"].as_array().unwrap();
    assert!(hits.iter().any(|h| {
        h["session_id"] == s1.id.as_str()
            && h["seq"].as_u64().is_some()
            && h["ts"].as_u64().is_some()
            && h["snippet"]
                .as_str()
                .is_some_and(|t| t.contains("respiration"))
    }));
    assert!(hits.iter().all(|h| h["session_id"] != other.id.as_str()));
    assert!(
        r[2].2.contains("Biology 101 syllabus") && !r[2].2.contains("Tax return"),
        "{r:?}"
    );
    assert!(r[3].1, "{r:?}");

    // The user's own agent can look back at the bot's sessions.
    let mine = chat(&h, None).await;
    h.mock.push(Reply::Tools(vec![(
        "recall".into(),
        json!({"query": "glycolysis", "scope": "all"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    h.mock.route(REVIEW, Reply::Text("Nothing to save.".into()));
    h.send(&mine.id, "what did my study bot quiz me on?").await;
    h.settle(&mine.id).await;
    let r = tool_results(&h.entries(&mine.id).await);
    assert!(r[0].2.contains(&s2.id) || r[0].2.contains(&s1.id), "{r:?}");

    // Bot chats work like any chat: fast mode, the subagents switch, files.
    let mut req = match serde_json::from_value::<Request>(json!({
        "method": "update_session",
        "params": {"session_id": s2.id, "fast": true, "subagents": false}
    })) {
        Ok(r) => r,
        Err(e) => panic!("{e}"),
    };
    if let Request::UpdateSession { title, .. } = &mut req {
        *title = Some("Biology prep".into());
    }
    let info: SessionInfo = call(&h, req).await;
    assert!(info.fast && !info.subagents && info.bot_id.as_deref() == Some(bot.id.as_str()));
    use base64::Engine as _;
    h.mock.push(Reply::Text("Got your notes.".into()));
    h.mock.route(REVIEW, Reply::Text("Nothing to save.".into()));
    let _: Value = call(
        &h,
        Request::Send {
            session_id: s2.id.clone(),
            content: vec![ContentPart::text("here are my lecture notes")],
            delivery: Delivery::Auto,
            attachments: vec![Attachment {
                name: "lecture.md".into(),
                mime: "text/markdown".into(),
                data: base64::engine::general_purpose::STANDARD
                    .encode("Krebs cycle: 2 ATP per glucose"),
            }],
        },
    )
    .await;
    h.settle(&s2.id).await;
    let last = h.mock.agent_requests().pop().unwrap().to_string();
    assert!(
        last.contains("Krebs cycle"),
        "the attachment reached the bot"
    );
}

#[tokio::test]
async fn bots_in_a_channel_hand_off_without_looping_forever() {
    let h = harness(vec![], 100_000).await;
    let mk = |name: &str, role: &str| -> Bot {
        serde_json::from_value(json!({
            "id": "", "name": name, "handle": "", "avatar": "", "color": "", "role": role,
            "personality": "", "instructions": "", "permission": "default", "tools": "assistant"
        }))
        .unwrap()
    };
    let tutor: Bot = call(
        &h,
        Request::SaveBot {
            bot: mk("Tutor", "explains"),
        },
    )
    .await;
    let quizzer: Bot = call(
        &h,
        Request::SaveBot {
            bot: mk("Quizzer", "asks questions"),
        },
    )
    .await;
    let room: SessionInfo = call(
        &h,
        Request::CreateChannel {
            title: "Study group".into(),
            members: vec![tutor.id.clone(), quizzer.id.clone()],
            cwd: None,
        },
    )
    .await;
    // They keep handing the floor to each other.
    for i in 0..10 {
        h.mock.push(Reply::Text(if i % 2 == 0 {
            format!("Over to you @quizzer ({i})")
        } else {
            format!("Back to you @tutor ({i})")
        }));
    }
    h.send(&room.id, "@tutor explain osmosis, then let's practice")
        .await;
    h.settle(&room.id).await;
    let authors: Vec<String> = h
        .entries(&room.id)
        .await
        .into_iter()
        .filter_map(|e| match e.body {
            EntryBody::Assistant { author, .. } => author,
            _ => None,
        })
        .collect();
    // Each takes at most two turns after the first: then it's the user's turn.
    assert_eq!(
        authors,
        [&tutor.id, &quizzer.id, &tutor.id, &quizzer.id].map(|s| s.to_string())
    );
    assert_eq!(h.info(&room.id).await.status, SessionStatus::Idle);
    // Each speaker hears the others as "[@handle]: …" and itself as itself.
    let last = h.mock.agent_requests().pop().unwrap();
    assert!(system(&last).starts_with("You are Quizzer"));
    assert!(
        last.to_string().contains("[@tutor]: Back to you @tutor")
            || last.to_string().contains("[@tutor]: Over to you")
    );
}
