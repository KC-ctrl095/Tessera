//! How credentials typed into a chat are saved (`[security] secret_saving`):
//! Passwords & keys at once, kept for that chat until the user answers its
//! card ("Ask me first"), or kept for that chat only ("Don't save"). The
//! model never sees a value in any of them, and an agent's `save_secret`
//! asks first when the user wants to be asked.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::auth::{SecretPlace, SecretUser};
use tessera_proto::*;

const ANTHROPIC: &str = "sk-ant-api03-Zq7d9Kx2Lm4Np6Rs8Tv0Wx3Yz5Ab7Cd9Ef1Gh3Ij5Kl7Mn9Op";
const GITHUB: &str = "ghp_Zq7d9Kx2Lm4Np6Rs8Tv0Wx3Yz5Ab7Cd9Ef1G";
const STAGING: &str = "sk-ant-api03-Different0Key1With2Enough3Characters4Here5";

/// A request from its JSON (so these tests don't list every field).
async fn call(h: &Harness, req: Value) -> Result<Value, ApiError> {
    h.engine
        .handle(serde_json::from_value(req).unwrap(), None)
        .await
}

async fn saving(h: &Harness, mode: &str) {
    let v = call(
        h,
        json!({"method": "set_security", "params": {"secret_saving": mode}}),
    )
    .await
    .unwrap();
    let info: SecurityInfo = serde_json::from_value(v).unwrap();
    assert_eq!(serde_json::to_value(info.secret_saving).unwrap(), mode);
}

fn secrets(h: &Harness) -> Vec<SecretInfo> {
    h.engine.auth().list_secrets()
}

fn kept(h: &Harness, name: &str) -> Option<SecretInfo> {
    secrets(h).into_iter().find(|s| s.name == name)
}

fn notices(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn last_user_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::User { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .unwrap()
}

/// What a command in the chat `chat` gets for `cmd` (an ordinary chat).
fn env_in(h: &Harness, chat: &str, cmd: &str) -> Vec<(String, String)> {
    let user = SecretUser::InChat {
        chat: chat.into(),
        user: Box::new(SecretUser::Chat),
    };
    h.engine
        .auth()
        .secrets_for(cmd, &user, SecretPlace::Server, &[])
        .env
}

/// No value anywhere the model, a client or the database could have it.
fn nowhere(h: &Harness, values: &[&str]) {
    let sent = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    let db = std::fs::read(h.dir.path().join("tessera.db")).unwrap();
    for v in values {
        assert!(!sent.contains(v), "the model saw {v}");
        assert!(
            !db.windows(v.len()).any(|w| w == v.as_bytes()),
            "{v} in the database"
        );
    }
}

/// Sends `text` and answers every approval with `decision` until the run
/// ends; the approvals asked.
async fn run(
    h: &Harness,
    id: &str,
    text: &str,
    decision: ApprovalDecision,
) -> Vec<ApprovalRequest> {
    let mut events = h.engine.subscribe();
    h.send(id, text).await;
    let mut asked = Vec::new();
    let mut started = false;
    let done = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Ok(Event::ApprovalRequested { request, .. }) => {
                    asked.push(request.clone());
                    h.engine
                        .handle(
                            Request::RespondApproval {
                                session_id: id.into(),
                                request_id: request.id,
                                decision,
                            },
                            None,
                        )
                        .await
                        .unwrap();
                }
                Ok(Event::SessionUpdated { session }) if session.id == id => {
                    if session.status != SessionStatus::Idle {
                        started = true;
                    } else if started {
                        return;
                    }
                }
                Ok(_) => {}
                Err(e) => panic!("events: {e}"),
            }
        }
    })
    .await;
    assert!(done.is_ok(), "the run did not finish");
    h.settle(id).await;
    asked
}

#[tokio::test]
async fn by_default_keys_are_saved_at_once() {
    let h = harness(vec![Reply::Text("noted".into())], 100_000).await;
    let info: SecurityInfo =
        serde_json::from_value(call(&h, json!({"method": "get_security"})).await.unwrap()).unwrap();
    assert_eq!(info.secret_saving, SecretSaving::Auto);
    let s = h.session().await;
    h.send(&s.id, &format!("use {GITHUB} please")).await;
    h.settle(&s.id).await;
    let gh = kept(&h, "GITHUB_TOKEN").expect("saved");
    assert_eq!(gh.chat, None, "saved for every chat");
    let entries = h.entries(&s.id).await;
    assert_eq!(last_user_text(&entries), "use $GITHUB_TOKEN please");
    assert!(
        notices(&entries)
            .iter()
            .any(|n| n.starts_with("Saved to Passwords & keys: GITHUB_TOKEN")),
        "{:?}",
        notices(&entries)
    );
    assert_eq!(env_in(&h, "another chat", "x $GITHUB_TOKEN").len(), 1);
    nowhere(&h, &[GITHUB]);
}

#[tokio::test]
async fn ask_me_first_keeps_a_key_for_its_chat_until_the_user_answers() {
    let h = harness(vec![], 100_000).await;
    for _ in 0..6 {
        h.mock.push(Reply::Text("noted".into()));
    }
    saving(&h, "ask").await;
    let s = h.session().await;
    let other = h.session().await;
    let mut events = h.engine.subscribe();
    h.send(
        &s.id,
        &format!("here is my anthropic key {ANTHROPIC} for the deploy"),
    )
    .await;
    h.settle(&s.id).await;

    // Out of the message, kept for this chat alone, and a card asks.
    let entries = h.entries(&s.id).await;
    assert_eq!(
        last_user_text(&entries),
        "here is my anthropic key $ANTHROPIC_API_KEY for the deploy"
    );
    let a = kept(&h, "ANTHROPIC_API_KEY").expect("kept");
    assert_eq!(a.chat.as_deref(), Some(s.id.as_str()));
    let card = notices(&entries);
    assert!(
        card.iter().any(|n| n
            == "Save to Passwords & keys? ANTHROPIC_API_KEY (Anthropic API key) was taken out of your message and is kept for this chat only until you decide; the agent uses it as $ANTHROPIC_API_KEY without seeing the value."),
        "{card:?}"
    );
    // This chat's commands can use it; no other chat can, nor knows it.
    assert_eq!(
        env_in(&h, &s.id, "x $ANTHROPIC_API_KEY"),
        [("ANTHROPIC_API_KEY".to_string(), ANTHROPIC.to_string())]
    );
    assert!(env_in(&h, &other.id, "x $ANTHROPIC_API_KEY").is_empty());
    assert!(
        h.engine
            .auth()
            .secrets_for(
                "x $ANTHROPIC_API_KEY",
                &SecretUser::Chat,
                SecretPlace::Server,
                &[]
            )
            .env
            .is_empty()
    );
    // Its agent is told it may use it (by name).
    let sent = serde_json::to_string(&h.mock.agent_requests()).unwrap();
    assert!(sent.contains("$ANTHROPIC_API_KEY"));
    // Typed in the other chat, it is that chat's own (not "a stored secret").
    h.send(&other.id, &format!("and here {ANTHROPIC}")).await;
    h.settle(&other.id).await;
    let theirs = secrets(&h)
        .into_iter()
        .find(|x| x.chat.as_deref() == Some(other.id.as_str()))
        .expect("kept for the other chat");
    assert_eq!(theirs.name, "ANTHROPIC_API_KEY_2");
    assert_eq!(
        last_user_text(&h.entries(&other.id).await),
        "and here $ANTHROPIC_API_KEY_2"
    );

    // Save: in Passwords & keys for every chat, and the card says so.
    call(
        &h,
        json!({"method": "decide_chat_secret", "params": {"session_id": s.id, "name": "ANTHROPIC_API_KEY", "choice": "save"}}),
    )
    .await
    .unwrap();
    assert_eq!(kept(&h, "ANTHROPIC_API_KEY").unwrap().chat, None);
    assert_eq!(env_in(&h, "a third chat", "x $ANTHROPIC_API_KEY").len(), 1);
    let card = notices(&h.entries(&s.id).await);
    assert!(
        card.iter().any(|n| n
            == "Saved to Passwords & keys: ANTHROPIC_API_KEY (Anthropic API key). It was taken out of your message; the agent uses it as $ANTHROPIC_API_KEY without seeing the value."),
        "{card:?}"
    );
    assert!(
        !card
            .iter()
            .any(|n| n.starts_with("Save to Passwords & keys? "))
    );
    // Clients were told to redraw it.
    let mut redrawn = false;
    while let Ok(e) = events.try_recv() {
        if let Event::EntriesChanged {
            session_id,
            entries,
        } = e
            && session_id == s.id
            && serde_json::to_string(&entries)
                .unwrap()
                .contains("Saved to Passwords & keys: ANTHROPIC_API_KEY")
        {
            redrawn = true;
        }
    }
    assert!(redrawn, "entries_changed with the new card");
    // Answered already: nothing left to decide.
    assert!(
        call(
            &h,
            json!({"method": "decide_chat_secret", "params": {"session_id": s.id, "name": "ANTHROPIC_API_KEY", "choice": "forget"}}),
        )
        .await
        .is_err()
    );
    // Another chat can't answer for this one.
    assert!(
        call(
            &h,
            json!({"method": "decide_chat_secret", "params": {"session_id": s.id, "name": "ANTHROPIC_API_KEY_2", "choice": "save"}}),
        )
        .await
        .is_err()
    );

    // Don't save: it stays this chat's own.
    h.send(&s.id, &format!("the staging one {STAGING}")).await;
    h.settle(&s.id).await;
    call(
        &h,
        json!({"method": "decide_chat_secret", "params": {"session_id": s.id, "name": "ANTHROPIC_API_KEY_3", "choice": "chat_only"}}),
    )
    .await
    .unwrap();
    assert_eq!(
        kept(&h, "ANTHROPIC_API_KEY_3").unwrap().chat.as_deref(),
        Some(s.id.as_str())
    );
    let card = notices(&h.entries(&s.id).await);
    assert!(
        card.iter()
            .any(|n| n
                .starts_with("Kept for this chat only: ANTHROPIC_API_KEY_3 (Anthropic API key).")),
        "{card:?}"
    );

    // Forget (or delete it in Passwords & keys): gone, and the card says so.
    h.send(&s.id, &format!("gh {GITHUB}")).await;
    h.settle(&s.id).await;
    call(
        &h,
        json!({"method": "delete_secret", "params": {"name": "GITHUB_TOKEN"}}),
    )
    .await
    .unwrap();
    assert!(kept(&h, "GITHUB_TOKEN").is_none());
    let card = notices(&h.entries(&s.id).await);
    assert!(
        card.iter().any(|n| n
            == "Forgot GITHUB_TOKEN (GitHub token): it was taken out of your message and isn't kept anywhere."),
        "{card:?}"
    );
    // A kept one keeps its name until it is saved.
    assert!(
        call(
            &h,
            json!({"method": "save_secret", "params": {"name": "ANTHROPIC_API_KEY_3", "kind": "token", "new_name": "STAGING_KEY"}}),
        )
        .await
        .is_err()
    );

    // Deleting the chat deletes what it kept; the saved one stays.
    call(
        &h,
        json!({"method": "delete_session", "params": {"session_id": s.id}}),
    )
    .await
    .unwrap();
    let names: Vec<String> = secrets(&h).into_iter().map(|x| x.name).collect();
    assert_eq!(names, ["ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY_2"]);

    // The model never saw a value, nor did the database keep one.
    nowhere(&h, &[ANTHROPIC, GITHUB, STAGING]);
}

#[tokio::test]
async fn dont_save_keeps_keys_and_codes_for_the_chat_alone() {
    let h = harness(vec![], 100_000).await;
    for _ in 0..4 {
        h.mock.push(Reply::Text("noted".into()));
    }
    saving(&h, "chat_only").await;
    let s = h.session().await;
    let other = h.session().await;
    h.send(
        &s.id,
        &format!("deploy with {GITHUB}; my verification code is 482 913"),
    )
    .await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    assert_eq!(
        last_user_text(&entries),
        "deploy with $GITHUB_TOKEN; my verification code is $ONE_TIME_CODE"
    );
    // No question: one calm line, and nothing in Passwords & keys for other chats.
    let n = notices(&entries);
    assert!(
        n.iter().any(|t| t
            == "Kept for this chat only: GITHUB_TOKEN (GitHub token), ONE_TIME_CODE (one-time code). They were taken out of your message; the agent uses them as $GITHUB_TOKEN, $ONE_TIME_CODE without seeing the value."),
        "{n:?}"
    );
    assert!(
        !n.iter()
            .any(|t| t.starts_with("Save to Passwords & keys? "))
    );
    assert!(
        secrets(&h)
            .iter()
            .all(|x| x.chat.as_deref() == Some(s.id.as_str()))
    );
    assert_eq!(
        env_in(&h, &s.id, "x $ONE_TIME_CODE"),
        [("ONE_TIME_CODE".to_string(), "482913".to_string())]
    );
    // Another chat's code is its own; this chat's next one replaces its old one.
    h.send(&other.id, "the login code is 551204").await;
    h.settle(&other.id).await;
    assert_eq!(
        last_user_text(&h.entries(&other.id).await),
        "the login code is $ONE_TIME_CODE_2"
    );
    h.send(&s.id, "new one: 2FA: 700100").await;
    h.settle(&s.id).await;
    assert_eq!(
        env_in(&h, &s.id, "x $ONE_TIME_CODE"),
        [("ONE_TIME_CODE".to_string(), "700100".to_string())]
    );
    assert!(env_in(&h, &other.id, "x $ONE_TIME_CODE $GITHUB_TOKEN").is_empty());
    assert_eq!(secrets(&h).len(), 3);
    nowhere(&h, &[GITHUB, "482913", "482 913", "551204", "700100"]);
}

#[tokio::test]
async fn the_agents_save_secret_asks_first_when_the_user_wants_to_be_asked() {
    let h = harness(vec![], 100_000).await;
    let password = "Gr33n-Tea!Pot";
    // (A value kept already is replaced when typed again: each login has its own.)
    let (work, old) = ("W0rk-Tea!Pot", "Old-Tea!Pot9");
    let save = |name: &str, password: &str| {
        Reply::Tools(vec![(
            "save_secret".into(),
            json!({"name": name, "username": "ann.k", "password": password, "url": "https://bank.example/login"}),
        )])
    };
    saving(&h, "ask").await;
    let s = h.session().await;
    // Even in auto mode, where nothing else asks.
    call(
        &h,
        json!({"method": "update_session", "params": {"session_id": s.id, "permission": "auto"}}),
    )
    .await
    .unwrap();

    // Denied: not in Passwords & keys, but out of the conversation, this chat's own.
    h.mock.push(save("BANK", password));
    h.mock.push(Reply::Text("Kept here only.".into()));
    let asked = run(
        &h,
        &s.id,
        &format!("my bank login is ann.k / {password}"),
        ApprovalDecision::Deny,
    )
    .await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].tool, "save_secret");
    assert!(
        asked[0]
            .summary
            .contains("Save BANK (a login) to Passwords & keys"),
        "{:?}",
        asked[0]
    );
    let shown = serde_json::to_string(&asked[0]).unwrap();
    assert!(
        !shown.contains(password) && !shown.contains("ann.k"),
        "{shown}"
    );
    let bank = kept(&h, "BANK").expect("kept");
    assert_eq!(bank.chat.as_deref(), Some(s.id.as_str()));
    let entries = h.entries(&s.id).await;
    let results = tool_results(&entries);
    assert!(
        !results[0].1 && results[0].2.contains("Kept for this chat only"),
        "{:?}",
        results[0]
    );
    let stored = serde_json::to_string(&entries).unwrap();
    assert!(!stored.contains(password), "{stored}");
    assert!(
        notices(&entries)
            .iter()
            .any(|n| n.starts_with("Kept for this chat only: BANK (login).")),
        "{:?}",
        notices(&entries)
    );
    assert_eq!(
        env_in(&h, &s.id, "login $BANK_PASSWORD"),
        [("BANK_PASSWORD".to_string(), password.to_string())]
    );
    // Saved later from Passwords & keys: its line in the chat says so.
    call(
        &h,
        json!({"method": "decide_chat_secret", "params": {"session_id": s.id, "name": "BANK", "choice": "save"}}),
    )
    .await
    .unwrap();
    assert_eq!(kept(&h, "BANK").unwrap().chat, None);
    assert!(
        notices(&h.entries(&s.id).await)
            .iter()
            .any(|n| n.starts_with(
                "Saved to Passwords & keys: BANK (login). It was taken out of the conversation;"
            )),
        "{:?}",
        notices(&h.entries(&s.id).await)
    );

    // Allowed: in Passwords & keys.
    h.mock.push(save("BANK_WORK", work));
    h.mock.push(Reply::Text("Saved.".into()));
    let asked = run(
        &h,
        &s.id,
        &format!("and the work one is ann.k / {work} too"),
        ApprovalDecision::Allow,
    )
    .await;
    assert_eq!(asked.len(), 1);
    assert_eq!(kept(&h, "BANK_WORK").unwrap().chat, None);

    // "Don't save": no question, this chat's own.
    saving(&h, "chat_only").await;
    h.mock.push(save("BANK_OLD", old));
    h.mock.push(Reply::Text("Kept.".into()));
    let asked = run(
        &h,
        &s.id,
        &format!("the old one: ann.k / {old}"),
        ApprovalDecision::Deny,
    )
    .await;
    assert!(asked.is_empty(), "{asked:?}");
    assert_eq!(
        kept(&h, "BANK_OLD").unwrap().chat.as_deref(),
        Some(s.id.as_str())
    );
}
