//! A new chat is named as soon as its first turn starts, not when it ends.

mod common;

use std::time::Duration;

use common::*;
use tessera_proto::*;

#[tokio::test]
async fn a_chat_is_named_while_its_first_turn_still_runs() {
    let h = harness(
        vec![Reply::Slow(Duration::from_secs(3), "Done at last.".into())],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "Plan the move of the NAS backups to the new pool")
        .await;
    // Long before the slow answer: first the message itself, then the model's title.
    let mut titles = Vec::new();
    for _ in 0..40 {
        let info = h.info(&s.id).await;
        if titles.last() != Some(&info.title) {
            titles.push(info.title.clone());
        }
        if info.title == "Test Title" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let info = h.info(&s.id).await;
    assert_eq!(info.title, "Test Title", "{titles:?}");
    assert_eq!(
        info.status,
        SessionStatus::Running,
        "named before the turn ends"
    );
    h.settle(&s.id).await;
    assert_eq!(h.info(&s.id).await.title, "Test Title");
}

#[tokio::test]
async fn chats_left_unnamed_by_a_restart_take_their_first_message() {
    let dir;
    let (a, b);
    {
        let h = harness(
            vec![
                Reply::Slow(Duration::from_secs(5), "late".into()),
                Reply::Text("ok".into()),
            ],
            100_000,
        )
        .await;
        let s1 = h.session().await;
        h.send(&s1.id, "Sort the holiday photos by place").await;
        let s2 = h.session().await;
        h.send(&s2.id, "Check the backup logs").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        (a, b) = (s1.id, s2.id);
        dir = h.dir;
        drop(h.engine);
    }
    // As older versions left them: never named; one still running, one stopped by an earlier restart.
    let store = tessera_core::store::Store::open(&dir.path().join("tessera.db")).unwrap();
    for (id, status) in [
        (&a, SessionStatus::Running),
        (&b, SessionStatus::Interrupted),
    ] {
        let (mut info, mut internal) = store.get_session(id).unwrap().unwrap();
        info.title = String::new();
        info.status = status;
        internal.titled = false;
        store.put_session(&info, &internal).unwrap();
    }
    drop(store);
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    let (ia, _) = engine.store().get_session(&a).unwrap().unwrap();
    let (ib, _) = engine.store().get_session(&b).unwrap().unwrap();
    assert_eq!(ia.title, "Sort the holiday photos by place");
    assert_eq!(ia.status, SessionStatus::Interrupted);
    assert_eq!(ib.title, "Check the backup logs");
}
