// Included in notify's test module so the real policy and dispatch are exercised.
#[tokio::test]
async fn notification_policy_presence_is_per_device_and_expires() {
    let (e, _dir, rt) = super::super::test_support::engine().await;
    let session = rt.st.lock().info.id.clone();
    e.0.notification_presence.lock().insert(
        "laptop".into(),
        (std::time::Instant::now(), vec![session.clone()]),
    );
    let n =
        e.0.send_notification(NewNotification {
            title: "Waiting for your approval".into(),
            kind: "approval".into(),
            session_id: Some(session.clone()),
            ..Default::default()
        })
        .unwrap();
    assert!(!n.quiet);
    assert!(!e.0.notification_delivery(&n, Some("laptop")));
    assert!(
        e.0.notification_delivery(&n, Some("phone")),
        "desktop tile cannot steal a phone page"
    );
    // A reply the user is watching on the laptop needs no phone alert.
    let reply =
        e.0.send_notification(NewNotification {
            title: "A new reply".into(),
            kind: "finished".into(),
            session_id: Some(session.clone()),
            ..Default::default()
        })
        .unwrap();
    assert!(reply.quiet && reply.level == NotificationLevel::Silent);
    assert!(!e.0.notification_delivery(&reply, Some("phone")));
    let urgent =
        e.0.send_notification(NewNotification {
            title: "Urgent news".into(),
            urgent: true,
            session_id: Some(session.clone()),
            ..Default::default()
        })
        .unwrap();
    assert!(e.0.notification_delivery(&urgent, Some("laptop")));
    e.0.notification_presence.lock().insert(
        "laptop".into(),
        (
            std::time::Instant::now() - Duration::from_secs(46),
            vec![session],
        ),
    );
    assert!(e.0.notification_delivery(&n, Some("laptop")));
}

fn job_alert(body: &str, urgent: bool) -> NewNotification {
    NewNotification {
        title: "Monitor".into(),
        body: body.into(),
        source_key: "job_test".into(),
        urgent,
        ..Default::default()
    }
}

#[test]
fn notification_policy_every_default_and_quiet_hours() {
    use NotificationLevel::*;
    let policy = NotificationPolicy {
        quiet: true,
        quiet_from: 1320,
        quiet_to: 420,
        ..Default::default()
    };
    for (kind, source, expected) in [
        ("approval", "bot", Page),
        ("secret", "chat", Page),
        ("error", "job", Page),
        ("helper", "helper", Silent),
        ("finished", "chat", Page),
        ("finished", "bot", Digest),
        ("message", "bot", Digest),
        ("reminder", "bot", Page),
        ("job", "job", Digest),
        ("progress", "bot", Digest),
        ("system", "system", Silent),
    ] {
        let mut n = NewNotification {
            kind: kind.into(),
            source: source.into(),
            ..Default::default()
        };
        assert_eq!(
            policy_level(&n, &policy, 720).0,
            expected,
            "{kind}/{source}"
        );
        assert_eq!(
            policy_level(&n, &policy, 1380).0,
            if expected == Page { Digest } else { expected }
        );
        n.urgent = true;
        assert_eq!(
            policy_level(&n, &policy, 1380).0,
            Page,
            "urgent always bypasses quiet hours"
        );
    }
    assert!(!quiet_at(&policy, 420) && quiet_at(&policy, 1320));
}

#[tokio::test]
async fn notification_policy_first_occurrence_exact_repeats_and_urgent() {
    let (e, _dir, _rt) = super::super::test_support::engine().await;
    let first =
        e.0.send_notification(job_alert("Queue has 3 items", false))
            .unwrap();
    assert_eq!(first.level, NotificationLevel::Digest);
    let repeat =
        e.0.send_notification(job_alert("Queue has 3 items", false))
            .unwrap();
    assert_eq!(
        (repeat.id, repeat.repeat_count, repeat.grouped_into),
        (first.id, 2, Some(first.id))
    );
    for text in [
        "Queue has 4 items",
        "QUEUE has 3 items",
        "Queue has 3 items at 04:40",
    ] {
        let n = e.0.send_notification(job_alert(text, false)).unwrap();
        assert!(n.grouped_into.is_none(), "only exact text is grouped");
        assert_eq!(n.level, NotificationLevel::Digest);
    }
    for i in 0..20 {
        let urgent =
            e.0.send_notification(job_alert("New vulnerability, work halted", true))
                .unwrap();
        if i < MAX_PER_HOUR {
            assert_eq!(urgent.level, NotificationLevel::Page);
            assert!(
                !urgent.quiet && e.0.notification_delivery(&urgent, Some("phone")),
                "even repeated urgent items page"
            );
        } else {
            // More than the hourly cap from one source: kept in the digest, saying why.
            assert_eq!(urgent.level, NotificationLevel::Digest, "{i}");
            assert!(urgent.reason.contains("in an hour"), "{}", urgent.reason);
        }
    }
    let chat =
        e.0.send_notification(NewNotification {
            title: "First requested result".into(),
            ..Default::default()
        })
        .unwrap();
    let changed =
        e.0.send_notification(NewNotification {
            title: "Second requested result".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(
        !chat.quiet && !changed.quiet,
        "no hourly per-source throttle"
    );
}

#[tokio::test]
async fn notification_policy_repeat_and_source_choices_survive_restart() {
    let (e, dir, rt) = super::super::test_support::engine().await;
    let first =
        e.0.send_notification(job_alert("Nothing changed", false))
            .unwrap();
    e.store()
        .kv_set(
            "notification_policy",
            &serde_json::to_string(&NotificationPolicy {
                sources: vec![NotificationSourceRule {
                    key: "job_muted".into(),
                    name: "Muted monitor".into(),
                    level: NotificationLevel::Silent,
                }],
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
    drop(rt);
    drop(e);
    let e = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let repeated =
        e.0.send_notification(job_alert("Nothing changed", false))
            .unwrap();
    assert_eq!(repeated.grouped_into, Some(first.id));
    let n =
        e.0.send_notification(NewNotification {
            title: "New muted result".into(),
            source_key: "job_muted".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(n.level, NotificationLevel::Silent);
    assert_eq!(n.reason, "you muted this source");
    let mut p = e.0.notification_policy().unwrap();
    p.sources[0].level = NotificationLevel::Digest;
    assert_eq!(
        policy_level(
            &NewNotification {
                urgent: true,
                source_key: "job_muted".into(),
                ..Default::default()
            },
            &p,
            720
        )
        .0,
        NotificationLevel::Digest,
        "only an explicit owner ceiling can turn urgent down"
    );
    p.sources.clear();
    assert_eq!(
        policy_level(
            &NewNotification {
                urgent: true,
                ..Default::default()
            },
            &p,
            720
        )
        .0,
        NotificationLevel::Page
    );
}

#[tokio::test]
async fn notification_policy_digest_empty_grouping_and_atomic_consumption() {
    let (e, _dir, _rt) = super::super::test_support::engine().await;
    assert!(
        e.store()
            .assemble_notification_digest(now_ms())
            .unwrap()
            .is_none()
    );
    for _ in 0..3 {
        e.0.send_notification(job_alert("One result", false))
            .unwrap();
    }
    e.0.send_notification(job_alert("Another result", false))
        .unwrap();
    let summary = e
        .store()
        .assemble_notification_digest(now_ms())
        .unwrap()
        .unwrap();
    assert_eq!(
        (summary.kind.as_str(), summary.level, summary.quiet),
        ("digest", NotificationLevel::Digest, false)
    );
    assert!(
        summary.body.contains("4")
            && summary.body.contains("One result")
            && summary.body.contains("Another result")
    );
    assert!(e.0.notification_delivery(&summary, Some("phone")));
    assert!(
        e.store()
            .assemble_notification_digest(now_ms())
            .unwrap()
            .is_none(),
        "consumption is atomic"
    );
    e.store().kv_set("notification_digest_next", "1").unwrap();
    e.0.deliver_due_notifications();
    assert!(
        e.store()
            .kv_get("notification_digest_next")
            .unwrap()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > now_ms()
    );
}

#[tokio::test]
async fn notification_policy_receipts_are_not_acknowledgements() {
    let (e, _dir, _rt) = super::super::test_support::engine().await;
    let (phone, _) = e.store().add_device("Phone").unwrap();
    let n =
        e.0.send_notification(NewNotification {
            title: "Test".into(),
            kind: "test".into(),
            urgent: true,
            ..Default::default()
        })
        .unwrap();
    e.store().mark_notifications_shown(&phone, &[n.id]).unwrap();
    assert!(!e.store().notification_delivered(n.id, &phone).unwrap());
    e.store()
        .report_notification_delivery(&phone, &[n.id])
        .unwrap();
    e.store()
        .report_notification_delivery(&phone, &[n.id])
        .unwrap();
    assert!(e.store().notification_delivered(n.id, &phone).unwrap());
}

#[tokio::test]
async fn notification_policy_synthetic_week_500_items() {
    let (e, _dir, rt) = super::super::test_support::engine().await;
    let id = rt.st.lock().info.id.clone();
    let mut pages = 0;
    let mut levels = [0usize; 3];
    // Shape matches the seven-day counts, not any real message content.
    for (kind, count) in [
        ("helper", 150),
        ("finished", 146),
        ("job", 144),
        ("approval", 18),
        ("secret", 2),
        ("other", 40),
    ] {
        for i in 0..count {
            let urgent = kind == "other" && i < 26;
            let viewing = kind == "finished" && i < 143
                || kind == "approval" && i < 16
                || kind == "secret" && i == 0;
            if viewing {
                e.0.notification_presence.lock().insert(
                    "phone".into(),
                    (std::time::Instant::now(), vec![id.clone()]),
                );
            } else {
                e.0.notification_presence.lock().clear();
            }
            let n =
                e.0.send_notification(NewNotification {
                    title: format!("Synthetic {kind} {i}"),
                    kind: kind.into(),
                    session_id: Some(id.clone()),
                    source: if kind == "job" {
                        "job"
                    } else if kind == "other" {
                        "bot"
                    } else {
                        "chat"
                    }
                    .into(),
                    urgent,
                    ..Default::default()
                })
                .unwrap();
            levels[match n.level {
                NotificationLevel::Page => 0,
                NotificationLevel::Digest => 1,
                NotificationLevel::Silent => 2,
            }] += 1;
            pages += usize::from(e.0.notification_delivery(&n, Some("phone")));
        }
    }
    // Replies watched as they finished are silent (143 of them here).
    assert_eq!(levels, [49, 158, 293]);
    assert_eq!(pages, 32);
    println!(
        "Synthetic week 500 inputs, 49 Page-classified, 158 Digest, 293 Silent (143 replies watched as they finished), 32 phone pages including all 26 urgent. Few pages cannot be guaranteed while 26 urgent flags must always page."
    );
}
