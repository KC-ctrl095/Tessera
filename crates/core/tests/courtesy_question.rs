//! A courtesy question at the end of an answer ("Want me to also add
//! tests?") doesn't skip the check of changed code; a question the work waits
//! for does.

mod common;

use common::*;
use serde_json::json;
use tessera_proto::*;

fn harness_notes(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a == "harness" => Some(plain_text(content)),
            _ => None,
        })
        .collect()
}

async fn changed_then_asked(closing: &str) -> (Vec<String>, usize) {
    // Ruby: no syntax checker here, so only a command could check it.
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "write".into(),
                json!({"path": "app.rb", "content": "puts 'hi'\n"}),
            )]),
            Reply::Text(closing.into()),
            // The note's extra step, when it comes.
            Reply::Text("No check applies: there is no Ruby here to run it.".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "write app.rb that prints hi").await;
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    (harness_notes(&entries), h.mock.agent_requests().len())
}

#[tokio::test]
async fn an_offer_at_the_end_still_gets_the_check_note() {
    for closing in [
        "Done: app.rb prints hi. Want me to also add tests?",
        "Done: app.rb prints hi.\n\nWould you like me to add a README for it?",
        "Done: app.rb prints hi. Does that sound good?",
        "Done: app.rb prints hi.\n\n**Is that what you had in mind?**",
        "Done: app.rb prints hi. Want me to add tests or docs for it?",
        "Done. Want me to add tests for the delete button?",
        "Done. Do you want me to document the overwrite option?",
        "Done. Would you like me to add a confirm dialog test?",
        "Done. Want me to first add tests for the delete button?",
        "Done. Do you want me to permanently document the overwrite option?",
        "app.rb ist fertig. Soll ich noch etwas ändern?",
    ] {
        let (notes, requests) = changed_then_asked(closing).await;
        assert_eq!(notes.len(), 1, "{closing}: {notes:?}");
        assert!(
            notes[0].starts_with("[Check] You changed app.rb"),
            "{}",
            notes[0]
        );
        assert_eq!(requests, 3, "{closing}");
    }
}

#[tokio::test]
async fn a_question_the_work_waits_for_still_skips_it() {
    for closing in [
        "I wrote app.rb. Which Ruby version should it target?",
        "Done. Should I overwrite the old script or keep both?",
        "I wrote app.rb. Do you want me to delete or overwrite the old script?",
        "I wrote app.rb. Do you want me to permanently delete the old config?",
        "I wrote app.rb. Do you want me to first overwrite the existing config?",
        "I wrote app.rb. Want me to just go ahead and permanently delete the old config?",
        "I wrote app.rb. Can I go ahead and restart the production server?",
        "I wrote app.rb. Do you want me to go ahead and send the email?",
        "Done. Should I go ahead with that change?",
    ] {
        let (notes, requests) = changed_then_asked(closing).await;
        assert!(notes.is_empty(), "{closing}: {notes:?}");
        assert_eq!(requests, 2, "{closing}");
    }
}
