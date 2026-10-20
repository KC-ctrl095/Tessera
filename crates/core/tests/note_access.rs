//! Which notes agents and bots may read (batch 17): every note, unless the
//! user keeps it from agents. A kept note is readable only in a chat the user
//! brought it into (a `[[Title]]` link, or "Ask about this" naming the note's
//! id) — in that chat only — and by a bot that has it as knowledge. What
//! agents write never widens it. Older apps' "Only bots I add" / "All my
//! bots" map onto it.

mod common;

use common::*;
use serde_json::{Value, json};
use tessera_proto::*;

async fn call<T: serde::de::DeserializeOwned>(h: &Harness, req: Request) -> T {
    serde_json::from_value(h.engine.handle(req, None).await.unwrap()).unwrap()
}

async fn note(h: &Harness, title: &str, content: &str) -> Note {
    call(
        h,
        Request::SaveNote {
            id: None,
            title: title.into(),
            content: content.into(),
            base_version: None,
            session_id: None,
        },
    )
    .await
}

async fn bot(h: &Harness, name: &str, handle: &str) -> Bot {
    bot_messaging(h, name, handle, &[]).await
}

/// A bot that may hand work to the bots in `can` (handles).
async fn bot_messaging(h: &Harness, name: &str, handle: &str, can: &[&str]) -> Bot {
    call(
        h,
        Request::SaveBot {
            bot: serde_json::from_value(json!({
                "id": "", "name": name, "handle": handle, "avatar": "", "color": "",
                "role": "helper", "personality": "", "instructions": "",
                "permission": "default", "tools": "assistant", "own_memory": true,
                "share_user_profile": false, "knowledge": [], "starters": [],
                "can_message": can
            }))
            .unwrap(),
        },
    )
    .await
}

async fn chat_with(h: &Harness, bot: Option<&Bot>) -> SessionInfo {
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

/// The agent in `chat` tries to read `id` (after the user's `message`): what it got.
async fn reads(h: &Harness, chat: &str, message: &str, id: &str) -> (bool, String) {
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "read", "id": id}),
    )]));
    h.mock.push(Reply::Text("Done.".into()));
    h.send(chat, message).await;
    h.settle(chat).await;
    let (_, error, text) = tool_results(&h.entries(chat).await)
        .pop()
        .expect("a tool result");
    (!error, text)
}

/// What `list` shows the agent in `chat`.
async fn listed(h: &Harness, chat: &str) -> String {
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "list"}),
    )]));
    h.mock.push(Reply::Text("Listed.".into()));
    h.send(chat, "What notes can you see?").await;
    h.settle(chat).await;
    tool_results(&h.entries(chat).await).pop().unwrap().2
}

/// Keep `id` from agents (the user's choice, from a device), or not.
async fn keep(h: &Harness, id: &str, keep_from_agents: bool) -> Note {
    call(
        h,
        Request::SetNoteAgents {
            id: id.into(),
            keep_from_agents,
        },
    )
    .await
}

#[tokio::test]
async fn every_agent_and_bot_reads_every_note_by_default() {
    let h = harness(vec![], 100_000).await;
    let trip = note(
        &h,
        "Trip plan",
        "Train to Lyon on the 12th, back on the 16th.",
    )
    .await;
    assert!(!trip.keep_from_agents && trip.all_bots, "{trip:?}");
    // A bot with its own memory, with nothing given to it.
    let ann = bot(&h, "Ann", "ann").await;
    let s1 = chat_with(&h, Some(&ann)).await;
    let (ok, text) = reads(&h, &s1.id, "When do I leave?", &trip.id).await;
    assert!(ok && text.contains("Train to Lyon"), "{text}");
    assert!(listed(&h, &s1.id).await.contains("Trip plan"));
    // A chat without a bot too.
    let plain = chat_with(&h, None).await;
    let (ok, _) = reads(&h, &plain.id, "Read my trip plan.", &trip.id).await;
    assert!(ok);
    // A note an agent made in one bot's chat is readable by another bot.
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "create", "title": "Shopping", "content": "Milk, bread."}),
    )]));
    h.mock.push(Reply::Text("Saved.".into()));
    h.send(&s1.id, "Write my shopping list.").await;
    h.settle(&s1.id).await;
    let shopping = h
        .engine
        .handle(Request::ListNotes, None)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["title"] == "Shopping")
        .cloned()
        .unwrap();
    assert_eq!(shopping["keep_from_agents"], Value::Bool(false));
    let bob = bot(&h, "Bob", "bob").await;
    let b1 = chat_with(&h, Some(&bob)).await;
    let (ok, text) = reads(
        &h,
        &b1.id,
        "What's on it?",
        shopping["id"].as_str().unwrap(),
    )
    .await;
    assert!(ok && text.contains("Milk"), "{text}");
}

#[tokio::test]
async fn a_note_kept_from_agents_is_readable_only_where_the_user_brings_it() {
    let h = harness(vec![], 100_000).await;
    let trip = note(
        &h,
        "Trip plan",
        "Train to Lyon on the 12th, back on the 16th.",
    )
    .await;
    let diary = note(&h, "Diary", "Dear diary: a quiet day.").await;
    let taxes = note(&h, "Tax return", "Refund details.").await;
    let mut events = h.engine.subscribe();
    for n in [&trip, &diary, &taxes] {
        let kept = keep(&h, &n.id, true).await;
        assert!(kept.keep_from_agents && !kept.all_bots);
        // Not a new version of its text.
        assert_eq!(
            (kept.version, kept.content.as_str()),
            (n.version, n.content.as_str())
        );
    }
    // Every device hears of it.
    let heard = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(Event::NoteUpdated { note }) = events.recv().await
                && note.id == trip.id
            {
                return note.keep_from_agents && !note.all_bots;
            }
        }
    })
    .await
    .unwrap();
    assert!(heard);
    let ann = bot(&h, "Ann", "ann").await;
    let s1 = chat_with(&h, Some(&ann)).await;

    // Not listed, not readable.
    let (ok, _) = reads(&h, &s1.id, "Hi Ann.", &trip.id).await;
    assert!(!ok, "a note kept from agents isn't readable");
    assert!(!listed(&h, &s1.id).await.contains("Trip plan"));
    // "Ask about this" puts the note's title and id in the message box: sent, it is readable here.
    let (ok, text) = reads(
        &h,
        &s1.id,
        &format!("About the “Trip plan” note ({}): when do I leave?", trip.id),
        &trip.id,
    )
    .await;
    assert!(ok && text.contains("Train to Lyon"), "{text}");
    // A [[link]] in the user's message, by title (any case).
    let (ok, text) = reads(&h, &s1.id, "And what does [[diary]] say?", &diary.id).await;
    assert!(ok && text.contains("quiet day"), "{text}");
    // Only those: another kept note stays out, and `list` shows just the ones brought in.
    let (ok, _) = reads(&h, &s1.id, "Anything else?", &taxes.id).await;
    assert!(!ok);
    let list = listed(&h, &s1.id).await;
    assert!(
        list.contains("Trip plan") && list.contains("Diary") && !list.contains("Tax return"),
        "{list}"
    );

    // In that chat only: not in another chat of Ann's, nor Bob's, nor a chat without a bot.
    let s2 = chat_with(&h, Some(&ann)).await;
    let (ok, _) = reads(&h, &s2.id, "Hello again.", &trip.id).await;
    assert!(!ok, "a grant is for the chat it was given in");
    let bob = bot(&h, "Bob", "bob").await;
    let b1 = chat_with(&h, Some(&bob)).await;
    let (ok, _) = reads(&h, &b1.id, "Hi Bob.", &trip.id).await;
    assert!(!ok, "another bot doesn't see it");
    let plain = chat_with(&h, None).await;
    let (ok, _) = reads(&h, &plain.id, "Read the tax note.", &taxes.id).await;
    assert!(!ok, "a chat without a bot doesn't see it either");
    let (ok, _) = reads(&h, &plain.id, "Read it: [[Tax return]]", &taxes.id).await;
    assert!(ok, "…until the user links it there");

    // A bot that has it as knowledge reads it anywhere.
    let mut b = bob.clone();
    b.knowledge = vec![taxes.id.clone()];
    let _: Bot = call(&h, Request::SaveBot { bot: b }).await;
    let b2 = chat_with(&h, Some(&bob)).await;
    let (ok, text) = reads(&h, &b2.id, "Refund?", &taxes.id).await;
    assert!(ok && text.contains("Refund"), "{text}");

    // An agent changing a kept note it was given keeps the user's choice.
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "update", "id": diary.id, "append": "\nEvening: rain."}),
    )]));
    h.mock.push(Reply::Text("Added.".into()));
    h.send(&s1.id, "Add that it rained to [[Diary]].").await;
    h.settle(&s1.id).await;
    let now: Note = call(
        &h,
        Request::GetNote {
            id: diary.id.clone(),
        },
    )
    .await;
    assert!(
        now.content.contains("rain") && now.keep_from_agents,
        "{now:?}"
    );
    // …and one it wasn't given can't change it.
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "update", "id": diary.id, "append": "\nOverwritten."}),
    )]));
    h.mock.push(Reply::Text("Tried.".into()));
    h.send(&s2.id, "Change the diary.").await;
    h.settle(&s2.id).await;
    let now: Note = call(
        &h,
        Request::GetNote {
            id: diary.id.clone(),
        },
    )
    .await;
    assert!(!now.content.contains("Overwritten"), "{now:?}");

    // Back to readable by every agent.
    let n = keep(&h, &trip.id, false).await;
    assert!(!n.keep_from_agents && n.all_bots);
    let (ok, _) = reads(&h, &s2.id, "Trip?", &trip.id).await;
    assert!(ok);
}

#[tokio::test]
async fn agents_never_widen_what_an_agent_reads() {
    let h = harness(vec![], 100_000).await;
    let trip = note(&h, "Trip plan", "Train to Lyon.").await;
    keep(&h, &trip.id, true).await;
    let ann = bot(&h, "Ann", "ann").await;
    let s1 = chat_with(&h, Some(&ann)).await;
    // The bot itself writes the link and the id, in its reply and in a note of its own.
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "create", "title": "Links", "content": format!("See [[Trip plan]] ({})", trip.id)}),
    )]));
    h.mock.push(Reply::Text(format!(
        "Let me look at [[Trip plan]] ({}).",
        trip.id
    )));
    h.send(&s1.id, "Hello.").await;
    h.settle(&s1.id).await;
    let (ok, _) = reads(&h, &s1.id, "Go on.", &trip.id).await;
    assert!(!ok, "what the agent wrote gave it nothing");
    // Its own note saves never change who may read a note.
    let links = h
        .engine
        .handle(Request::ListNotes, None)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["title"] == "Links")
        .cloned()
        .unwrap();
    assert_eq!(links["keep_from_agents"], Value::Bool(false));
    let t: Note = call(
        &h,
        Request::GetNote {
            id: trip.id.clone(),
        },
    )
    .await;
    assert!(t.keep_from_agents);
}

#[tokio::test]
async fn a_bot_handing_work_on_passes_no_note_along() {
    let h = harness(vec![], 100_000).await;
    let trip = note(&h, "Trip plan", "Train to Lyon.").await;
    keep(&h, &trip.id, true).await;
    let ann = bot_messaging(&h, "Ann", "ann", &["bob"]).await;
    bot(&h, "Bob", "bob").await;
    let s1 = chat_with(&h, Some(&ann)).await;
    // The user brings the note to Ann; Ann asks Bob about it, naming it as the user did.
    let ask = format!("Please look at [[Trip plan]] ({}).", trip.id);
    h.mock.push(Reply::Tools(vec![(
        "message_bot".into(),
        json!({"bot": "@bob", "message": ask, "wait": true}),
    )]));
    // Bob tries to read it, then answers; Ann wraps up.
    h.mock.push(Reply::Tools(vec![(
        "note".into(),
        json!({"action": "read", "id": trip.id}),
    )]));
    h.mock.push(Reply::Text("I can't open that note.".into()));
    h.mock.push(Reply::Text("Bob can't see it.".into()));
    h.send(&s1.id, "Ask Bob about [[Trip plan]].").await;
    h.settle(&s1.id).await;
    let sessions: Vec<SessionInfo> = call(&h, Request::ListSessions { archived: false }).await;
    let dm = sessions
        .iter()
        .find(|x| x.kind == SessionKind::Channel)
        .expect("Ann's message to Bob");
    h.settle(&dm.id).await;
    let read = tool_results(&h.entries(&dm.id).await)
        .into_iter()
        .find(|(name, ..)| name == "note")
        .expect("Bob's read");
    assert!(read.1, "Bob was given nothing by Ann's message: {}", read.2);
    // Ann herself reads it: the user brought it into her chat.
    let (ok, _) = reads(&h, &s1.id, "Read it yourself.", &trip.id).await;
    assert!(ok);
}

#[tokio::test]
async fn older_apps_and_older_notes_keep_working() {
    let h = harness(vec![], 100_000).await;
    let recipes = note(&h, "Recipes", "Pancakes: flour, milk, eggs.").await;
    let ann = bot(&h, "Ann", "ann").await;
    let s1 = chat_with(&h, Some(&ann)).await;
    // An older app's "Only bots I add" keeps it from agents; "All my bots" opens it again.
    let n: Note = call(
        &h,
        Request::SetNoteBots {
            id: recipes.id.clone(),
            all_bots: false,
        },
    )
    .await;
    assert!(n.keep_from_agents && !n.all_bots);
    let (ok, _) = reads(&h, &s1.id, "Pancakes?", &recipes.id).await;
    assert!(!ok);
    let n: Note = call(
        &h,
        Request::SetNoteBots {
            id: recipes.id.clone(),
            all_bots: true,
        },
    )
    .await;
    assert!(!n.keep_from_agents && n.all_bots);
    let (ok, text) = reads(&h, &s1.id, "Pancakes now?", &recipes.id).await;
    assert!(ok && text.contains("Pancakes"), "{text}");
    // A note as an older server stored it (no `keep_from_agents`, "only bots it is given to"):
    // every agent reads it now, and older apps see it as open to all bots.
    let old: Note = serde_json::from_value(json!({
        "id": "nt_old", "title": "Old", "content": "From before.", "version": 3,
        "created_at": 1, "updated_at": 2, "by_user": true, "all_bots": false
    }))
    .unwrap();
    assert!(!old.keep_from_agents);
    let info: Vec<NoteInfo> = call(&h, Request::ListNotes).await;
    assert!(info.iter().all(|i| i.all_bots == !i.keep_from_agents));
}
