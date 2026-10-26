use super::*;
use serde_json::json;
use tessera_proto::ApprovalRequest;

fn info(id: &str, status: &str, private: bool, kind: &str) -> SessionInfo {
    serde_json::from_value(json!({
        "id": id, "title": "Fix the build", "kind": kind, "cwd": "/w",
        "model": { "provider": "p", "model": "m" }, "thinking": "auto",
        "permission": "default", "status": status, "members": [],
        "created_at": 0, "updated_at": 0, "last_seq": 0,
        "usage": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 },
        "context": { "tokens": 0, "window": 0, "compactions": 0 },
        "pinned": false, "archived": false,
        "preview": "Deleted the token sk-live-123 from the config", "private": private
    }))
    .unwrap()
}

fn updated(id: &str, status: &str) -> Event {
    Event::SessionUpdated {
        session: info(id, status, false, "chat"),
    }
}

fn asked(session: &str, id: &str) -> Event {
    Event::ApprovalRequested {
        session_id: session.into(),
        request: ApprovalRequest {
            id: id.into(),
            call_id: "c".into(),
            tool: "bash".into(),
            summary: "rm -rf build/".into(),
            reason: "destructive".into(),
            arguments: json!({ "command": "rm -rf build/" }),
            once: false,
        },
    }
}

fn report(session: &str) -> Event {
    report_from(session, "child")
}

fn report_from(session: &str, child: &str) -> Event {
    Event::EntryAdded {
        session_id: session.into(),
        entry: serde_json::from_value(json!({
            "seq": 1, "ts": 0, "kind": "user", "author": format!("subagent:{child}"),
            "content": [{ "type": "text",
                "text": "[Report from subagent \"scout\" · 3 steps · 20 s · session child]\n\nFound it." }]
        }))
        .unwrap(),
    }
}

/// Connected, in the background, notifications allowed.
fn away(settings: Settings) -> Inner {
    let mut i = Inner::new(Stored {
        settings,
        permission_asked: false,
    });
    i.permission = Permission::Granted;
    i.attach();
    i.link = true;
    i.automatic_notifications = Some(false);
    i.foreground = false;
    i
}

fn posts(actions: &[Action]) -> Vec<&Note> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Post(n) => Some(n),
            _ => None,
        })
        .collect()
}

#[test]
fn replacing_or_detaching_a_connection_withdraws_posts_and_clears_opened_chat() {
    let mut i = away(Settings::default());
    let posted_generation = i.generation;
    i.shown.insert("s1".into(), Kind::Approval);
    i.opened = Some(("s1".into(), Instant::now()));
    let withdrawn = i.invalidate();
    assert!(
        withdrawn
            .iter()
            .any(|a| matches!(a, Action::Withdraw(key) if key == "s1"))
    );
    assert!(i.shown.is_empty() && i.opened.is_none());
    i.detach();
    assert!(!toast_content::connection_allowed(
        posted_generation,
        i.generation,
        i.attached
    ));
    i.attach(); // another server (or the same saved server, fresh Client)
    assert!(!toast_content::connection_allowed(
        posted_generation,
        i.generation,
        i.attached
    ));
}

#[test]
fn defaults_are_safe_and_partial_settings_fill_in() {
    let s = Settings::default();
    assert!(s.enabled && s.approvals && !s.previews);
    assert_eq!(s.keep_alive, KeepAlive::WhileBusy);
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["keepAlive"], "while_busy");
    let partial: Settings = serde_json::from_value(json!({ "jobs": false })).unwrap();
    assert!(!partial.jobs && !partial.previews && partial.enabled);
    // A file from an older version still loads.
    let stored: Stored = serde_json::from_value(json!({ "enabled": false })).unwrap();
    assert!(!stored.settings.enabled && !stored.permission_asked);
}

#[test]
fn nothing_while_the_app_is_in_front() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    i.foreground = true;
    i.event(now, &updated("s1", "running"));
    assert!(posts(&i.event(now, &asked("s1", "a1"))).is_empty());
}

#[test]
fn approvals_are_generic_unless_previews_are_on() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    i.event(now, &updated("s1", "awaiting_approval"));
    let out = i.event(now, &asked("s1", "a1"));
    let n = posts(&out)[0];
    assert_eq!(n.title, "Tessera");
    assert_eq!(n.body, "New activity. Open Tessera for details.");
    assert!(n.from.is_none());
    assert!(n.urgent() && n.session.as_deref() == Some("s1"));

    let mut i = away(Settings {
        previews: true,
        ..Settings::default()
    });
    i.event(now, &updated("s1", "awaiting_approval"));
    let out = i.event(now, &asked("s1", "a1"));
    assert_eq!(posts(&out)[0].body, "Fix the build: rm -rf build/");
}

#[test]
fn private_chats_never_show_title_or_content() {
    let now = Instant::now();
    let mut i = away(Settings {
        previews: true,
        ..Settings::default()
    });
    i.event(
        now,
        &Event::SessionUpdated {
            session: info("p1", "awaiting_approval", true, "chat"),
        },
    );
    let out = i.event(now, &asked("p1", "a1"));
    assert_eq!(posts(&out)[0].title, "Tessera");
    assert_eq!(
        posts(&out)[0].body,
        "New activity. Open Tessera for details."
    );
    // Unknown session: it might be private.
    let out = i.event(now, &asked("zz", "a2"));
    assert_eq!(
        posts(&out)[0].body,
        "New activity. Open Tessera for details."
    );
}

#[test]
fn a_long_turn_is_reported_once_after_it_settles() {
    let t0 = Instant::now();
    let mut i = away(Settings::default());
    i.event(t0, &updated("s1", "running"));
    // A step ends and the next starts within the settle time: no notification.
    i.event(t0 + Duration::from_secs(10), &updated("s1", "idle"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(11))).is_empty());
    i.event(t0 + Duration::from_secs(11), &updated("s1", "running"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(13))).is_empty());
    i.event(t0 + Duration::from_secs(30), &updated("s1", "idle"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(31))).is_empty());
    let out = i.tick(t0 + Duration::from_secs(32));
    let n = posts(&out)[0];
    assert_eq!(
        (n.title.as_str(), n.body.as_str()),
        ("Tessera", "New activity. Open Tessera for details.")
    );
    assert!(posts(&i.tick(t0 + Duration::from_secs(60))).is_empty());
}

#[test]
fn short_turns_and_turns_seen_in_the_app_stay_quiet() {
    let t0 = Instant::now();
    let mut i = away(Settings::default());
    i.event(t0, &updated("s1", "running"));
    i.event(t0 + Duration::from_secs(5), &updated("s1", "idle"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(10))).is_empty());

    i.foreground = true;
    i.event(t0 + Duration::from_secs(20), &updated("s1", "running"));
    i.event(t0 + Duration::from_secs(60), &updated("s1", "idle"));
    i.foreground = false;
    assert!(posts(&i.tick(t0 + Duration::from_secs(70))).is_empty());
}

#[test]
fn a_failed_turn_is_an_error_notification() {
    let t0 = Instant::now();
    let mut i = away(Settings::default());
    i.event(t0, &updated("s1", "running"));
    i.event(t0 + Duration::from_secs(1), &updated("s1", "error"));
    let out = i.tick(t0 + Duration::from_secs(4));
    assert_eq!(posts(&out)[0].title, "Tessera");
    assert_eq!(
        posts(&out)[0].body,
        "New activity. Open Tessera for details."
    );
}

#[test]
fn subagents_and_jobs_sessions_have_their_own_triggers() {
    let t0 = Instant::now();
    let mut i = away(Settings::default());
    for kind in ["subagent", "job"] {
        i.event(
            t0,
            &Event::SessionUpdated {
                session: info(kind, "running", false, kind),
            },
        );
        i.event(
            t0 + Duration::from_secs(60),
            &Event::SessionUpdated {
                session: info(kind, "idle", false, kind),
            },
        );
    }
    assert!(posts(&i.tick(t0 + Duration::from_secs(70))).is_empty());
}

#[test]
fn the_harness_helpers_are_never_counted_or_kept_alive_for() {
    let t0 = Instant::now();
    let helper = |status: &str| {
        let mut info = info("review", status, false, "subagent");
        info.internal = true;
        info.parent_id = Some("s1".into());
        Event::SessionUpdated { session: info }
    };
    let mut i = away(Settings::default());
    i.event(t0, &updated("s1", "idle"));
    // A memory review at work is not the user's work: no keep-alive for it.
    i.event(t0 + Duration::from_secs(1), &helper("running"));
    assert!(!i.busy());
    i.event(t0 + Duration::from_secs(40), &helper("idle"));
    // Even something shaped like a report from it is not a subagent's report.
    i.event(t0 + Duration::from_secs(41), &report_from("s1", "review"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(60))).is_empty());
}

#[test]
fn subagent_reports_never_popup_even_with_an_old_enabled_preference() {
    let t0 = Instant::now();
    let mut i = away(Settings { subagents: true, ..Settings::default() });
    i.event(t0, &updated("s1", "idle"));
    for k in 0..3 { i.event(t0 + Duration::from_millis(100 * k), &report("s1")); }
    assert!(posts(&i.tick(t0 + Duration::from_secs(3))).is_empty());
    i.event(t0 + Duration::from_secs(4), &updated("s1", "running"));
    i.event(t0 + Duration::from_secs(5), &report("s1"));
    i.event(t0 + Duration::from_secs(8), &updated("s1", "idle"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(11))).is_empty());
}

#[test]
fn one_notification_per_session_per_window_and_the_important_one_wins() {
    let t0 = Instant::now();
    let mut i = away(Settings::default());
    i.event(t0, &updated("s1", "idle"));
    i.event(t0 + Duration::from_secs(2), &asked("s1", "a0"));
    assert_eq!(i.last_posted.get("s1"), Some(&(t0 + Duration::from_secs(2))));
    // Within ten seconds another approval replaces, without another burst.
    assert!(posts(&i.event(t0 + Duration::from_secs(6), &asked("s1", "a1"))).is_empty());
    assert_eq!(i.next_deadline(), Some(t0 + Duration::from_secs(12)));
    let out = i.tick(t0 + Duration::from_secs(12));
    assert_eq!(posts(&out).len(), 1);
    assert_eq!(posts(&out)[0].kind, Kind::Approval);
}

#[test]
fn an_answered_approval_is_withdrawn() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    i.event(now, &updated("s1", "awaiting_approval"));
    i.event(now, &asked("s1", "a1"));
    let out = i.event(
        now,
        &Event::ApprovalResolved {
            session_id: "s1".into(),
            request_id: "a1".into(),
            decision: tessera_proto::ApprovalDecision::Allow,
        },
    );
    assert!(out.contains(&Action::Withdraw("s1".into())));
}

#[test]
fn leaving_with_an_unanswered_approval_says_so_once() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    i.foreground = true;
    i.event(now, &updated("s1", "awaiting_approval"));
    i.event(now, &asked("s1", "a1"));
    assert_eq!(posts(&i.set_foreground(now, false)).len(), 1);
    i.set_foreground(now, true);
    assert!(posts(&i.set_foreground(now + Duration::from_secs(60), false)).is_empty());
}

#[test]
fn scheduled_runs_notify_through_the_server_under_their_setting() {
    let now = Instant::now();
    let job = |status: &str| -> Event {
        Event::JobUpdated {
            job: serde_json::from_value(json!({
                "id": "job_1", "name": "Nightly tests", "schedule": "@every 1h",
                "prompt": "run", "cwd": "/w", "enabled": true, "permission": "default",
                "last_session": "s9", "last_status": status
            }))
            .unwrap(),
        }
    };
    let failed = |id: u64| Event::Notification {
        notification: serde_json::from_value(json!({
            "id": id, "title": "Scheduled task failed", "body": "Nightly tests: the build broke",
            "session_id": "s9", "from": "Scheduled tasks", "created_at": 1, "due_at": 2,
            "sent": true, "seen": false, "kind": "job"
        }))
        .unwrap(),
    };
    // The job's own updates alert nothing (the server's notification does).
    let mut i = away(Settings::default());
    i.automatic_notifications = Some(true);
    i.event(now, &job("running"));
    assert!(posts(&i.event(now, &job("error"))).is_empty());
    let out = i.event(now, &failed(5));
    let n = posts(&out)[0];
    assert_eq!(n.kind, Kind::Job);
    assert_eq!(n.session.as_deref(), Some("s9"));
    // Without previews every OS field is fixed and generic.
    assert_eq!(
        (n.title.as_str(), n.body.as_str()),
        ("Tessera", "New activity. Open Tessera for details.")
    );
    assert!(!out.iter().any(|a| matches!(a, Action::Ack(_))));
    // Scheduled tasks turned off on this device: acknowledged, not shown.
    let mut i = away(Settings {
        jobs: false,
        ..Settings::default()
    });
    let out = i.event(now, &failed(6));
    assert!(posts(&out).is_empty());
    assert!(out.contains(&i.acknowledge(vec![6])));
    // With previews: what happened.
    let mut i = away(Settings {
        previews: true,
        ..Settings::default()
    });
    assert_eq!(
        posts(&i.event(now, &failed(7)))[0].body,
        "Nightly tests: the build broke"
    );
}

#[test]
fn error_notices_without_a_session_are_generic() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    let out = i.event(
        now,
        &Event::Notice {
            session_id: None,
            level: NoticeLevel::Error,
            message: "ChatGPT sign-in failed: token=abc".into(),
        },
    );
    let n = posts(&out)[0];
    assert_eq!(
        (n.key.as_str(), n.body.as_str()),
        (APP_KEY, "New activity. Open Tessera for details.")
    );
    assert!(n.session.is_none());
    let out = i.event(
        now,
        &Event::Notice {
            session_id: None,
            level: NoticeLevel::Warning,
            message: "slow".into(),
        },
    );
    assert!(posts(&out).is_empty());
}

#[test]
fn categories_and_the_master_switch_are_respected() {
    let now = Instant::now();
    let mut i = away(Settings {
        approvals: false,
        ..Settings::default()
    });
    i.event(now, &updated("s1", "awaiting_approval"));
    assert!(posts(&i.event(now, &asked("s1", "a1"))).is_empty());
    let mut i = away(Settings {
        enabled: false,
        ..Settings::default()
    });
    assert!(posts(&i.event(now, &asked("s1", "a1"))).is_empty());
}

#[test]
fn ids_from_the_server_are_checked() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    for bad in ["", "a/b", "x y", "intent:#Intent;end", &"a".repeat(65)] {
        assert!(!valid_id(bad), "{bad}");
        assert!(posts(&i.event(now, &asked(bad, "a1"))).is_empty(), "{bad}");
    }
    assert!(valid_id("01j9zk3m5q7r_ab-CD"));
    // A session with a bad id is never tracked, so it leaves no deadline
    // behind (which would spin the timer).
    let t0 = Instant::now();
    i.event(t0, &updated("bad/id", "running"));
    i.event(t0 + Duration::from_secs(30), &updated("bad/id", "idle"));
    assert!(posts(&i.tick(t0 + Duration::from_secs(40))).is_empty());
    assert!(i.sessions.is_empty());
    assert_eq!(i.next_deadline(), None);
}

#[test]
fn clip_makes_one_short_plain_line() {
    assert_eq!(clip("  a\n\tb  \u{7}c ", 10), "a b c");
    assert_eq!(clip("abc\u{202e}def", 10), "abcdef");
    assert_eq!(clip("one two three", 7), "one two…");
    assert_eq!(clip("one two three", 4), "one…");
    assert_eq!(clip("", 5), "");
}

#[test]
fn keep_alive_follows_the_work() {
    let t0 = Instant::now();
    let service = |out: &[Action]| {
        out.iter().find_map(|a| match a {
            Action::Service(s) => Some(*s),
            _ => None,
        })
    };
    let mut i = away(Settings::default());
    let out = i.event(t0, &updated("s1", "running"));
    assert_eq!(
        service(&out),
        Some(Service {
            run: true,
            eager: false
        })
    );
    i.event(t0 + Duration::from_secs(5), &updated("s1", "idle"));
    // Lingers after the work, then stops.
    assert_eq!(service(&i.tick(t0 + Duration::from_secs(60))), None);
    let stop = t0 + Duration::from_secs(5) + LINGER;
    assert_eq!(i.next_deadline(), Some(stop));
    assert_eq!(
        service(&i.tick(stop)),
        Some(Service {
            run: false,
            eager: false
        })
    );

    // Lost link: not busy as far as we can tell.
    let mut i = away(Settings::default());
    i.event(t0, &updated("s1", "running"));
    i.link = false;
    assert_eq!(
        service(&i.tick(t0 + LINGER)),
        Some(Service {
            run: false,
            eager: false
        })
    );

    let mut i = away(Settings {
        keep_alive: KeepAlive::Always,
        ..Settings::default()
    });
    assert_eq!(
        service(&i.tick(t0)),
        Some(Service {
            run: true,
            eager: true
        })
    );
    i.detach();
    assert_eq!(
        service(&i.tick(t0)),
        Some(Service {
            run: false,
            eager: false
        })
    );

    let mut i = away(Settings {
        keep_alive: KeepAlive::Off,
        ..Settings::default()
    });
    assert_eq!(
        service(&i.event(t0, &updated("s1", "running"))),
        Some(Service {
            run: false,
            eager: false
        })
    );
}

#[test]
fn a_missed_approval_asks_for_the_permission_once() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    i.permission = Permission::Prompt;
    i.event(now, &updated("s1", "awaiting_approval"));
    i.event(now, &asked("s1", "a1"));
    assert!(i.ask_after_miss());
    i.event(now, &asked("s1", "a2"));
    assert!(!i.ask_after_miss());
}

#[test]
fn reconnecting_catches_approvals_asked_meanwhile() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    let mut child = info("c1", "awaiting_approval", false, "subagent");
    child.parent_id = Some("s1".into());
    let out = i.seed(now, Some(vec![info("s1", "running", false, "chat"), child]));
    let n = posts(&out)[0];
    assert_eq!(
        (n.key.as_str(), n.body.as_str()),
        ("s1", "New activity. Open Tessera for details.")
    );
    // Seen already: no repeat on the next resync.
    let mut child = info("c1", "awaiting_approval", false, "subagent");
    child.parent_id = Some("s1".into());
    assert!(posts(&i.seed(now + Duration::from_secs(30), Some(vec![child]))).is_empty());
}

fn agent_message(id: u64, session: &str, sent: bool) -> Event {
    Event::Notification {
        notification: serde_json::from_value(json!({
            "id": id, "title": "Build finished", "body": "All 42 tests pass",
            "session_id": session, "from": "Release bot", "urgent": false,
            "created_at": 1, "due_at": 5000, "sent": sent, "seen": false
        }))
        .unwrap(),
    }
}

#[test]
fn agents_messages_show_in_full_once_and_go_when_seen() {
    let mut i = away(Settings {
        previews: true,
        ..Settings::default()
    });
    let t = Instant::now();
    i.event(t, &updated("s1", "idle"));
    let out = i.event(t, &agent_message(7, "s1", true));
    assert!(
        !out.iter().any(|a| matches!(a, Action::Ack(_))),
        "must wait for accepted post: {out:?}"
    );
    let p = posts(&out);
    assert_eq!(p.len(), 1);
    // Native previews opt in to the requested message text.
    assert_eq!(
        (
            p[0].key.as_str(),
            p[0].kind,
            p[0].title.as_str(),
            p[0].body.as_str()
        ),
        ("s1", Kind::Agent, "Build finished", "All 42 tests pass")
    );
    assert_eq!(p[0].session.as_deref(), Some("s1"));
    assert_eq!(p[0].from.as_deref(), Some("Release bot"));
    assert_eq!(p[0].public_body(), "A message for you");
    // A second one from the same chat is burst-throttled, then replaces.
    let out = i.event(t, &agent_message(8, "s1", true));
    assert!(posts(&out).is_empty());
    assert_eq!(posts(&i.tick(t + PER_SESSION)).len(), 1);
    assert!(posts(&i.event(t + PER_SESSION, &agent_message(8, "s1", true))).is_empty());
    let out = i.event(t + PER_SESSION, &Event::NotificationsSeen { ids: vec![7] });
    assert!(!out.contains(&Action::Withdraw("s1".into())), "stale read cannot withdraw newer replacement");
    let out = i.seen("s1");
    assert!(out.contains(&Action::Withdraw("s1".into())), "opening chat clears its live card");
}

#[test]
fn agents_messages_in_front_are_left_to_the_app_and_scheduled_ones_wake_the_phone() {
    let mut i = away(Settings::default());
    let t = Instant::now();
    i.set_foreground(t, true);
    let out = i.event(t, &agent_message(9, "s1", true));
    assert_eq!(
        out.iter().filter(|a| matches!(a, Action::Post(_))).count(),
        0
    );
    assert!(
        out.contains(&i.acknowledge(vec![9])),
        "handled: the app shows it"
    );
    let out = i.event(t, &agent_message(10, "s1", false));
    assert!(out.contains(&Action::WakeAt(5000)), "{out:?}");
    assert!(!out.iter().any(|a| matches!(a, Action::Ack(_))));
    // Missed while away: shown when the connection comes back.
    let mut i = away(Settings::default());
    let p = PendingNotifications {
        new: vec![
            serde_json::from_value(json!({
                "id": 11, "title": "Done", "body": "", "created_at": 1, "due_at": 2,
                "sent": true, "seen": false
            }))
            .unwrap(),
        ],
        seen: vec![4],
        withdraw: Vec::new(),
        next_due: Some(6000),
        waiting: vec![],
    };
    let out = i.pending(t, p);
    assert!(out.contains(&Action::Withdraw("n4".into())));
    assert!(!out.iter().any(|a| matches!(a, Action::Ack(_))));
    assert!(out.contains(&Action::WakeAt(6000)));
    assert_eq!(posts(&out)[0].session, None);
}

#[test]
fn quiet_hours_cross_midnight_and_make_notifications_silent() {
    let s = Settings {
        quiet: true,
        ..Settings::default()
    };
    let at = |h: u16, m: u16| s.quiet_at(h * 60 + m);
    assert!(at(22, 0) && at(23, 59) && at(0, 0) && at(6, 59));
    assert!(!at(7, 0) && !at(12, 0) && !at(21, 59));
    let day = Settings {
        quiet: true,
        quiet_from: "13:00".into(),
        quiet_to: "14:30".into(),
        ..Settings::default()
    };
    assert!(day.quiet_at(13 * 60) && !day.quiet_at(14 * 60 + 30));
    let off = Settings::default();
    assert!(!off.quiet_at(23 * 60), "off unless turned on");
    let broken = Settings {
        quiet: true,
        quiet_from: "25:00".into(),
        ..Settings::default()
    };
    assert!(!broken.quiet_at(23 * 60));
    // Older settings files have no quiet hours: defaults fill in.
    let old: Settings = serde_json::from_value(json!({"enabled": true, "previews": true})).unwrap();
    assert!(!old.quiet && old.quiet_from == "22:00" && old.quiet_to == "07:00");
}

#[test]
fn completion_marks_from_old_servers_are_handled_without_os_posts_or_preview_changes() {
    let mut i = away(Settings::default());
    let t = Instant::now();
    let original = info("s1", "running", false, "chat");
    i.session(t, &original);
    let mut mark = original.clone();
    mark.preview = "✓".into();
    i.session(t, &mark);
    assert_eq!(i.sessions["s1"].preview, original.preview);
    for sent in [false, true] {
        let n: UserNotification = serde_json::from_value(json!({
            "id": 72, "title": "Finished", "body": "Done ✓", "created_at": 1,
            "due_at": 2, "sent": sent, "seen": false
        }))
        .unwrap();
        let out = i.agent_message(t, &n);
        assert_eq!(out, vec![i.acknowledge(vec![72])]);
        let p = PendingNotifications {
            new: vec![n],
            seen: vec![],
        withdraw: Vec::new(),
            next_due: None,
            waiting: vec![],
        };
        let (pulled, ack, _) = pull::decide(&Settings::default(), &p, &[], false);
        assert!(pulled.posts.is_empty());
        assert_eq!(ack, vec![72]);
    }
}
// Exercise the production producer queue and the Windows consumer's exact
// generation predicate, not just Inner's un-stamped decisions.
fn producer() -> (Notifier, tokio::sync::mpsc::UnboundedReceiver<Action>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        Notifier {
            client: Mutex::new(None),
            dispatch: tokio::sync::Mutex::new(()),
            save_gate: std::sync::Arc::new(Mutex::new(())),
            file: PathBuf::new(),
            inner: std::sync::Arc::new(Mutex::new(away(Settings::default()))),
            tx,
            wake: tokio::sync::Notify::new(),
        },
        rx,
    )
}

fn delivered(n: &Notifier, rx: &mut tokio::sync::mpsc::UnboundedReceiver<Action>) -> Vec<Note> {
    let i = n.inner.lock();
    let mut out = Vec::new();
    while let Ok(action) = rx.try_recv() {
        match action {
            Action::Post(note) if i.accepts_post(&note) => out.push(note),
            // Dedicated local sample branch: no session or server authority.
            Action::Sample => out.push(Note::sample()),
            _ => {}
        }
    }
    out
}

#[test]
fn settling_deadline_reaches_the_consumer_with_its_connection_generation() {
    let (n, mut rx) = producer();
    let now = Instant::now();
    n.with_at(now, |i, t| i.event(t, &updated("s1", "running")));
    n.with_at(now + LONG_TURN, |i, t| i.event(t, &updated("s1", "idle")));
    assert!(delivered(&n, &mut rx).is_empty());
    n.tick(now + LONG_TURN + SETTLE);
    let notes = delivered(&n, &mut rx);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].kind, Kind::Finished);
    assert_eq!(notes[0].generation, n.inner.lock().generation);
    assert_ne!(notes[0].generation, 0);
}

#[test]
fn rate_limit_deadline_flush_reaches_the_consumer_with_its_generation() {
    let (n, mut rx) = producer();
    let now = Instant::now();
    n.with_at(now, |i, t| i.event(t, &asked("s1", "a1")));
    assert_eq!(delivered(&n, &mut rx).len(), 1);
    n.with_at(now + Duration::from_secs(1), |i, t| {
        i.event(t, &asked("s1", "a2"))
    });
    assert!(delivered(&n, &mut rx).is_empty());
    n.tick(now + PER_SESSION);
    let notes = delivered(&n, &mut rx);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].generation, n.inner.lock().generation);
}

#[tokio::test]
async fn replacement_drops_queued_deadline_posts_and_events_without_restamping() {
    let (n, mut rx) = producer();
    let now = Instant::now();
    let old = n.inner.lock().generation;
    n.with_at(now, |i, t| i.event(t, &updated("s1", "running")));
    n.with_at(now + LONG_TURN, |i, t| i.event(t, &updated("s1", "idle")));
    n.tick(now + LONG_TURN + SETTLE); // queued, not consumed yet
    n.attach(None, "test-server".into()).await;
    assert!(delivered(&n, &mut rx).is_empty());
    n.event(old, &asked("s1", "stale"));
    assert!(delivered(&n, &mut rx).is_empty());
    let current = n.inner.lock().generation;
    n.link(current, true).await;
    n.event(current, &asked("s1", "fresh"));
    assert_eq!(delivered(&n, &mut rx).len(), 1);
}

#[tokio::test]
async fn sample_command_uses_a_local_no_session_path_before_and_after_replacement() {
    let (n, mut rx) = producer();
    n.detach().await;
    n.sample();
    let notes = delivered(&n, &mut rx);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].key, "test");
    assert_eq!(notes[0].session, None);
    assert_eq!(notes[0].generation, 0); // never a connection post
    assert_eq!(
        notes[0].body,
        "This is how Tessera lets you know when it needs you."
    );
    n.attach(None, "test-server".into()).await;
    n.sample();
    n.attach(None, "test-server".into()).await; // queued local sample is safe, not an old server's post
    assert_eq!(delivered(&n, &mut rx)[0].session, None);
}

#[test]
fn claude_plan_reset_native_system_body_is_local_on_live_and_pull_paths() {
    for (stamp, hours, expected) in [
        ("2026-11-01T06:59:00Z", -5, "Nov 1, 1:59 AM -05:00"),
        ("2026-11-01T07:01:00Z", -6, "Nov 1, 1:01 AM -06:00"),
        ("2026-10-12T16:00:00Z", 9, "Oct 13, 1:00 AM +09:00"),
    ] {
        let body = format!("Your Claude plan's limit is reached. It resets at {stamp}.");
        let converted = limit_body_with(&body, |at| {
            at.with_timezone(&chrono::FixedOffset::east_opt(hours * 3600).unwrap())
                .format("%b %-d, %-I:%M %p %:z")
                .to_string()
        });
        assert!(converted.contains(expected), "{converted}");
        let n = UserNotification {
            id: 1,
            title: "Waiting".into(),
            body: body.clone(),
            session_id: None,
            from: String::new(),
            urgent: false,
            created_at: 1,
            due_at: 1,
            sent: true,
            seen: false,
            kind: String::new(), quiet: false, private: false, source_key: String::new(),
            ..Default::default()
        };
        let note = Note::agent(&n); // the same constructor Android pull uses
        assert_eq!(note.body, Note::local_limit_body(&body));
        assert!(!note.body.contains(stamp));
    }
    for body in [
        "Unrelated. It resets at 2026-10-12T16:00:00Z.",
        "Your Claude plan's limit is reached but no reset was reported.",
        "Your Claude plan's limit. It resets at invalid.",
    ] {
        assert_eq!(Note::local_limit_body(body), body);
    }
}

#[test]
fn jobupdated_remains_a_generic_preference_aware_old_server_fallback_only() {
    let now = Instant::now();
    let mut job = Job {
        id: "job_legacy".into(),
        name: "Secret duty".into(),
        last_session: Some("s1".into()),
        last_status: Some("running".into()),
        ..Default::default()
    };
    let mut i = away(Settings::default());
    i.automatic_notifications = Some(false);
    i.event(now, &Event::JobUpdated { job: job.clone() });
    job.last_status = Some("error".into());
    let out = i.event(now, &Event::JobUpdated { job: job.clone() });
    assert_eq!(
        posts(&out)[0].body,
        "New activity. Open Tessera for details."
    );
    let mut i = away(Settings {
        jobs: false,
        previews: false,
        ..Settings::default()
    });
    i.automatic_notifications = Some(false);
    job.last_status = Some("running".into());
    i.event(now, &Event::JobUpdated { job: job.clone() });
    job.last_status = Some("error".into());
    assert!(posts(&i.event(now, &Event::JobUpdated { job: job.clone() })).is_empty());
    let mut i = away(Settings::default());
    i.automatic_notifications = Some(true);
    job.last_status = Some("running".into());
    i.event(now, &Event::JobUpdated { job: job.clone() });
    job.last_status = Some("error".into());
    assert!(
        posts(&i.event(now, &Event::JobUpdated { job: job.clone() })).is_empty(),
        "negotiated server must not duplicate typed alerts"
    );
    i.detach();
    assert!(
        i.automatic_notifications.is_none(),
        "capability never survives a connection change"
    );
    i.attach();
    i.link = true;
    job.last_status = Some("running".into());
    i.event(now, &Event::JobUpdated { job: job.clone() });
    job.last_status = Some("error".into());
    assert!(
        posts(&i.event(now, &Event::JobUpdated { job })).is_empty(),
        "hydration cannot briefly run both paths before negotiation completes"
    );
}

#[tokio::test]
async fn transport_reconnect_rejects_stale_syncs_in_both_mixed_version_directions() {
    for (before, after) in [(true, false), (false, true)] {
        let (n, mut rx) = producer();
        let now = Instant::now();
        let generation = n.inner.lock().generation;
        let old = n.inner.lock().begin_sync(generation).unwrap();
        assert!(n.inner.lock().negotiated(old, before));
        n.inner
            .lock()
            .jobs
            .insert("job_mixed".into(), Some("running".into()));
        n.event(generation, &asked("s1", "old")); // queued for the OS, not consumed
        n.link(generation, false).await;
        assert!(delivered(&n, &mut rx).is_empty());
        {
            let mut i = n.inner.lock();
            assert!(i.automatic_notifications.is_none() && i.jobs.is_empty() && !i.link);
            assert!(!i.negotiated(old, before));
            assert!(
                i.hydrated(
                    old,
                    now,
                    Some(vec![info("old", "running", false, "chat")]),
                    None,
                    None
                )
                .is_empty()
            );
            assert!(!i.link && i.sessions.is_empty());
        }
        n.link(generation, true).await; // same Client, a different transport/server version
        let current = n.inner.lock().begin_sync(generation).unwrap();
        assert_ne!(old.epoch, current.epoch);
        assert!(n.inner.lock().negotiated(current, after));
        let mut job = Job {
            id: "job_mixed".into(),
            name: "PRIVATE_DUTY".into(),
            last_session: Some("s2".into()),
            last_status: Some("running".into()),
            ..Default::default()
        };
        n.with_at(now, |i, t| {
            i.hydrated(current, t, None, Some(vec![job.clone()]), None)
        });
        assert!(!n.inner.lock().negotiated(old, before));
        assert_eq!(n.inner.lock().automatic_notifications, Some(after));
        job.last_status = Some("error".into());
        n.event(generation, &Event::JobUpdated { job });
        assert_eq!(
            delivered(&n, &mut rx).len(),
            usize::from(!after),
            "old-server fallback only after its own negotiation"
        );
        if after {
            let alert: UserNotification = serde_json::from_value(json!({"id": 812, "title": "Private title", "body": "Private report", "from": "Private bot", "kind": "job", "created_at": 1, "due_at": 1, "sent": true, "seen": false})).unwrap();
            n.event(
                generation,
                &Event::Notification {
                    notification: alert,
                },
            );
            assert_eq!(
                delivered(&n, &mut rx).len(),
                1,
                "new-server typed path, not fallback"
            );
        }
        let newer = n.inner.lock().begin_sync(generation).unwrap();
        assert!(
            !n.inner.lock().negotiated(current, !after),
            "older hydration attempts in this transport are stale too"
        );
        assert!(n.inner.lock().negotiated(newer, after));
    }
}

#[test]
fn live_automatic_and_private_payloads_redact_all_os_fields_without_hydration() {
    for previews in [false, true] {
        for private in [false, true] {
            for kind in [
                "job",
                "finished",
                "error",
                "approval",
                "helper",
                "future-kind",
                "",
            ] {
                let alert: UserNotification = serde_json::from_value(json!({"id": 901, "title": "SECRET_TITLE", "body": "SECRET_BODY", "from": "SECRET_BOT", "session_id": "unknown", "kind": kind, "private": private, "created_at": 1, "due_at": 1, "sent": true, "seen": false})).unwrap();
                let mut i = away(Settings {
                    previews,
                    ..Settings::default()
                });
                let out = i.event(
                    Instant::now(),
                    &Event::Notification {
                        notification: alert,
                    },
                );
                if kind == "helper" {
                    assert!(posts(&out).is_empty(), "helper reports never make OS alerts");
                    assert!(out.contains(&i.acknowledge(vec![901])));
                    continue;
                }
                assert_eq!(posts(&out).len(), 1, "kind={kind} private={private} previews={previews}");
                let note = posts(&out)[0];
                if private || !previews {
                    assert_eq!(
                        (
                            note.title.as_str(),
                            note.body.as_str(),
                            note.from.as_deref()
                        ),
                        ("Tessera", "New activity. Open Tessera for details.", None)
                    );
                } else {
                    assert_eq!(
                        (
                            note.title.as_str(),
                            note.body.as_str(),
                            note.from.as_deref()
                        ),
                        ("SECRET_TITLE", "SECRET_BODY", Some("SECRET_BOT"))
                    );
                }
            }
        }
    }
}

#[test]
fn queued_with_previews_on_is_redacted_by_current_boundary_policy_for_every_kind() {
    for kind in [
        Kind::Agent,
        Kind::Finished,
        Kind::Approval,
        Kind::Error,
        Kind::Job,
    ] {
        let mut i = away(Settings {
            previews: true,
            ..Settings::default()
        });
        let mut note = Note::new("n902", None, kind, "SECRET_TITLE", "SECRET_BODY".into());
        note.from = Some("SECRET_SUBTITLE".into());
        let out = i.post(Instant::now(), note).unwrap();
        let Action::Post(queued) = out else {
            panic!("expected post")
        };
        assert_eq!(queued.title, "SECRET_TITLE");
        i.stored.settings.previews = false;
        let boundary = queued.payload(i.settings());
        let android = pull::post_of(&boundary);
        assert!(!serde_json::to_string(&android).unwrap().contains("SECRET"));
        assert_eq!(
            (
                boundary.title.as_str(),
                boundary.body.as_str(),
                boundary.from.as_deref()
            ),
            ("Tessera", "New activity. Open Tessera for details.", None)
        );
        let xml = toast_content::xml(&boundary.title, &boundary.body, boundary.silent);
        assert!(!xml.contains("SECRET"));
    }
}

#[test]
fn accepted_posts_reack_after_reconnect_without_posting_and_failed_posts_retry() {
    let mut i = away(Settings::default());
    let now = Instant::now();
    let event = agent_message(903, "s1", true);
    let actions = i.event(now, &event);
    assert!(!actions.iter().any(|a| matches!(a, Action::Ack(_))));
    let mut note = posts(&actions)[0].clone();
    note.generation = i.generation;
    note.transport_epoch = i.transport_epoch;
    i.transport(now, false, 1_000);
    assert!(
        i.accepted(&note, false).is_none(),
        "invalidated before post is replayable"
    );
    i.transport(now, true, 1_100);
    let replay = i.event(now, &event);
    assert_eq!(posts(&replay).len(), 1);
    let mut current = posts(&replay)[0].clone();
    current.generation = i.generation;
    current.transport_epoch = i.transport_epoch;
    let Action::Ack(ack) = i.accepted(&current, true).unwrap() else {
        panic!("expected ack")
    };
    assert_eq!(ack.ids, [903]);
    i.transport(now, false, 1_000); // ack was dropped, but OS accepted the post
    i.transport(now, true, 1_100);
    let replay = i.event(now, &event);
    assert!(
        posts(&replay).is_empty(),
        "accepted receipt must not alert twice"
    );
    assert!(
        replay.contains(&i.acknowledge(vec![903])),
        "re-Ack in current epoch"
    );
    let failed = agent_message(904, "s1", true);
    let actions = i.event(now, &failed);
    let mut current = posts(&actions)[0].clone();
    current.generation = i.generation;
    current.transport_epoch = i.transport_epoch;
    assert!(i.accepted(&current, false).is_none());
    assert_eq!(posts(&i.event(now, &failed)).len(), 1);
    let mut local = Note::new(
        "n903",
        None,
        Kind::Approval,
        "Local question",
        String::new(),
    );
    local.generation = i.generation;
    local.transport_epoch = i.transport_epoch;
    assert!(i.accepted(&local, true).is_none());
    assert!(pull::post_of(&local).notification_id.is_none());
    i.server_scope = "other-server".into();
    i.invalidate();
    assert_eq!(
        posts(&i.event(now, &event)).len(),
        1,
        "receipt ids belong to a server"
    );
}

#[cfg(all(target_os = "linux", desktop))]
#[tokio::test]
async fn xdg_show_rechecks_after_setup_and_serializes_send_with_policy_and_disconnect() {
    use super::xdg::{Sender, Xdg};
    struct Fake {
        setup: tokio::sync::Notify,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        payloads: Mutex<Vec<Note>>,
    }
    impl Sender for Fake {
        async fn setup(&self) -> Result<(), String> {
            self.setup.notified().await;
            Ok(())
        }
        async fn send(&self, note: &Note, _: u32) -> Result<u32, String> {
            self.payloads.lock().push(note.clone());
            self.started.notify_one();
            self.release.notified().await;
            Ok(1)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut n, mut rx) = producer();
    n.file = dir.path().join("notifications.json");
    n.inner.lock().stored.settings.previews = true;
    let n = std::sync::Arc::new(n);
    let generation = n.inner.lock().generation;
    n.event(generation, &agent_message(904, "s1", true));
    let Action::Post(note) = rx.try_recv().unwrap() else {
        panic!("expected post")
    };
    let fake = std::sync::Arc::new(Fake {
        setup: Default::default(),
        started: Default::default(),
        release: Default::default(),
        payloads: Mutex::new(Vec::new()),
    });
    let other = n.clone();
    let sender = fake.clone();
    let stale = note.clone();
    let post = tokio::spawn(async move { Xdg::default().show(&other, &stale, &*sender).await });
    n.link(generation, false).await;
    n.link(generation, true).await;
    fake.setup.notify_one();
    assert!(!post.await.unwrap());
    assert!(
        fake.payloads.lock().is_empty(),
        "real show path must not dispatch stale payloads"
    );

    let mut note = note;
    note.transport_epoch = n.inner.lock().transport_epoch;
    let fresh = note.clone();
    let other = n.clone();
    let sender = fake.clone();
    let post = tokio::spawn(async move { Xdg::default().show(&other, &note, &*sender).await });
    fake.setup.notify_one();
    fake.started.notified().await;
    assert!(
        n.dispatch.try_lock().is_err(),
        "Notify await must retain the dispatch guard"
    );
    let changed = n.clone();
    let disconnect = tokio::spawn(async move { changed.link_at(generation, false, 5_000).await });
    let changed = n.clone();
    let policy = tokio::spawn(async move {
        changed
            .apply_settings(Settings {
                previews: false,
                ..Settings::default()
            })
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        n.inner.lock().link,
        "invalidation cannot race final dispatch"
    );
    assert!(
        n.settings().previews && !policy.is_finished(),
        "policy cannot race final dispatch"
    );
    fake.release.notify_one();
    assert!(post.await.unwrap());
    disconnect.await.unwrap();
    assert_eq!(
        n.inner.lock().disconnected_at,
        Some(5_000),
        "capture before dispatch wait, not after the OS reply"
    );
    assert!(!n.inner.lock().link);
    policy.await.unwrap().unwrap();
    n.link(generation, true).await;
    {
        let mut inner = n.inner.lock();
        inner.automatic_notifications = Some(false);
        let actions = inner.event(
            Instant::now(),
            &Event::JobUpdated {
                job: Job {
                    id: "finished-while-xdg-dispatch-was-held".into(),
                    last_status: Some("ok".into()),
                    last_end: Some(5_001),
                    ..Default::default()
                },
            },
        );
        assert_eq!(
            posts(&actions).len(),
            1,
            "completion during dispatch contention must remain replayable"
        );
    }
    let mut fresh = fresh;
    fresh.transport_epoch = n.inner.lock().transport_epoch;
    let other = n.clone();
    let sender = fake.clone();
    let post = tokio::spawn(async move { Xdg::default().show(&other, &fresh, &*sender).await });
    fake.setup.notify_one();
    fake.started.notified().await;
    let payload = fake.payloads.lock().last().unwrap().clone();
    assert_eq!(payload.title, "Tessera");
    assert_eq!(payload.body, "New activity. Open Tessera for details.");
    assert!(payload.from.is_none());
    fake.release.notify_one();
    assert!(post.await.unwrap());
}

#[test]
fn legacy_job_completion_survives_negotiation_and_stale_hydration_once() {
    let now = Instant::now();
    let mut i = away(Settings::default());
    i.automatic_notifications = None;
    let token = i.begin_sync(i.generation).unwrap();
    let mut job = Job {
        id: "job_905".into(),
        name: "SECRET_JOB".into(),
        last_status: Some("running".into()),
        ..Job::default()
    };
    i.event(now, &Event::JobUpdated { job: job.clone() });
    job.last_status = Some("ok".into());
    i.event(now, &Event::JobUpdated { job: job.clone() });
    assert!(i.negotiated(token, false));
    assert_eq!(
        posts(&i.replay_jobs(now)).len(),
        1,
        "completion before negotiation"
    );
    let mut stale = job.clone();
    stale.last_status = Some("running".into());
    assert!(posts(&i.hydrated(token, now, None, Some(vec![stale]), None)).is_empty());
    assert_eq!(i.jobs[&job.id].as_deref(), Some("ok"));
    assert!(posts(&i.event(now, &Event::JobUpdated { job: job.clone() })).is_empty());

    let mut i = away(Settings::default());
    i.automatic_notifications = Some(false);
    let token = i.begin_sync(i.generation).unwrap();
    job.last_status = Some("running".into());
    i.event(now, &Event::JobUpdated { job: job.clone() });
    let stale = job.clone();
    job.last_status = Some("ok".into());
    assert_eq!(
        posts(&i.event(now, &Event::JobUpdated { job: job.clone() })).len(),
        1,
        "completion during hydration"
    );
    i.hydrated(token, now, None, Some(vec![stale]), None);
    assert_eq!(i.jobs[&job.id].as_deref(), Some("ok"));
    assert!(posts(&i.event(now, &Event::JobUpdated { job })).is_empty());
}

#[test]
fn terminal_job_first_seen_after_disconnect_is_recent_once_and_buffer_is_bounded() {
    let mut i = away(Settings::default());
    let now = Instant::now();
    i.transport(now, false, 1_000);
    let away = i.disconnected_at.unwrap();
    i.transport(now, true, 1_100);
    i.automatic_notifications = Some(false);
    let job = Job {
        id: "job_906".into(),
        last_status: Some("ok".into()),
        last_end: Some(away + 1),
        ..Job::default()
    };
    assert_eq!(
        posts(&i.event(now, &Event::JobUpdated { job: job.clone() })).len(),
        1
    );
    assert!(posts(&i.event(now, &Event::JobUpdated { job })).is_empty());
    let old = Job {
        id: "job_907".into(),
        last_status: Some("ok".into()),
        last_end: Some(away),
        ..Job::default()
    };
    assert!(posts(&i.event(now, &Event::JobUpdated { job: old })).is_empty());
    i.automatic_notifications = None;
    for id in 0..300 {
        i.event(
            now,
            &Event::JobUpdated {
                job: Job {
                    id: format!("job_{id}"),
                    ..Job::default()
                },
            },
        );
    }
    assert_eq!(i.buffered_jobs.len(), 256);
}

#[tokio::test]
async fn client_install_is_atomic_and_equal_transport_tokens_do_not_cross_instances() {
    let (n, mut rx) = producer();
    let a = Client::connect(tessera_client::ServerConfig::default());
    let b = Client::connect(tessera_client::ServerConfig::default());
    assert_ne!(a.instance_id(), b.instance_id());
    assert_eq!(a.instance_id(), a.clone().instance_id());
    let generation = n.attach(Some(a.clone()), "server-a".into()).await;
    n.link(generation, true).await;
    n.with(|i, _| vec![i.acknowledge(vec![903])]);
    let mut old = loop {
        if let Action::Ack(ack) = rx.try_recv().unwrap() {
            break ack;
        }
    };
    old.client_epoch = Some(1); // both transport tokens may be identical
    let generation = n.attach(Some(b.clone()), "server-b".into()).await;
    n.link(generation, true).await;
    let i = n.inner.lock();
    assert_eq!(i.client_instance, Some(b.instance_id()));
    assert_eq!(
        n.client.lock().as_ref().unwrap().instance_id(),
        b.instance_id()
    );
    assert!(!i.accepts_ack(&old, &b));
    old.generation = i.generation;
    old.epoch = i.transport_epoch;
    assert!(
        !i.accepts_ack(&old, &b),
        "equal epoch1 and even restamped generation cannot change the originating instance"
    );
    old.client_instance = Some(b.instance_id());
    assert!(i.accepts_ack(&old, &b));
    a.close();
    b.close();
}

#[tokio::test]
async fn previews_off_persists_first_then_live_and_failed_save_keeps_off() {
    let dir = tempfile::tempdir().unwrap();
    let (mut n, _) = producer();
    n.file = dir.path().join("notifications.json");
    n.inner.lock().stored.settings.previews = true;
    n.apply_settings(n.settings()).await.unwrap();
    let n = std::sync::Arc::new(n);
    let (started, paused) = tokio::sync::oneshot::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let other = n.clone();
    let verify_live = other.clone();
    let setter = tokio::spawn(async move {
        other
            .apply_settings_with(
                Settings {
                    previews: false,
                    ..Settings::default()
                },
                move |file, data| {
                    started.send(()).unwrap();
                    resume.recv().unwrap();
                    crate::private_file::write_verified(file, data)?;
                    assert!(
                        verify_live.settings().previews,
                        "persisted off before live changes"
                    );
                    let disk: Stored = serde_json::from_slice(&std::fs::read(file)?).unwrap();
                    assert!(!disk.settings.previews);
                    Ok(())
                },
            )
            .await
    });
    paused.await.unwrap();
    assert!(n.settings().previews, "transition has not returned yet");
    assert!(
        n.dispatch.try_lock().is_err(),
        "final Linux dispatch cannot slip through persistence"
    );
    let before: Stored = serde_json::from_slice(&std::fs::read(&n.file).unwrap()).unwrap();
    assert!(before.settings.previews);
    release.send(()).unwrap();
    setter.await.unwrap().unwrap();
    assert!(
        n.file.exists(),
        "successful previews-off save must not delete the policy file"
    );
    let persisted: Stored = serde_json::from_slice(&std::fs::read(&n.file).unwrap()).unwrap();
    assert!(!persisted.settings.previews);
    let note = Note::new(
        "test-payload",
        None,
        Kind::Agent,
        "SECRET_TITLE",
        "SECRET_BODY".into(),
    );
    let payload = pull::post_of(&note.payload(&n.settings()));
    assert_eq!(payload.title, "Tessera");
    assert_eq!(payload.body, "New activity. Open Tessera for details.");
    assert_eq!(payload.public_body, "A message for you");
    assert!(payload.sub.is_none());
    n.inner.lock().stored.settings.previews = true;
    n.apply_settings(n.settings()).await.unwrap();
    let e = n
        .apply_settings_with(
            Settings {
                previews: false,
                ..Settings::default()
            },
            |_, _| Err(std::io::Error::other("failed save")),
        )
        .await
        .unwrap_err();
    assert!(e.contains("this phone might still show previews until it can save"));
    assert!(
        !n.settings().previews && !n.file.exists(),
        "stale persisted on must be removed"
    );
    assert_eq!(note.payload(&n.settings()).title, "Tessera");
}

#[test]
fn reconnect_retries_preserve_first_disconnect_cutoff_and_accepted_set_is_bounded() {
    let mut i = away(Settings::default());
    let now = Instant::now();
    i.transport(now, false, 1_000);
    assert_eq!(i.disconnected_at, Some(1_000));
    let first = i.disconnected_at.unwrap();
    for retry in 1..10 {
        i.transport(now + Duration::from_secs(retry), false, 2_000 + retry * 100);
        assert_eq!(i.disconnected_at, Some(first));
    }
    i.transport(now, true, 1_100);
    i.transport(now, false, 9_000);
    assert_eq!(
        i.disconnected_at,
        Some(9_000),
        "a new outage gets a fresh cutoff"
    );
    i.transport(now, false, 9_900);
    assert_eq!(i.disconnected_at, Some(9_000));
    i.transport(now, true, 10_000);
    i.automatic_notifications = Some(false);
    let job = Job {
        id: "finished-between-retries".into(),
        last_status: Some("ok".into()),
        last_end: Some(9_001),
        ..Default::default()
    };
    assert_eq!(posts(&i.event(now, &Event::JobUpdated { job })).len(), 1);
    for id in 0..600 {
        let mut note = Note::new(
            &format!("n{id}"),
            None,
            Kind::Agent,
            "Tessera",
            String::new(),
        );
        note.notification_id = Some(id);
        note.generation = i.generation;
        note.transport_epoch = i.transport_epoch;
        i.accepted(&note, true);
    }
    assert_eq!(i.accepted_receipts[""].len(), 512);
    assert_eq!(i.accepted_receipts[""][0], 88);
}

#[cfg(unix)]
#[tokio::test]
async fn previews_off_save_survives_protected_ancestor_and_keeps_written_file() {
    let dir = crate::private_file::ProtectedAppDir::new();
    let (mut n, _) = producer();
    n.file = dir.app.join("notifications.json");
    n.inner.lock().stored.settings.previews = true;
    n.apply_settings(n.settings()).await.unwrap();
    n.apply_settings(Settings {
        previews: false,
        ..Settings::default()
    })
    .await
    .unwrap();
    assert!(n.file.exists());
    let disk: Stored = serde_json::from_slice(&std::fs::read(&n.file).unwrap()).unwrap();
    assert!(!disk.settings.previews && !n.settings().previews);
    // A later error must not delete a correctly installed off-policy file.
    let error = n
        .apply_settings_with(
            Settings {
                previews: false,
                ..Settings::default()
            },
            |file, bytes| {
                crate::private_file::write_verified(file, bytes)?;
                Err(std::io::Error::other("late save error"))
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("this phone might still show previews"));
    assert!(n.file.exists());
    let disk: Stored = serde_json::from_slice(&std::fs::read(&n.file).unwrap()).unwrap();
    assert!(!disk.settings.previews);
}

#[cfg(any(desktop, mobile))]
#[tokio::test]
async fn real_connection_lifecycle_serializes_connect_disconnect_and_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let state = std::sync::Arc::new(crate::AppState {
        file: dir.path().join("servers.json"),
        config: Mutex::new(crate::ClientConfig::default()),
        conn: Mutex::new(None),
        lifecycle: tokio::sync::Mutex::new(()),
        credentials: crate::credential_write::CredentialWrites::default(),
    });
    let (n, _) = producer();
    let n = std::sync::Arc::new(n);
    let held_dispatch = n.dispatch.lock().await;
    let other = state.clone();
    let notifier = n.clone();
    let (started, queued) = tokio::sync::oneshot::channel();
    let first = tokio::spawn(async move {
        let server = crate::SavedServer {
            id: "first".into(),
            ..Default::default()
        };
        started.send(()).unwrap();
        let (_lifecycle, installed) = other.transition_connection(&notifier, Some(&server)).await;
        installed.unwrap().client
    });
    queued.await.unwrap();
    assert!(
        state.lifecycle.try_lock().is_err(),
        "connect holds lifecycle while waiting for dispatch"
    );
    let other = state.clone();
    let notifier = n.clone();
    let (started, queued) = tokio::sync::oneshot::channel();
    let disconnect = tokio::spawn(async move {
        started.send(()).unwrap();
        let (_lifecycle, installed) = other.transition_connection(&notifier, None).await;
        assert!(installed.is_none());
        assert!(other.conn.lock().is_none());
        assert!(!notifier.inner.lock().attached);
    });
    queued.await.unwrap();
    let other = state.clone();
    let notifier = n.clone();
    let (started, queued) = tokio::sync::oneshot::channel();
    let replacement = tokio::spawn(async move {
        let server = crate::SavedServer {
            id: "replacement".into(),
            ..Default::default()
        };
        started.send(()).unwrap();
        let (_lifecycle, installed) = other.transition_connection(&notifier, Some(&server)).await;
        installed.unwrap().client
    });
    queued.await.unwrap();
    assert!(!first.is_finished() && !disconnect.is_finished() && !replacement.is_finished());
    drop(held_dispatch);
    let first = first.await.unwrap();
    disconnect.await.unwrap();
    let replacement = replacement.await.unwrap();
    assert_ne!(first.instance_id(), replacement.instance_id());
    let current = state
        .conn
        .lock()
        .as_ref()
        .map(|(id, client)| (id.clone(), client.instance_id()))
        .unwrap();
    assert_eq!(current, ("replacement".into(), replacement.instance_id()));
    assert_eq!(
        n.inner.lock().client_instance,
        Some(replacement.instance_id())
    );
    assert_eq!(
        n.client.lock().as_ref().unwrap().instance_id(),
        replacement.instance_id()
    );
    let (_lifecycle, _) = state.transition_connection(&n, None).await;
    assert!(state.conn.lock().is_none() && n.client.lock().is_none());
    assert!(!n.inner.lock().attached);
}

#[test]
fn modern_server_owns_approvals_replies_and_terminal_errors() {
    let t = Instant::now();
    let mut i = away(Settings::default());
    i.automatic_notifications = Some(true);
    i.event(t, &updated("s1", "running"));
    assert!(posts(&i.event(t, &asked("s1", "a1"))).is_empty());
    assert!(posts(&i.event(t, &Event::Notice { session_id: Some("s1".into()), level: NoticeLevel::Error, message: "Transient tool retry".into() })).is_empty());
    i.event(t + LONG_TURN, &updated("s1", "idle"));
    assert!(posts(&i.tick(t + LONG_TURN + SETTLE)).is_empty(), "no inferred duplicate completion");
    let mut notification: UserNotification = serde_json::from_value(json!({ "id": 5001, "title": "Finished", "body": "Answer", "session_id": "s1", "created_at": 1, "due_at": 2, "sent": true, "seen": false, "kind": "finished" })).unwrap();
    let out = i.agent_message(t + LONG_TURN + SETTLE, &notification);
    assert_eq!(posts(&out).len(), 1);
    assert_eq!(posts(&out)[0].key, "s1");
    notification.id += 1;
    notification.source_key = "job_stable".into();
    notification.session_id = Some("another_run".into());
    let out = i.agent_message(t + LONG_TURN + SETTLE, &notification);
    assert_eq!(posts(&out)[0].key, "job_stable", "job key survives fresh run chats");
    assert!(i.agent_seen(&[notification.id]).contains(&Action::Withdraw("job_stable".into())));
}

#[test]
fn server_helper_and_quiet_messages_never_make_native_popups() {
    let mut i = away(Settings { subagents: true, ..Settings::default() });
    let t = Instant::now();
    for (id, kind, quiet) in [(7001, "helper", false), (7002, "job", true)] {
        let n = serde_json::from_value(json!({ "id": id, "title": "Quiet", "body": "Report", "session_id": "s1", "created_at": 1, "due_at": 2, "sent": true, "seen": false, "kind": kind, "quiet": quiet })).unwrap();
        assert!(posts(&i.agent_message(t, &n)).is_empty());
    }
}
