// Included by store::tests so the canonical SessionInfo fixture is shared.
#[test]
fn session_pages_are_bounded_keyset_metadata_and_legacy_stays_full() {
    let s = Store::in_memory().unwrap();
    let internal = SessionInternal::default();
    for n in 0..235 {
        let mut i = info(&format!("chat-{n:04}"));
        // Equal times exercise the id tie-break, not offset pagination.
        i.updated_at = 10;
        i.pinned = n == 0;
        i.task = Some(TaskLedger {
            goal: "x".repeat(10_000),
            ..Default::default()
        });
        i.usage.input = 123;
        s.put_session(&i, &internal).unwrap();
    }
    for (id, kind) in [("job", SessionKind::Job), ("helper", SessionKind::Subagent)] {
        let mut i = info(id);
        i.kind = kind;
        i.updated_at = 20;
        s.put_session(&i, &internal).unwrap();
    }
    let first = s
        .list_session_page(false, 100, None, &["job".into(), "helper".into()])
        .unwrap();
    assert_eq!(first.sessions.len(), 103); // 100 + old pin + two open tiles
    assert!(first
        .sessions
        .iter()
        .any(|i| i.id == "chat-0000" && i.pinned));
    assert!(first.sessions.iter().all(|i| i.task.is_none()));
    assert_eq!(first.sessions[0].id, "chat-0234");
    assert_eq!(first.sessions[0].usage.input, 123);
    let mut cursor = first.next_cursor;
    let mut ids: std::collections::BTreeSet<_> = first.sessions.into_iter().map(|i| i.id).collect();
    while let Some(before) = cursor {
        let page = s.list_session_page(false, 100, Some(&before), &[]).unwrap();
        assert!(page.sessions.len() <= 100);
        assert!(page
            .sessions
            .iter()
            .all(|i| i.kind == SessionKind::Chat && i.task.is_none()));
        for i in page.sessions {
            ids.insert(i.id);
        }
        cursor = page.next_cursor;
    }
    assert_eq!(ids.len(), 237);
    let legacy = s.list_sessions(false).unwrap();
    assert_eq!(legacy.len(), 237);
    assert_eq!(
        legacy
            .iter()
            .find(|i| i.id == "chat-0001")
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .goal
            .len(),
        10_000
    );
    // Clamp on both sides and keep pinned as an extra, not a cursor boundary.
    assert_eq!(
        s.list_session_page(false, 0, None, &[])
            .unwrap()
            .sessions
            .len(),
        2
    );
    assert_eq!(
        s.list_session_page(false, u32::MAX, None, &[])
            .unwrap()
            .sessions
            .len(),
        201
    );
}

#[test]
fn session_summary_migration_writes_archive_and_private_are_safe() {
    // Simulate an older store, without projection columns or triggers.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let mut old = info("old");
    old.task = Some(TaskLedger {
        goal: "kept in full".into(),
        ..Default::default()
    });
    conn.execute(
        "INSERT INTO sessions(id,info,internal,updated_at) VALUES(?1,?2,'',1)",
        params![old.id, serde_json::to_string(&old).unwrap()],
    )
    .unwrap();
    let s = Store::init(conn).unwrap();
    assert!(
        s.list_session_page(false, 10, None, &[]).unwrap().sessions[0]
            .task
            .is_none()
    );
    assert!(s.get_session("old").unwrap().unwrap().0.task.is_some());
    old.title = "changed".into();
    old.archived = true;
    let internal = SessionInternal {
        revision: 2,
        ..Default::default()
    };
    s.update_session(&old, &internal, 2).unwrap();
    assert!(s
        .list_session_page(false, 10, None, &[])
        .unwrap()
        .sessions
        .is_empty());
    let page = s.list_session_page(true, 10, None, &[]).unwrap();
    assert_eq!(page.sessions[0].title, "changed");
    assert_eq!(page.sessions[0].revision, 2);
    assert!(page.sessions[0].archived);
    let mut private = info("private");
    private.private = true;
    s.put_session(&private, &internal).unwrap();
    // Private metadata has no persistent projection. The engine filters the
    // in-memory summaries for the authorized device.
    assert_eq!(
        s.conn
            .lock()
            .query_row(
                "SELECT count(*) FROM sessions WHERE id='private'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        s.list_session_page(false, 10, None, &[]).unwrap().sessions[0].id,
        "private"
    );
    s.delete_session("old").unwrap();
    assert!(s
        .list_session_page(true, 10, None, &[])
        .unwrap()
        .sessions
        .is_empty());
}
