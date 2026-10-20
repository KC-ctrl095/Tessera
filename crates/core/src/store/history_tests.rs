#[test]
fn history_pages_reconstruct_ordered_history_across_appends_and_rewinds() {
    for private in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("history.db")).unwrap();
        let mut session = info("pages");
        session.private = private;
        s.put_session(&session, &SessionInternal::default())
            .unwrap();
        for n in 0..513 {
            s.append("pages", said(&format!("entry-{n}"))).unwrap();
        }
        let first = s.entries_page("pages", None, 120).unwrap();
        assert_eq!(
            first.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            (394..=513).collect::<Vec<_>>()
        );
        s.append("pages", said("new while paging")).unwrap();
        let mut all = first.entries;
        let mut before = first.before;
        while before.is_some() {
            let page = s.entries_page("pages", before, 120).unwrap();
            assert_eq!(page.revision, 0);
            before = page.before;
            all.splice(..0, page.entries);
        }
        assert_eq!(
            all.iter().map(|e| e.seq).collect::<Vec<_>>(),
            (1..=513).collect::<Vec<_>>()
        );
        assert_eq!(
            texts(&all),
            (0..513).map(|n| format!("entry-{n}")).collect::<Vec<_>>()
        );
        assert!(
            s.entries_page("pages", Some(0), 120)
                .unwrap()
                .entries
                .is_empty()
        );
        s.truncate_entries("pages", 12).unwrap();
        s.append("pages", said("replacement")).unwrap();
        let page = s.entries_page("pages", Some(394), 120).unwrap();
        assert_eq!(page.revision, 1);
        assert!(page.before.is_none());
        assert_eq!(page.entries.len(), 13);
        assert_eq!(texts(&page.entries).last().unwrap(), "replacement");
    }
}

#[test]
fn history_pages_bound_bytes_but_always_advance_past_one_large_entry() {
    for private in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("history.db")).unwrap();
        let mut session = info("large-pages");
        session.private = private;
        s.put_session(&session, &SessionInternal::default())
            .unwrap();
        s.append("large-pages", said("first")).unwrap();
        s.append("large-pages", said(&"x".repeat(600_000))).unwrap();
        s.append("large-pages", said("last")).unwrap();
        let last = s.entries_page("large-pages", None, 120).unwrap();
        assert_eq!(texts(&last.entries), ["last"]);
        assert_eq!(last.before, Some(3));
        let large = s.entries_page("large-pages", last.before, 120).unwrap();
        assert_eq!(large.entries.len(), 1);
        assert_eq!(large.before, Some(2));
        assert_eq!(
            texts(
                &s.entries_page("large-pages", large.before, 120)
                    .unwrap()
                    .entries
            ),
            ["first"]
        );
    }
}

fn checkpoint(first_kept_seq: u64) -> EntryBody {
    EntryBody::Compaction {
        external: false,
        summary: "Checkpoint fixture".into(),
        first_kept_seq,
        tokens_before: 500,
        tokens_after: 50,
    }
}

#[test]
fn context_history_cache_preserves_views_and_follows_every_history_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let s = Store::open(&path).unwrap();
    s.put_session(&info("context"), &SessionInternal::default())
        .unwrap();
    for n in 0..30 {
        s.append("context", said(&format!("entry-{n}"))).unwrap();
    }
    s.append("context", checkpoint(21)).unwrap();
    let context = s.context_history("context").unwrap();
    assert_eq!(context.first().unwrap().seq, 21);
    assert_eq!(context.last().unwrap().seq, 31);
    if history_cache_budget() > 0 {
        assert!(std::sync::Arc::ptr_eq(
            &context,
            &s.context_history("context").unwrap()
        ));
    }
    let full = s.entries("context", 0).unwrap();
    let view = |entries: &[Entry]| {
        format!(
            "{:?}",
            crate::context::build_view(entries, &SessionInternal::default(), None).messages
        )
    };
    assert_eq!(view(&context), view(&full));
    assert_eq!(
        full.len(),
        31,
        "display reads never mistake a context tail for complete history"
    );
    s.append("context", said("appended")).unwrap();
    assert_eq!(
        texts(&s.context_history("context").unwrap())
            .last()
            .unwrap(),
        "appended"
    );
    s.append("context", checkpoint(29)).unwrap();
    // A full history may already be cached; either form must produce the same context.
    assert_eq!(
        view(&s.context_history("context").unwrap()),
        view(&s.entries("context", 0).unwrap())
    );
    s.replace_entry(
        "context",
        &Entry {
            seq: 29,
            ts: 1,
            body: said("rewritten"),
        },
    )
    .unwrap();
    assert_eq!(
        texts(&s.context_history("context").unwrap())[0],
        "rewritten"
    );
    s.truncate_entries("context", 10).unwrap();
    assert_eq!(
        s.context_history("context").unwrap().len(),
        10,
        "rewinding past checkpoints restores the original region"
    );
    let other = Store::open(&path).unwrap();
    other.append("context", checkpoint(5)).unwrap();
    assert_eq!(
        s.context_history("context").unwrap().first().unwrap().seq,
        5
    );
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .context_history("context")
            .unwrap()
            .first()
            .unwrap()
            .seq,
        5,
        "restart rebuilds the cache from SQLite"
    );
    // Moving the kept boundary backward must still include earlier entries.
    s.append("context", checkpoint(2)).unwrap();
    assert_eq!(
        s.context_history("context").unwrap().first().unwrap().seq,
        2
    );
    s.delete_session("context").unwrap();
    assert!(s.context_history("context").unwrap().is_empty());
}

#[test]
fn private_context_history_preserves_the_same_checkpoint_view() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("private-context.db")).unwrap();
    let mut session = info("private-context");
    session.private = true;
    s.put_session(&session, &SessionInternal::default())
        .unwrap();
    for n in 0..30 {
        s.append("private-context", said(&format!("entry-{n}")))
            .unwrap();
    }
    s.append("private-context", checkpoint(21)).unwrap();
    let context = s.context_history("private-context").unwrap();
    assert_eq!(context.first().unwrap().seq, 21);
    assert_eq!(context.last().unwrap().seq, 31);
    let full = s.entries("private-context", 0).unwrap();
    let view = |entries: &[Entry]| {
        format!(
            "{:?}",
            crate::context::build_view(entries, &SessionInternal::default(), None).messages
        )
    };
    assert_eq!(view(&context), view(&full));
    assert_eq!(full.len(), 31);
}

#[test]
fn context_tail_fits_the_cache_even_when_the_full_history_does_not() {
    if history_cache_budget() == 0 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("history.db")).unwrap();
    s.put_session(&info("huge-context"), &SessionInternal::default())
        .unwrap();
    let text = "x".repeat(history_cache_budget() / 2 + 1);
    for _ in 0..3 {
        s.append("huge-context", said(&text)).unwrap();
    }
    s.append("huge-context", checkpoint(4)).unwrap();
    let first = s.context_history("huge-context").unwrap();
    assert_eq!(first.len(), 1);
    let all = s.history("huge-context").unwrap();
    assert_eq!(all.len(), 4);
    assert!(
        std::sync::Arc::ptr_eq(&first, &s.context_history("huge-context").unwrap()),
        "an uncachable recall must not evict the compacted model region"
    );
    assert!(s.history_cache_stats().1 < history_cache_budget());
}

#[test]
fn external_compaction_notices_never_cut_native_model_history() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("external-context.db")).unwrap();
    s.put_session(&info("external-context"), &SessionInternal::default())
        .unwrap();
    for n in 0..30 {
        s.append("external-context", said(&format!("entry-{n}")))
            .unwrap();
    }
    let external = || EntryBody::Compaction {
        external: true,
        summary: "External notice".into(),
        first_kept_seq: 30,
        tokens_before: 500,
        tokens_after: 50,
    };
    s.append("external-context", external()).unwrap();
    assert_eq!(s.context_history("external-context").unwrap().len(), 31);
    s.append("external-context", checkpoint(21)).unwrap();
    s.append("external-context", external()).unwrap();
    s.histories.lock().forget("external-context");
    let context = s.context_history("external-context").unwrap();
    let full = s.entries("external-context", 0).unwrap();
    assert_eq!(context.first().unwrap().seq, 21);
    let view = |entries: &[Entry]| {
        format!(
            "{:?}",
            crate::context::build_view(entries, &SessionInternal::default(), None).messages
        )
    };
    assert_eq!(view(&context), view(&full));
}
