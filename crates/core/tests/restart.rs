//! A planned restart (an update) continues the chats it cut off, whatever
//! `resume_on_restart` says; a crash leaves interactive chats waiting.

mod common;

use std::time::Duration;

use common::*;
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

#[tokio::test]
async fn chats_cut_off_by_a_planned_restart_continue_by_themselves() {
    let h = harness(
        vec![
            Reply::Slow(Duration::from_secs(4), "late".into()),
            Reply::Text("Picked up again and finished.".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    assert_eq!(s.permission, PermissionMode::Default, "an interactive chat");
    h.send(&s.id, "Reorganise the photo library").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.engine.shutdown().await;
    let file = Paths::at(h.dir.path())
        .data_dir
        .join("resume-after-restart.json");
    let listed = std::fs::read_to_string(&file).unwrap();
    assert!(listed.contains(&s.id), "{listed}");

    let engine = Engine::open(Paths::at(h.dir.path()), EngineOptions { scheduler: true })
        .await
        .unwrap();
    assert!(!file.exists(), "read once, then gone");
    let mut done = false;
    for _ in 0..200 {
        let v = engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap();
        let snap: SessionSnapshot = serde_json::from_value(v).unwrap();
        let resumed_note = snap.entries.iter().any(|e| {
            matches!(&e.body, EntryBody::Notice { text, .. } if text.contains("restarted (an update)"))
        });
        let answered = snap.entries.iter().any(|e| {
            matches!(&e.body, EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p, AssistantPart::Text { text } if text.contains("Picked up again"))))
        });
        if resumed_note && answered && snap.info.status == SessionStatus::Idle {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        done,
        "the chat continued by itself after the planned restart"
    );
}
