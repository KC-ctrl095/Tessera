//! The notification center (`list_notification_inbox`): who sees what,
//! read history across restarts, and what each scheduled run leaves there
//! (how it ended, its log, the end of its schedule).

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tessera_core::{Engine, EngineOptions, config::Paths};
use tessera_proto::*;

async fn open(dir: &std::path::Path) -> Engine {
    std::fs::write(dir.join("config.toml"), "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n").unwrap();
    Engine::open(Paths::at(dir), EngineOptions { scheduler: false })
        .await
        .unwrap()
}

async fn call<T: serde::de::DeserializeOwned>(e: &Engine, req: Request, d: Option<&str>) -> T {
    serde_json::from_value(e.handle(req, d).await.unwrap()).unwrap()
}

async fn chat(e: &Engine, d: &str, private: bool) -> SessionInfo {
    call(
        e,
        Request::CreateSession {
            cwd: None,
            model: None,
            title: Some("Inbox chat".into()),
            bot_id: None,
            private,
            lean: false,
        },
        Some(d),
    )
    .await
}

async fn notify(e: &Engine, d: &str, session: Option<&str>, delay_secs: u64) -> UserNotification {
    call(
        e,
        Request::SendNotification {
            title: "Reminder".into(),
            body: "Check the report".into(),
            session_id: session.map(Into::into),
            delay_secs,
        },
        Some(d),
    )
    .await
}

async fn inbox(e: &Engine, d: &str) -> Vec<UserNotification> {
    call(e, Request::ListNotificationInbox, Some(d)).await
}

#[tokio::test]
async fn private_ones_stay_in_memory_with_their_device_and_go_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let e = open(dir.path()).await;
    let (phone, _) = e.store().add_device("Phone").unwrap();
    let (desk, _) = e.store().add_device("Desk").unwrap();
    let s = chat(&e, &phone, true).await;
    let mut events = e.subscribe();
    let n = notify(&e, &phone, Some(&s.id), 0).await;
    let scheduled = notify(&e, &phone, Some(&s.id), 3600).await;
    let public = notify(&e, &desk, None, 0).await;
    assert!(n.id != scheduled.id && n.id != public.id && scheduled.id != public.id);
    assert!(n.private && scheduled.private && !public.private);
    for payload in [&n, &scheduled] {
        assert_eq!(
            (
                payload.title.as_str(),
                payload.body.as_str(),
                payload.from.as_str()
            ),
            ("Tessera", "", "")
        );
    }
    let mut live = None;
    while let Ok(event) = events.try_recv() {
        if let Event::Notification { notification } = event
            && notification.id == n.id
        {
            live = Some(notification);
        }
    }
    assert_eq!(live.unwrap(), n, "live payload is already privacy-safe");
    let pending: PendingNotifications = call(&e, Request::PendingNotifications, Some(&phone)).await;
    assert!(
        pending
            .new
            .iter()
            .filter(|x| x.private)
            .all(|x| x.title == "Tessera" && x.body.is_empty() && x.from.is_empty())
    );
    let conn = rusqlite::Connection::open(e.paths().db_file()).unwrap();
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM notifications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "only the public one is in the database");
    drop(conn);
    assert_eq!(inbox(&e, &phone).await.len(), 3);
    assert_eq!(
        inbox(&e, &desk)
            .await
            .iter()
            .map(|n| n.id)
            .collect::<Vec<_>>(),
        [public.id]
    );
    let p: PendingNotifications = call(&e, Request::PendingNotifications, Some(&desk)).await;
    assert!(p.new.iter().all(|x| x.id == public.id) && p.next_due.is_none());
    for req in [
        Request::AckNotifications { ids: vec![n.id] },
        Request::SeenNotifications {
            ids: vec![n.id],
            session_id: None,
        },
        Request::CancelNotification { id: scheduled.id },
        Request::ClearNotifications { ids: vec![n.id] },
    ] {
        assert_eq!(
            e.handle(req, Some(&desk)).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }
    assert!(!e.store().notification(n.id).unwrap().unwrap().seen);
    let event = Event::Notification {
        notification: n.clone(),
    };
    assert!(e.event_for_device(&event, &phone));
    assert!(!e.event_for_device(&event, &desk));
    // Signing the phone out takes its private chat and all of this along.
    let _: Value = call(&e, Request::RevokeDevice { id: phone.clone() }, Some(&desk)).await;
    assert!(e.store().notification(n.id).unwrap().is_none());
    assert!(e.store().notification(scheduled.id).unwrap().is_none());
    assert!(
        !e.event_for_device(&event, &desk),
        "a queued event stays hidden"
    );
    for req in [
        Request::ListNotificationInbox,
        Request::PendingNotifications,
        Request::SeenNotifications {
            ids: vec![public.id],
            session_id: None,
        },
    ] {
        assert_eq!(
            e.handle(req, Some(&phone)).await.unwrap_err().code,
            ErrorCode::Unauthorized
        );
    }
}

#[tokio::test]
async fn read_history_survives_a_restart_and_a_deleted_chat_takes_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let e = open(dir.path()).await;
    let (d, _) = e.store().add_device("Desk").unwrap();
    let s = chat(&e, &d, false).await;
    let n = notify(&e, &d, Some(&s.id), 0).await;
    let general = notify(&e, &d, None, 0).await;
    let _: Value = call(
        &e,
        Request::SetDeviceNotify {
            id: d.clone(),
            notify: false,
        },
        Some(&d),
    )
    .await;
    assert_eq!(inbox(&e, &d).await.len(), 2, "whatever the device's choice");
    let _: Value = call(&e, Request::AckNotifications { ids: vec![n.id] }, Some(&d)).await;
    assert!(
        !inbox(&e, &d).await.iter().any(|x| x.seen),
        "shown isn't read"
    );
    let _: Value = call(
        &e,
        Request::SeenNotifications {
            ids: vec![n.id, n.id],
            session_id: None,
        },
        Some(&d),
    )
    .await;
    let private = chat(&e, &d, true).await;
    let pn = notify(&e, &d, Some(&private.id), 3600).await;
    drop(e);
    let e = open(dir.path()).await;
    let items = inbox(&e, &d).await;
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|x| x.id == n.id && x.seen));
    assert!(e.store().notification(pn.id).unwrap().is_none());
    // Clear forgets it everywhere.
    let mut events = e.subscribe();
    let _: Value = call(
        &e,
        Request::ClearNotifications { ids: vec![n.id] },
        Some(&d),
    )
    .await;
    assert!(matches!(events.try_recv(), Ok(Event::NotificationsSeen { ids }) if ids == [n.id]));
    assert_eq!(inbox(&e, &d).await.len(), 1);
    // A deleted chat's go with it (they don't become about nothing).
    let n2 = notify(&e, &d, Some(&s.id), 0).await;
    let _: Value = call(&e, Request::DeleteSession { session_id: s.id }, Some(&d)).await;
    assert!(e.store().notification(n2.id).unwrap().is_none());
    assert_eq!(
        inbox(&e, &d).await.iter().map(|x| x.id).collect::<Vec<_>>(),
        [general.id]
    );
}

#[tokio::test]
async fn unknown_and_machine_logins_have_no_notification_center() {
    let dir = tempfile::tempdir().unwrap();
    let e = open(dir.path()).await;
    let (machine, _) = e.store().add_device_as("Build box", true).unwrap();
    for d in ["unknown", machine.as_str()] {
        assert_eq!(
            e.handle(Request::ListNotificationInbox, Some(d))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
    }
}

// ---- scheduled runs -----------------------------------------------------------

async fn save_job(h: &Harness, job: Job) -> Result<Job, ApiError> {
    h.engine
        .handle(Request::SaveJob { job }, None)
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

fn job(h: &Harness, name: &str) -> Job {
    Job {
        name: name.into(),
        schedule: "@every 1h".into(),
        prompt: format!("Do the {name}"),
        cwd: h.work.display().to_string(),
        enabled: true,
        permission: PermissionMode::Auto,
        ..Default::default()
    }
}

async fn run(h: &Harness, id: &str) -> SessionInfo {
    let info: SessionInfo = call(&h.engine, Request::RunJob { id: id.into() }, None).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&info.id))
        .await
        .expect("the run did not end");
    // Its end is recorded right after the turn.
    tokio::time::sleep(Duration::from_millis(100)).await;
    info
}

async fn jobs(h: &Harness) -> Vec<Job> {
    call(&h.engine, Request::ListJobs, None).await
}

async fn all(h: &Harness) -> Vec<UserNotification> {
    call(&h.engine, Request::ListNotificationInbox, None).await
}

fn alerts(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<UserNotification> {
    let mut out = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::Notification { notification } = e {
            out.push(notification);
        }
    }
    out
}

#[tokio::test]
async fn a_finished_run_is_logged_without_a_bell_entry_or_alert() {
    let h = harness(
        vec![Reply::Text("All quiet: nothing new today.".into())],
        100_000,
    )
    .await;
    let j = save_job(&h, job(&h, "morning check")).await.unwrap();
    assert_eq!((j.runs, j.last_status.as_deref()), (0, None));
    let mut events = h.engine.subscribe();
    let s = run(&h, &j.id).await;
    let j = jobs(&h).await.pop().unwrap();
    assert_eq!(j.last_status.as_deref(), Some("ok"));
    assert_eq!(j.last_session.as_deref(), Some(s.id.as_str()));
    assert_eq!(j.runs, 1);
    assert!(j.last_end.is_some_and(|t| t >= j.last_run.unwrap()));
    let runs: Vec<JobRun> = call(&h.engine, Request::ListJobRuns { id: j.id.clone() }, None).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].outcome.as_str(), runs[0].session_id.as_str()),
        ("ok", s.id.as_str())
    );
    assert!(runs[0].ended_at.is_some());
    // Routine success stays in run history, not another bell entry.
    assert!(!all(&h).await.iter().any(|n| n.kind == "job"));
    assert!(alerts(&mut events).is_empty());
    // The prompt was kept before the turn started, as the job's words.
    let entries = h.entries(&s.id).await;
    assert!(
        matches!(&entries[0].body, EntryBody::User { author: Some(a), .. } if a == &format!("job:{}", j.id))
    );
}

#[tokio::test]
async fn a_run_that_pauses_alerts_and_one_the_user_stops_is_only_listed() {
    let script: Vec<Reply> = (0..12)
        .map(|_| Reply::Tools(vec![("read".into(), json!({"path": "missing.txt"}))]))
        .collect();
    let h = harness_agent(script, 100_000, "max_steps = 4").await;
    let j = save_job(&h, job(&h, "stubborn check")).await.unwrap();
    let mut events = h.engine.subscribe();
    let s = run(&h, &j.id).await;
    let j = jobs(&h).await.pop().unwrap();
    assert_eq!(j.last_status.as_deref(), Some("paused"));
    let sent = alerts(&mut events);
    let alert = sent.iter().find(|n| n.kind == "job").expect("an alert");
    assert_eq!(alert.title, "Scheduled task paused");
    assert_eq!(alert.session_id.as_deref(), Some(s.id.as_str()));
    assert!(!alert.quiet);

    // Stopped by the user: listed, quiet.
    h.mock.clear();
    h.mock
        .push(Reply::Slow(Duration::from_secs(5), "late".into()));
    let info: SessionInfo = call(&h.engine, Request::RunJob { id: j.id.clone() }, None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _: Value = call(
        &h.engine,
        Request::Abort {
            session_id: info.id.clone(),
        },
        None,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(20), h.engine.wait_idle(&info.id))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let j = jobs(&h).await.pop().unwrap();
    assert_eq!(j.last_status.as_deref(), Some("stopped"));
    let item = all(&h)
        .await
        .into_iter()
        .find(|n| n.session_id.as_deref() == Some(info.id.as_str()))
        .unwrap();
    assert!(item.quiet && item.title == "Scheduled task stopped");
    let runs: Vec<JobRun> = call(&h.engine, Request::ListJobRuns { id: j.id }, None).await;
    assert_eq!(
        runs.iter().map(|r| r.outcome.as_str()).collect::<Vec<_>>(),
        ["stopped", "paused"]
    );
}

#[tokio::test]
async fn a_schedule_ends_after_its_runs_or_when_a_run_says_its_goal_is_met() {
    let h = harness(
        vec![
            // The one-off.
            Reply::Text("Done once.".into()),
            // The ticket check: the answer is there, so it ends its schedule.
            Reply::Tools(vec![(
                "schedule".into(),
                json!({"action": "end", "reason": "the ticket was answered"}),
            )]),
            Reply::Text("The ticket has an answer; no more checks.".into()),
            // A plain chat can't end a job's schedule.
            Reply::Tools(vec![("schedule".into(), json!({"action": "end"}))]),
            Reply::Text("ok".into()),
        ],
        100_000,
    )
    .await;
    let once = save_job(
        &h,
        Job {
            max_runs: Some(1),
            ..job(&h, "one-off")
        },
    )
    .await
    .unwrap();
    run(&h, &once.id).await;
    let once = jobs(&h)
        .await
        .into_iter()
        .find(|j| j.id == once.id)
        .unwrap();
    assert!(!once.enabled && once.next_run.is_none());
    assert_eq!(once.ended.as_deref(), Some("it ran once, as set"));
    assert!(
        all(&h)
            .await
            .iter()
            .any(|n| n.title == "Scheduled task ended" && n.body == "one-off: it ran once, as set.")
    );
    // Turning it on again needs a higher limit.
    let err = save_job(
        &h,
        Job {
            enabled: true,
            ..once.clone()
        },
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("max_runs"), "{}", err.message);
    let again = save_job(
        &h,
        Job {
            enabled: true,
            max_runs: Some(2),
            ..once
        },
    )
    .await
    .unwrap();
    assert!(again.enabled && again.ended.is_none() && again.next_run.is_some());

    let ticket = save_job(&h, job(&h, "ticket check")).await.unwrap();
    let s = run(&h, &ticket.id).await;
    let ticket = jobs(&h)
        .await
        .into_iter()
        .find(|j| j.id == ticket.id)
        .unwrap();
    assert!(!ticket.enabled);
    assert_eq!(
        ticket.ended.as_deref(),
        Some("its goal was met (the ticket was answered)")
    );
    assert_eq!(
        ticket.last_status.as_deref(),
        Some("ok"),
        "the run itself finished"
    );
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("no more runs"),
        "{results:?}"
    );

    let c = h.session().await;
    h.send(&c.id, "stop that job").await;
    h.settle(&c.id).await;
    let results = tool_results(&h.entries(&c.id).await);
    assert!(
        results[0].1 && results[0].2.contains("this chat isn't one"),
        "{results:?}"
    );
}

#[tokio::test]
async fn limits_an_older_app_leaves_out_stay_and_zero_clears_them() {
    let h = harness(vec![], 100_000).await;
    let j = save_job(
        &h,
        Job {
            max_runs: Some(3),
            until: Some(tessera_core::util::now_ms() + 7 * 86_400_000),
            ..job(&h, "week of checks")
        },
    )
    .await
    .unwrap();
    // What an older app sends: the job without the fields it doesn't know.
    let old_app: Job = serde_json::from_value(json!({
        "id": j.id, "name": "week of checks", "schedule": "@every 2h", "prompt": "Do it",
        "cwd": j.cwd, "enabled": true, "permission": "auto"
    }))
    .unwrap();
    let saved = save_job(&h, old_app).await.unwrap();
    assert_eq!((saved.max_runs, saved.until), (j.max_runs, j.until));
    assert_eq!(saved.schedule, "@every 2h");
    let cleared = save_job(
        &h,
        Job {
            max_runs: Some(0),
            until: Some(0),
            ..saved
        },
    )
    .await
    .unwrap();
    assert_eq!((cleared.max_runs, cleared.until), (None, None));
    // An end date already past: not saved as on.
    let err = save_job(
        &h,
        Job {
            until: Some(tessera_core::util::now_ms() - 60_000),
            ..job(&h, "too late")
        },
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("until"), "{}", err.message);
}

#[tokio::test]
async fn the_schedule_tool_gives_temporary_checks_an_end() {
    let h = harness_agent(
        vec![
            Reply::Tools(vec![(
                "schedule".into(),
                json!({"action": "create", "name": "Ticket check", "schedule": "@every 12h",
                       "prompt": "Check the ticket; notify me and end the schedule once it has an answer.",
                       "until": "in 3d", "max_runs": 6}),
            )]),
            Reply::Tools(vec![("schedule".into(), json!({"action": "list"}))]),
            Reply::Text("Scheduled.".into()),
        ],
        100_000,
        "permission = \"auto\"",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "check that ticket twice a day until it's answered")
        .await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        !results[0].1 && results[0].2.contains("It ends after 6 run(s)."),
        "{results:?}"
    );
    assert!(
        results[1].2.contains("run 0/6") && results[1].2.contains("until "),
        "{results:?}"
    );
    let j = jobs(&h).await.pop().unwrap();
    assert_eq!(j.max_runs, Some(6));
    let in_3d = tessera_core::util::now_ms() + 3 * 86_400_000;
    assert!(j.until.is_some_and(|u| u.abs_diff(in_3d) < 60_000));
}

#[tokio::test]
async fn continuing_a_paused_run_clears_its_end_and_does_not_start_a_new_run() {
    let h = harness_agent(
        (0..4)
            .map(|_| Reply::Tools(vec![("read".into(), json!({"path": "missing.txt"}))]))
            .collect(),
        100_000,
        "max_steps = 4",
    )
    .await;
    let j = save_job(&h, job(&h, "continued check")).await.unwrap();
    let s = run(&h, &j.id).await;
    let before = jobs(&h).await.pop().unwrap();
    assert_eq!(before.last_status.as_deref(), Some("paused"));
    h.mock.clear();
    h.mock
        .push(Reply::Slow(Duration::from_secs(3), "Now finished.".into()));
    h.send(&s.id, "Continue the check.").await;
    let live = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let j = jobs(&h).await.pop().unwrap();
            if j.last_status.as_deref() == Some("running") {
                break j;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the continued run was not marked running");
    assert_eq!((live.runs, live.last_run), (1, before.last_run));
    assert!(live.last_end.is_none());
    let runs: Vec<JobRun> = call(&h.engine, Request::ListJobRuns { id: j.id.clone() }, None).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, "running");
    assert!(runs[0].ended_at.is_none());
    let error = h
        .engine
        .handle(Request::RunJob { id: j.id }, None)
        .await
        .unwrap_err();
    assert!(error.message.contains("still working"), "{error:?}");
    h.settle(&s.id).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let done = jobs(&h).await.pop().unwrap();
    assert_eq!((done.runs, done.last_status.as_deref()), (1, Some("ok")));
    assert!(done.last_end.is_some());
    let items: Vec<_> = all(&h)
        .await
        .into_iter()
        .filter(|n| n.session_id.as_deref() == Some(s.id.as_str()))
        .collect();
    assert_eq!(items.len(), 2, "distinct outcomes remain in the inbox");
    assert!(
        items
            .iter()
            .any(|n| n.title == "Scheduled task recovered" && n.quiet)
    );
}

#[tokio::test]
async fn stale_two_device_reads_clears_and_acks_cannot_consume_a_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let e = open(dir.path()).await;
    let (a, _) = e.store().add_device("A").unwrap();
    let (b, _) = e.store().add_device("B").unwrap();
    let s = chat(&e, &a, false).await;
    let n = tessera_core::store::NewNotification {
        session_id: Some(s.id),
        kind: "job".into(),
        title: "First outcome".into(),
        ..Default::default()
    };
    let first = e.store().record_notification(&n).unwrap().0;
    let seen_by_b = inbox(&e, &b).await;
    assert_eq!(seen_by_b[0].id, first.id);
    e.store().mark_notifications_seen(&[first.id], &[]).unwrap();
    let second = e
        .store()
        .record_notification(&tessera_core::store::NewNotification {
            title: "New outcome".into(),
            replace: tessera_core::store::Replace::Same,
            ..n
        })
        .unwrap()
        .0;
    assert_ne!(first.id, second.id, "id carries the revision");
    for req in [
        Request::SeenNotifications {
            ids: vec![first.id],
            session_id: None,
        },
        Request::ClearNotifications {
            ids: vec![first.id],
        },
        Request::AckNotifications {
            ids: vec![first.id],
        },
    ] {
        let _ = e.handle(req, Some(&b)).await; // gone revisions may return not_found
    }
    assert!(!e.store().notification(second.id).unwrap().unwrap().seen);
    let p: PendingNotifications = call(&e, Request::PendingNotificationsV2, Some(&b)).await;
    assert_eq!(p.new.iter().map(|n| n.id).collect::<Vec<_>>(), [second.id]);
}

#[tokio::test]
async fn scheduled_notify_calls_share_one_source_across_fresh_run_chats() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "notify".into(),
                json!({"title":"Monitor", "body":"Healthy at 18:15"}),
            )]),
            Reply::Text("Checked.".into()),
            Reply::Tools(vec![(
                "notify".into(),
                json!({"title":"Monitor", "body":"HEALTHY at 18:30"}),
            )]),
            Reply::Text("Checked again.".into()),
            Reply::Tools(vec![(
                "notify".into(),
                json!({"title":"Monitor", "body":"A new failure needs you", "urgent":true}),
            )]),
            Reply::Text("Waiting for you.".into()),
        ],
        100_000,
    )
    .await;
    let j = save_job(&h, job(&h, "frequent monitor")).await.unwrap();
    let mut events = h.engine.subscribe();
    let first = run(&h, &j.id).await;
    assert!(
        alerts(&mut events).is_empty(),
        "scheduled updates start in the digest"
    );
    let second = run(&h, &j.id).await;
    assert_ne!(
        first.id, second.id,
        "fresh run chats must not bypass a job's policy"
    );
    assert!(
        alerts(&mut events).is_empty(),
        "timestamp-only status does not alert again"
    );
    let items = all(&h).await;
    let messages = items
        .iter()
        .filter(|n| n.kind == "message")
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2, "different text is not a repeat");
    assert!(
        messages
            .iter()
            .all(|n| n.source_key == format!("job_{}", j.id)
                && n.level == tessera_proto::NotificationLevel::Digest)
    );
    assert!(
        messages
            .iter()
            .any(|n| n.session_id.as_deref() == Some(second.id.as_str()))
    );
    run(&h, &j.id).await;
    let events = alerts(&mut events);
    assert_eq!(
        events.len(),
        1,
        "a real urgent failure bypasses the ordinary source limit"
    );
    assert!(events[0].urgent);
}

#[tokio::test]
async fn foreground_chat_presence_is_per_device_and_does_not_mark_global_read() {
    let dir = tempfile::tempdir().unwrap();
    let e = open(dir.path()).await;
    let (a, _) = e.store().add_device("Laptop").unwrap();
    let (b, _) = e.store().add_device("Phone").unwrap();
    let s = chat(&e, &a, false).await;
    let n = notify(&e, &a, Some(&s.id), 0).await;
    call::<Value>(&e, Request::AckNotifications { ids: vec![n.id] }, Some(&b)).await;
    call::<Value>(
        &e,
        Request::NotificationPresence {
            active: true,
            session_ids: vec![s.id.clone()],
        },
        Some(&a),
    )
    .await;
    let p: tessera_proto::PendingNotifications =
        call(&e, Request::PendingNotificationsV2, Some(&b)).await;
    assert!(p.new.is_empty(), "phone already acknowledged this item");
    assert!(
        !p.withdraw.contains(&s.id),
        "a desktop tile does not withdraw the phone card"
    );
    assert!(!inbox(&e, &a).await[0].seen);
    let next: UserNotification = call(
        &e,
        Request::SendNotification {
            title: "A different first occurrence".into(),
            body: "New text".into(),
            session_id: Some(s.id.clone()),
            delay_secs: 0,
        },
        Some(&a),
    )
    .await;
    let p: PendingNotifications = call(&e, Request::PendingNotificationsV2, Some(&b)).await;
    assert!(
        p.new.iter().any(|n| n.id == next.id),
        "new phone alerts survive a visible desktop tile"
    );
    let p: PendingNotifications = call(&e, Request::PendingNotificationsV2, Some(&a)).await;
    assert!(
        !p.new.iter().any(|n| n.id == next.id),
        "viewing device does not buzz twice"
    );
    call::<Value>(
        &e,
        Request::ClearNotifications { ids: vec![n.id] },
        Some(&a),
    )
    .await;
    let p: tessera_proto::PendingNotifications =
        call(&e, Request::PendingNotificationsV2, Some(&b)).await;
    assert!(
        !p.withdraw.contains(&s.id),
        "deleting an older item does not withdraw newer news"
    );
    call::<Value>(
        &e,
        Request::AckNotifications { ids: vec![next.id] },
        Some(&b),
    )
    .await;
    call::<Value>(
        &e,
        Request::ClearNotifications { ids: vec![next.id] },
        Some(&a),
    )
    .await;
    let p: PendingNotifications = call(&e, Request::PendingNotificationsV2, Some(&b)).await;
    assert!(
        p.withdraw.contains(&s.id),
        "deleting the current alert withdraws its posted card"
    );
    let unauthorized = e
        .handle(
            Request::NotificationPresence {
                active: true,
                session_ids: vec![s.id],
            },
            None,
        )
        .await;
    assert!(unauthorized.is_err());
}

#[tokio::test]
async fn notification_policy_rpc_source_controls_quiet_hours_and_test_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let e = open(dir.path()).await;
    let (a, _) = e.store().add_device("Laptop").unwrap();
    let (b, _) = e.store().add_device("Phone").unwrap();
    let first = notify(&e, &a, None, 0).await;
    assert_eq!(first.level, NotificationLevel::Page);
    let p: NotificationPolicy = call(
        &e,
        Request::SetNotificationSource {
            key: first.source_key.clone(),
            level: Some(NotificationLevel::Digest),
        },
        Some(&a),
    )
    .await;
    assert_eq!(p.sources.len(), 1);
    let n: UserNotification = call(
        &e,
        Request::SendNotification {
            title: "New source result".into(),
            body: "Different text".into(),
            session_id: None,
            delay_secs: 0,
        },
        Some(&a),
    )
    .await;
    assert_eq!(n.level, NotificationLevel::Digest);
    let _: NotificationPolicy = call(
        &e,
        Request::SetNotificationSource {
            key: first.source_key,
            level: None,
        },
        Some(&a),
    )
    .await;
    let mut p: NotificationPolicy = call(&e, Request::GetNotificationPolicy, Some(&a)).await;
    assert!(!p.hourly && !p.quiet && p.daily_at == vec![1080] && p.sources.is_empty());
    p.hourly = true;
    p.quiet = true;
    p.quiet_from = 1;
    p.quiet_to = 0;
    let p: NotificationPolicy =
        call(&e, Request::SetNotificationPolicy { policy: p }, Some(&a)).await;
    assert!(p.hourly && p.quiet);
    let _: Vec<DeviceInfo> = call(
        &e,
        Request::SetDeviceNotify {
            id: b.clone(),
            notify: false,
        },
        Some(&a),
    )
    .await;
    let test: UserNotification =
        call(&e, Request::TestNotification { delay_secs: 0 }, Some(&a)).await;
    assert!(test.urgent && !test.quiet && test.level == NotificationLevel::Page);
    let pending: PendingNotifications = call(&e, Request::PendingNotificationsV2, Some(&b)).await;
    assert_eq!(
        pending.new.iter().map(|n| n.id).collect::<Vec<_>>(),
        [test.id],
        "test bypasses device opt-out"
    );
    let reports: Vec<NotificationDelivery> = call(
        &e,
        Request::NotificationDeliveries { id: test.id },
        Some(&a),
    )
    .await;
    assert!(reports.iter().all(|d| !d.delivered));
    let _: Value = call(
        &e,
        Request::AckNotifications { ids: vec![test.id] },
        Some(&b),
    )
    .await;
    let reports: Vec<NotificationDelivery> = call(
        &e,
        Request::NotificationDeliveries { id: test.id },
        Some(&a),
    )
    .await;
    assert!(
        reports.iter().all(|d| !d.delivered),
        "suppression acknowledgements are not OS receipts"
    );
    let _: Value = call(
        &e,
        Request::ReportNotificationDelivery { ids: vec![test.id] },
        Some(&b),
    )
    .await;
    let reports: Vec<NotificationDelivery> = call(
        &e,
        Request::NotificationDeliveries { id: test.id },
        Some(&a),
    )
    .await;
    assert!(reports.iter().any(|d| d.device_id == b && d.delivered));
}
