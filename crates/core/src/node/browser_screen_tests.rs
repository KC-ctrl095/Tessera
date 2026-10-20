//! Deterministic native-screen launch admission races; no real desktop.
use super::*;

fn desktop() -> super::super::desktop::Session {
    super::super::desktop::Session {
        vars: vec![],
        keep: vec![],
        bus: None,
        label: "test display".into(),
    }
}

#[tokio::test]
async fn stop_during_desktop_discovery_cancels_pending_screen_launch() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    paths.ensure().unwrap();
    let nb = NodeBrowser::new(paths.clone());
    let launch = nb.0.begin_screen(0).unwrap();
    let (entered, seen) = oneshot::channel();
    let (_release, release) = oneshot::channel::<()>();
    let inner = nb.0.clone();
    let task_launch = launch.clone();
    let task = tokio::spawn(async move {
        inner
            .open_screen_with("https://example.org/", &task_launch, async move {
                let _ = entered.send(());
                let _ = release.await;
                Some(desktop())
            })
            .await
    });
    seen.await.unwrap();
    nb.stop("test Stop during discovery");
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error, STOPPED_SINCE_START);
    assert!(launch.cancel.is_cancelled());
    assert!(nb.0.screen_launch.lock().pending.is_none());
    assert!(nb.0.screen.lock().is_none());
    assert!(
        !super::dir(&paths).exists(),
        "no profile setup or spawn after cancellation"
    );
}

#[tokio::test]
async fn stop_during_old_browser_shutdown_cancels_that_run_and_pending_screen() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    paths.ensure().unwrap();
    let nb = NodeBrowser::new(paths.clone());
    let launch = nb.0.begin_screen(0).unwrap();
    let stop = CancellationToken::new();
    let (tx, mut rx) = queue::channel(8 << 20, stop.clone());
    *nb.0.run.lock() = Some(Run {
        run: 1,
        tx,
        filter: Filter::new(MAX_TABS),
        own: Arc::new(OwnCalls::default()),
        stop: stop.clone(),
        asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        last_use: Arc::new(Mutex::new(Instant::now())),
        downloads: dir.path().join("downloads"),
        names: HashMap::new(),
        saved: HashMap::new(),
        kept: 0,
        receiving: HashMap::new(),
        pid: None,
        stops: 0,
        uploads: crate::browser::upload::Uploads::new(dir.path().join("uploads")),
        upload_finishes: HashMap::new(),
    });
    let inner = nb.0.clone();
    let task_launch = launch.clone();
    let task = tokio::spawn(async move {
        inner
            .open_screen_with("https://example.org/", &task_launch, async {
                Some(desktop())
            })
            .await
    });
    let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let msg: Value = serde_json::from_slice(&msg.value).unwrap();
    assert_eq!(msg["method"], "Browser.close");
    assert!(
        nb.0.run.lock().is_none(),
        "run removed while graceful close waits"
    );
    nb.stop("test Stop during old browser shutdown");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        STOPPED_SINCE_START
    );
    assert!(
        stop.is_cancelled(),
        "removed old run still killed when its waiter cancels"
    );
    assert!(!super::dir(&paths).exists());
    assert!(nb.0.screen.lock().is_none());
}

#[tokio::test]
async fn stop_before_spawn_refuses_and_stop_before_registration_kills_child() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    paths.ensure().unwrap();
    let nb = NodeBrowser::new(paths.clone());
    let launch = nb.0.begin_screen(0).unwrap();
    nb.stop("before spawn");
    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.arg("30").process_group(0);
    assert!(
        nb.0.spawn_screen(
            &launch,
            &mut cmd,
            crate::browser::process::ExitLease::new(None, CancellationToken::new())
        )
        .is_err()
    );
    let launch = nb.0.begin_screen(0).unwrap();
    let mut child =
        nb.0.spawn_screen(
            &launch,
            &mut cmd,
            crate::browser::process::ExitLease::new(None, CancellationToken::new()),
        )
        .unwrap();
    let pid = child.pgid.unwrap();
    nb.stop("after spawn before registration");
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            nb.0.screen_child_ready(&launch, &mut child)
        )
        .await
        .unwrap()
        .unwrap_err(),
        STOPPED_SINCE_START
    );
    assert!(child.child.is_none());
    assert!(child.pgid.is_none(), "Drop must not signal a reaped group");
    // SAFETY: checking our reaped child, without sending it a signal.
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert!(nb.0.screen.lock().is_none());
    // A file Stop/resume before the node's next reload still revokes the old generation.
    let launch = nb.0.begin_screen(0).unwrap();
    let latch = super::super::StopLatch::stop(&paths, 0).unwrap();
    super::super::StopLatch::set(&paths, false, latch.stops).unwrap();
    assert!(
        nb.0.spawn_screen(
            &launch,
            &mut cmd,
            crate::browser::process::ExitLease::new(None, CancellationToken::new())
        )
        .is_err()
    );
    assert!(nb.0.begin_screen(0).is_err());
}

#[tokio::test]
async fn early_readiness_exit_kills_helpers_while_pinned_and_clears_pgid() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    paths.ensure().unwrap();
    let nb = NodeBrowser::new(paths);
    let launch = nb.0.begin_screen(0).unwrap();
    let pidfile = dir.path().join("helper.pid");
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args([
        "-c",
        &format!("sleep 60 & echo $! > '{}'; exit 0", pidfile.display()),
    ])
    .process_group(0);
    let guard = nb.0.profile_gate.clone().lock_owned().await;
    let done = CancellationToken::new();
    let lease = crate::browser::process::ExitLease::new(Some(guard), done.clone());
    let mut child = nb.0.spawn_screen(&launch, &mut cmd, lease).unwrap();
    assert!(nb.0.screen_child_ready(&launch, &mut child).await.is_err());
    assert!(child.pgid.is_none() && child.child.is_none());
    assert!(done.is_cancelled());
    assert!(nb.0.profile_gate.try_lock().is_ok());
    let pid = std::fs::read_to_string(pidfile).unwrap();
    // A helper may be a zombie until the host's reaper collects it, not live.
    let ended = || {
        std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
            .ok()
            .is_none_or(|s| {
                s.rsplit_once(')')
                    .is_some_and(|(_, s)| s.trim_start().starts_with('Z'))
            })
    };
    for _ in 0..20 {
        if ended() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ended(), "no helper survives readiness failure");
    drop(child); // cleared pgid means no second signal after reaping
}
