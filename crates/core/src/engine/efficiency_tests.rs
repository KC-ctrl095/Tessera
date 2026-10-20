//! No model calls: stale-date repair, cached-prefix stability and persistence.
use super::*;

#[tokio::test]
async fn environment_updates_are_append_only_and_survive_reload() {
    let (engine, dir, rt) = test_support::engine().await;
    let inner = &engine.0;
    inner
        .append(
            &rt,
            EntryBody::User {
                content: vec![ContentPart::text("inspect the widget")],
                author: None,
            },
        )
        .unwrap();
    let first = inner.prepare_request(&rt).unwrap().request;
    assert!(first.system.ends_with(&crate::prompt::current_environment(
        &rt.st.lock().info.model
    )));
    // A long-lived session crossing midnight. No clock/global environment mutation.
    rt.st.lock().internal.environment = Some("- Date: 2000-01-01\n- Model: fixture/model\n".into());
    let next = inner.prepare_request(&rt).unwrap().request;
    assert_eq!(first.system.as_bytes(), next.system.as_bytes());
    assert_eq!(
        serde_json::to_vec(&first.tools).unwrap(),
        serde_json::to_vec(&next.tools).unwrap()
    );
    assert_eq!(
        format!("{:?}", first.messages),
        format!("{:?}", &next.messages[..first.messages.len()])
    );
    assert_eq!(next.messages.len(), first.messages.len() + 1);
    let Msg::User(parts) = next.messages.last().unwrap() else {
        panic!("missing environment note")
    };
    let note = plain_text(parts);
    assert!(note.contains("[Environment update]"), "{note}");
    assert!(note.contains(&crate::prompt::current_environment(
        &rt.st.lock().info.model
    )));
    let again = inner.prepare_request(&rt).unwrap().request;
    assert_eq!(
        format!("{:?}", next.messages),
        format!("{:?}", again.messages)
    );
    inner.save(&rt);
    let id = rt.st.lock().info.id.clone();
    drop(rt);
    drop(engine);
    let resumed = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let rt = resumed.0.rt(&id).unwrap();
    let request = resumed.0.prepare_request(&rt).unwrap().request;
    assert_eq!(next.system, request.system);
    assert_eq!(
        format!("{:?}", next.messages),
        format!("{:?}", request.messages)
    );
}

#[tokio::test]
async fn utility_work_never_waits_for_its_own_running_turn() {
    let (engine, _dir, rt) = test_support::engine().await;
    let sid = {
        let mut st = rt.st.lock();
        st.running = true;
        st.info.id.clone()
    };
    // Port 1 is a non-answering fixture, so a provider attempt would fail or retry.
    // The rejection must happen before any request (and before waiting for idle).
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        engine
            .0
            .utility_complete(&sid, "extract", "page", &CancellationToken::new()),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("Extraction skipped"));
}
