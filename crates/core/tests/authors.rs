//! Every message says who really wrote it. What a plain chat's agent sends
//! a bot, the task a subagent is given and a scheduled job's instructions
//! are not the user's words: they carry their real author (`chat:<id>`,
//! the bot, `job:<id>`) for the apps to show, and the model that reads them
//! is told who wrote them where that matters.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use tessera_proto::*;

fn bot(name: &str, tools: ToolPreset) -> Bot {
    Bot {
        id: String::new(),
        name: name.into(),
        handle: String::new(),
        avatar: String::new(),
        color: String::new(),
        role: "helper".into(),
        personality: String::new(),
        instructions: String::new(),
        model: None,
        thinking: None,
        permission: PermissionMode::Default,
        tools,
        allowed_tools: vec![],
        cwd: None,
        own_memory: true,
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

async fn save(h: &Harness, b: Bot) -> Bot {
    serde_json::from_value(
        h.engine
            .handle(Request::SaveBot { bot: b }, None)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn titled(h: &Harness, id: &str, title: &str) {
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: id.into(),
                title: Some(title.into()),
                model: None,
                thinking: None,
                permission: None,
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

async fn sessions(h: &Harness) -> Vec<SessionInfo> {
    serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, None)
            .await
            .unwrap(),
    )
    .unwrap()
}

/// The subagent an `agent` call in `entries` started.
fn child_of(entries: &[Entry]) -> String {
    entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                details: Some(d), ..
            } => d["subagents"][0]["session_id"].as_str().map(str::to_string),
            _ => None,
        })
        .expect("a subagent")
}

fn first_user(entries: &[Entry]) -> (String, Option<String>) {
    entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::User { content, author } => Some((plain_text(content), author.clone())),
            _ => None,
        })
        .expect("a user entry")
}

/// What the model was sent as user messages, across every request.
fn user_messages(h: &Harness) -> Vec<String> {
    let text = |c: &serde_json::Value| match c {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    h.mock
        .agent_requests()
        .iter()
        .flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
        .filter(|m| m["role"] == "user")
        .map(|m| text(&m["content"]))
        .collect()
}

/// The request whose system prompt starts with `who`, containing `text`.
fn request_with(h: &Harness, who: &str, text: &str) -> String {
    h.mock
        .agent_requests()
        .into_iter()
        .map(|r| r.to_string())
        .find(|r| r.contains(text) && r.contains(who))
        .unwrap_or_else(|| panic!("no request from {who} with {text}"))
}

#[tokio::test]
async fn a_plain_chats_message_to_a_bot_comes_from_that_chat_not_the_user() {
    let h = harness(vec![], 100_000).await;
    let forge = save(&h, bot("Forge", ToolPreset::Coding)).await;
    let s = h.session().await;
    titled(&h, &s.id, "Server upkeep").await;
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@forge", "message": "Please check the backups.", "wait": true}),
    )]));
    h.mock.route(
        "Please check the backups.",
        Reply::Text("Backups are fine.".into()),
    );
    h.mock
        .push(Reply::Text("Forge says the backups are fine.".into()));
    h.send(&s.id, "ask forge about the backups").await;
    h.settle(&s.id).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("Backups are fine."),
        "{results:?}"
    );

    let dm = sessions(&h)
        .await
        .into_iter()
        .find(|x| x.kind == SessionKind::Channel && x.bot_id.as_deref() == Some(&forge.id))
        .expect("the conversation with Forge");
    assert_eq!(dm.title, "Your chats → @forge");
    // The message is the chat's agent's, never the user's.
    let (text, author) = first_user(&h.entries(&dm.id).await);
    assert_eq!(text, "Please check the backups.");
    assert_eq!(author.as_deref(), Some(format!("chat:{}", s.id).as_str()));
    // Forge is told who is asking.
    let forge_req = request_with(&h, "You are Forge", "Please check the backups.");
    assert!(
        forge_req.contains(
            "[the user's agent in the chat \\\"Server upkeep\\\"]: Please check the backups."
        ),
        "{forge_req}"
    );
}

#[tokio::test]
async fn a_bots_message_to_another_bot_stays_the_bots() {
    let h = harness(vec![], 100_000).await;
    let nova = save(&h, bot("Nova", ToolPreset::Assistant)).await;
    let _forge = save(&h, bot("Forge", ToolPreset::Coding)).await;
    let s: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: Some(nova.id.clone()),
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@forge", "message": "Is the build green?", "wait": true}),
    )]));
    h.mock
        .route("Is the build green?", Reply::Text("Green.".into()));
    h.mock.push(Reply::Text("It's green.".into()));
    h.send(&s.id, "ask forge about the build").await;
    h.settle(&s.id).await;
    let dm = sessions(&h)
        .await
        .into_iter()
        .find(|x| x.kind == SessionKind::Channel)
        .unwrap();
    assert_eq!(dm.title, "@nova ↔ @forge");
    let (_, author) = first_user(&h.entries(&dm.id).await);
    assert_eq!(author.as_deref(), Some(nova.id.as_str()));
    assert!(
        request_with(&h, "You are Forge", "Is the build green?")
            .contains("[@nova]: Is the build green?")
    );
}

#[tokio::test]
async fn a_subagents_task_comes_from_the_agent_that_started_it() {
    let h = harness(vec![], 100_000).await;
    // A plain chat's subagent: its task is the chat's.
    let s = h.session().await;
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait": true, "tasks": [{"name": "counter", "prompt": "SUBTASK count the files"}]}),
    )]));
    h.mock
        .route("SUBTASK count the files", Reply::Text("12 files.".into()));
    h.mock.push(Reply::Text("There are 12 files.".into()));
    h.send(&s.id, "how many files are there?").await;
    h.settle(&s.id).await;
    let child = child_of(&h.entries(&s.id).await);
    let (text, author) = first_user(&h.entries(&child).await);
    assert!(text.starts_with("SUBTASK count the files"), "{text}");
    assert_eq!(author.as_deref(), Some(format!("chat:{}", s.id).as_str()));
    // The subagent reads its task as before: no label in front of it.
    let said = user_messages(&h);
    assert!(
        said.iter()
            .any(|m| m.starts_with("SUBTASK count the files")),
        "the task stays unlabelled: {said:?}"
    );

    // A bot's subagent: its task is the bot's.
    let nova = save(&h, bot("Nova", ToolPreset::Full)).await;
    let b: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: Some(nova.id.clone()),
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    h.mock.push(Reply::Tools(vec![(
        "agent".into(),
        json!({"wait": true, "tasks": [{"name": "reader", "prompt": "BOTTASK read the notes"}]}),
    )]));
    h.mock
        .route("BOTTASK read the notes", Reply::Text("Read them.".into()));
    h.mock.push(Reply::Text("Done.".into()));
    h.send(&b.id, "read the notes").await;
    h.settle(&b.id).await;
    let child = child_of(&h.entries(&b.id).await);
    let (_, author) = first_user(&h.entries(&child).await);
    assert_eq!(author.as_deref(), Some(nova.id.as_str()));
    let said = user_messages(&h);
    assert!(
        said.iter().any(|m| m.starts_with("BOTTASK read the notes")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_scheduled_jobs_instructions_are_the_jobs() {
    let h = harness(vec![Reply::Text("Here is your briefing.".into())], 100_000).await;
    let job: Job = serde_json::from_value(
        h.engine
            .handle(
                Request::SaveJob {
                    job: Job {
                        id: String::new(),
                        name: "Morning briefing".into(),
                        schedule: "0 8 * * *".into(),
                        prompt: "Brief me on today.".into(),
                        cwd: h.work.display().to_string(),
                        model: None,
                        enabled: true,
                        permission: PermissionMode::Auto,
                        bot_id: None,
                        last_run: None,
                        next_run: None,
                        last_session: None,
                        last_status: None,
                        ..Default::default()
                    },
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let info: SessionInfo = serde_json::from_value(
        h.engine
            .handle(Request::RunJob { id: job.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    h.settle(&info.id).await;
    let (text, author) = first_user(&h.entries(&info.id).await);
    assert_eq!(text, "Brief me on today.");
    assert_eq!(author.as_deref(), Some(format!("job:{}", job.id).as_str()));
    // The model reads it as before.
    let said = user_messages(&h);
    assert!(said.iter().any(|m| m == "Brief me on today."), "{said:?}");
}

#[tokio::test]
async fn a_notification_from_a_channel_names_the_bot_speaking() {
    let h = harness(vec![], 100_000).await;
    let nova = save(&h, bot("Nova", ToolPreset::Assistant)).await;
    let room: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateChannel {
                    title: "Household".into(),
                    members: vec![nova.id.clone()],
                    cwd: Some(h.work.display().to_string()),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let (_phone, _) = h.engine.store().add_device("Pixel").unwrap();
    let mut events = h.engine.subscribe();
    h.mock.push(Reply::Tools(vec![(
        "notify".into(),
        // Asked for by the user: an alert (a bot's plain update would go in the digest).
        json!({"title": "Bins", "body": "Put the bins out tonight.", "urgent": true}),
    )]));
    h.mock.push(Reply::Text("Reminder sent.".into()));
    h.send(&room.id, "@nova remind me about the bins").await;
    h.settle(&room.id).await;
    let mut sent = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::Notification { notification } = e {
            sent.push(notification);
        }
    }
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].from, "Nova", "the bot, not the channel's title");
}
