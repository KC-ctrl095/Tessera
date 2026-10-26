mod common;
use common::harness;
use tessera_proto::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paged_lists_negotiate_preserve_legacy_and_filter_private_extras() {
    let h = harness(vec![], 128_000).await;
    let hello = h
        .engine
        .handle(
            Request::Hello {
                client: "test".into(),
                version: "test".into(),
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(hello["session_list_pages"], true);
    let mut ids = Vec::new();
    for n in 0..4 {
        let v = h
            .engine
            .handle(
                Request::CreateSession {
                    cwd: None,
                    model: None,
                    title: Some(format!("chat-{n}")),
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap();
        ids.push(v["id"].as_str().unwrap().to_string());
    }
    h.engine
        .handle(
            Request::SetGoal {
                session_id: ids[0].clone(),
                goal: "full ledger must survive".into(),
            },
            None,
        )
        .await
        .unwrap();
    let private = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: None,
                model: None,
                title: Some("Private".into()),
                bot_id: None,
                private: true,
                lean: false,
            },
            Some("phone"),
        )
        .await
        .unwrap();
    let private_id = private["id"].as_str().unwrap().to_string();
    let req = || Request::ListSessionPage {
        archived: false,
        limit: Some(1),
        before: None,
        include_ids: vec![private_id.clone(), ids[0].clone()],
    };
    let owner: SessionListPage =
        serde_json::from_value(h.engine.handle(req(), Some("phone")).await.unwrap()).unwrap();
    assert!(owner.sessions.iter().any(|i| i.id == private_id));
    assert!(
        owner
            .sessions
            .iter()
            .any(|i| i.id == ids[0] && i.task.is_none())
    );
    let other: SessionListPage =
        serde_json::from_value(h.engine.handle(req(), Some("desktop")).await.unwrap()).unwrap();
    assert!(!other.sessions.iter().any(|i| i.id == private_id));
    let local: SessionListPage =
        serde_json::from_value(h.engine.handle(req(), None).await.unwrap()).unwrap();
    assert!(local.sessions.iter().any(|i| i.id == private_id)); // None is the server itself
    let legacy: Vec<SessionInfo> = serde_json::from_value(
        h.engine
            .handle(Request::ListSessions { archived: false }, Some("desktop"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(legacy.len(), 4);
    assert_eq!(
        legacy
            .iter()
            .find(|i| i.id == ids[0])
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .goal,
        "full ledger must survive"
    );
    let mut before = owner.next_cursor;
    let mut found: std::collections::BTreeSet<_> = owner
        .sessions
        .into_iter()
        .filter(|i| !i.private)
        .map(|i| i.id)
        .collect();
    while let Some(cursor) = before {
        let req = Request::ListSessionPage {
            archived: false,
            limit: Some(1),
            before: Some(cursor),
            include_ids: vec![],
        };
        let page: SessionListPage =
            serde_json::from_value(h.engine.handle(req, Some("desktop")).await.unwrap()).unwrap();
        assert_eq!(page.sessions.len(), 1);
        for i in page.sessions {
            found.insert(i.id);
        }
        before = page.next_cursor;
    }
    assert_eq!(found.len(), 4);
    // Servers before this feature omit the capability. Their hello remains
    // readable and selects the unchanged full ListSessions path in the UI.
    let mut old = hello;
    old.as_object_mut().unwrap().remove("session_list_pages");
    assert!(
        !serde_json::from_value::<ServerInfo>(old)
            .unwrap()
            .session_list_pages
    );
    // Old request payloads need no additional fields.
    let old_request: Request =
        serde_json::from_str(r#"{"method":"list_sessions","params":{"archived":false}}"#).unwrap();
    assert!(matches!(
        old_request,
        Request::ListSessions { archived: false }
    ));
}
