//! Manager behaviour against the in-process fake server (`fake.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::fake::{self, Behavior, Handle};
use super::*;
use crate::config::{LspConfig, LspServerConfig};

struct Setup {
    mgr: Arc<LspManager>,
    handles: Arc<Mutex<Vec<Handle>>>,
    spawns: Arc<Mutex<Vec<PathBuf>>>,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

fn setup(b: Behavior, tweak: impl FnOnce(&mut LspConfig)) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = crate::tools::canonical(&dir.path().join("proj"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("foo.toml"), "").unwrap();
    let mut cfg = LspConfig {
        servers: vec![LspServerConfig {
            name: "fake".into(),
            // Any executable: the spawner below never runs it.
            command: vec![std::env::current_exe().unwrap().display().to_string()],
            extensions: vec!["foo".into()],
            markers: vec!["foo.toml".into()],
            language_id: None,
        }],
        ..Default::default()
    };
    tweak(&mut cfg);
    let handles: Arc<Mutex<Vec<Handle>>> = Arc::default();
    let spawns: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let (h2, s2) = (handles.clone(), spawns.clone());
    let mgr = LspManager::with_spawner(
        cfg,
        Box::new(move |_def, root, _sandbox| {
            let (t, h) = fake::pair(b.clone());
            h2.lock().push(h);
            s2.lock().push(root.to_path_buf());
            Ok(t)
        }),
    );
    Setup {
        mgr,
        handles,
        spawns,
        root,
        _dir: dir,
    }
}

fn env(root: &Path, may_start: bool) -> SessionEnv {
    SessionEnv {
        session: "s1".into(),
        cwd: root.to_path_buf(),
        turn: 1,
        sandbox: None,
        may_start,
    }
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

#[tokio::test]
async fn detects_routes_and_shares_one_server_per_root() {
    let s = setup(Behavior::default(), |_| {});
    let names: Vec<&str> = s
        .mgr
        .detect(&s.root)
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert!(names.contains(&"fake"), "{names:?}");
    let file = s.root.join("src/a.foo");
    let (def, root) = s.mgr.route(&file, &s.root).unwrap();
    assert_eq!(
        (def.name.as_str(), root.as_path()),
        ("fake", s.root.as_path())
    );
    assert!(s.mgr.route(&s.root.join("a.txt"), &s.root).is_none());
    // A file outside the session's directory (a dependency with its own
    // marker) goes to the session's server, not a new one rooted there.
    let dep = s._dir.path().join("dep");
    std::fs::create_dir_all(&dep).unwrap();
    std::fs::write(dep.join("foo.toml"), "").unwrap();
    let (_, dep_root) = s.mgr.route(&dep.join("lib.foo"), &s.root).unwrap();
    assert_eq!(dep_root, s.root);
    let elsewhere = s._dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(s.mgr.route(&dep.join("lib.foo"), &elsewhere).is_none());

    let e = env(&s.root, true);
    // Nothing starts until something needs it.
    assert!(s.spawns.lock().is_empty());
    let (a, b) = tokio::join!(
        s.mgr.client(&def, &root, &e, true),
        s.mgr.client(&def, &root, &e, true)
    );
    assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()));
    assert_eq!(
        s.spawns.lock().len(),
        1,
        "concurrent callers share one start"
    );
    // Another project gets its own server.
    let other = s.root.join("sub");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("foo.toml"), "").unwrap();
    let (_, other_root) = s.mgr.route(&other.join("x.foo"), &s.root).unwrap();
    assert_eq!(other_root, other);
    s.mgr.client(&def, &other_root, &e, true).await.unwrap();
    assert_eq!(s.spawns.lock().len(), 2);
    // Without permission to start, only running servers are used.
    let s2 = setup(Behavior::default(), |_| {});
    let e2 = env(&s2.root, false);
    let (def2, root2) = s2.mgr.route(&s2.root.join("a.foo"), &s2.root).unwrap();
    assert!(s2.mgr.client(&def2, &root2, &e2, false).await.is_err());
    s2.mgr.touch(&e2, &s2.root.join("a.foo"), "x");
    assert!(
        s2.mgr
            .after_write(&e2, &s2.root.join("a.foo"), None, "BAD", false)
            .await
            .is_none()
    );
    assert!(s2.spawns.lock().is_empty());
}

#[tokio::test]
async fn edits_report_only_new_problems_and_what_they_fixed() {
    let s = setup(Behavior::default(), |_| {});
    let e = env(&s.root, true);
    let file = s.root.join("a.foo");
    let old = "ok\nBAD old\nWARN keep\n";
    std::fs::write(&file, old).unwrap();
    // A read warms the server and gives it a baseline.
    s.mgr.touch(&e, &file, old);
    assert!(
        eventually(|| s
            .mgr
            .running("fake", &s.root)
            .and_then(|c| c.published(&file))
            .is_some())
        .await
    );
    let new = "inserted\nok\nBAD old\nWARN keep\nBAD new\n";
    let out = s
        .mgr
        .after_write(&e, &file, Some(old), new, true)
        .await
        .expect("a report");
    assert_eq!(
        out,
        "LSP fake: 1 new error\na.foo:5:1 error bad thing on this line"
    );
    // Fixing it: nothing new, one fixed.
    let fixed = "inserted\nok\nBAD old\nWARN keep\n";
    let out = s.mgr.after_write(&e, &file, Some(new), fixed, true).await;
    assert_eq!(out.as_deref(), Some("LSP fake: 1 problem fixed, none new"));
    // A clean edit says nothing.
    let clean = "inserted\nok\nBAD old\nWARN keep\nfine\n";
    assert_eq!(
        s.mgr.after_write(&e, &file, Some(fixed), clean, true).await,
        None
    );
}

#[tokio::test]
async fn slow_diagnostics_arrive_with_a_later_result_once_per_turn() {
    let s = setup(
        Behavior {
            publish_delay: Duration::from_millis(700),
            ..Default::default()
        },
        |c| c.wait_ms = 200,
    );
    let e = env(&s.root, true);
    let a = s.root.join("a.foo");
    let started = Instant::now();
    let out = s.mgr.after_write(&e, &a, None, "ok\nBAD\n", false).await;
    assert!(
        started.elapsed() < Duration::from_millis(600),
        "the edit didn't wait"
    );
    assert_eq!(out.as_deref(), Some("LSP fake: (diagnostics pending)"));
    // Said at most once per session and server.
    let b = s.root.join("b.foo");
    assert_eq!(s.mgr.after_write(&e, &b, None, "fine\n", false).await, None);
    assert!(
        eventually(|| s
            .mgr
            .late
            .lock()
            .get("s1")
            .is_some_and(|l| !l.blocks.is_empty()))
        .await
    );
    let late = s.mgr.take_late("s1", 1, &s.root).unwrap();
    assert_eq!(
        late,
        "LSP fake (late): 1 new error\na.foo:2:1 error bad thing on this line"
    );
    assert_eq!(s.mgr.take_late("s1", 1, &s.root), None, "delivered once");

    // At most LATE_BLOCKS_PER_TURN reports per turn; the rest wait.
    let client = s.mgr.running("fake", &s.root).unwrap();
    let d = client.published(&a).unwrap().items[0].clone();
    let block = || LateBlock {
        server: "fake".into(),
        client: Arc::downgrade(&client),
        items: vec![(a.clone(), d.clone())],
    };
    for _ in 1..LATE_BLOCKS_PER_TURN {
        s.mgr.queue_late("s1", block());
        assert!(s.mgr.take_late("s1", 1, &s.root).is_some());
    }
    s.mgr.queue_late("s1", block());
    assert_eq!(
        s.mgr.take_late("s1", 1, &s.root),
        None,
        "cap reached this turn"
    );
    assert!(s.mgr.take_late("s1", 2, &s.root).is_some(), "next turn");
    // Something fixed in the meantime is dropped instead of reported.
    s.mgr.queue_late("s1", block());
    client.sync(&a, "ok\n", "foo").unwrap();
    assert!(eventually(|| client.published(&a).is_some_and(|p| p.items.is_empty())).await);
    assert_eq!(s.mgr.take_late("s1", 3, &s.root), None);
}

#[tokio::test]
async fn crashed_servers_restart_a_limited_number_of_times() {
    let s = setup(Behavior::default(), |_| {});
    let e = env(&s.root, true);
    let (def, root) = s.mgr.route(&s.root.join("a.foo"), &s.root).unwrap();
    for round in 0..MAX_CRASHES {
        let c = s.mgr.client(&def, &root, &e, true).await.unwrap();
        s.handles.lock().last().unwrap().crash();
        assert!(eventually(|| !c.alive()).await, "round {round}");
    }
    let err = s.mgr.client(&def, &root, &e, true).await.err().unwrap();
    assert!(err.contains("crashed 3 times"), "{err}");
    assert_eq!(s.spawns.lock().len(), MAX_CRASHES as usize);
}

#[tokio::test]
async fn idle_servers_stop_and_the_limit_on_running_servers_holds() {
    let s = setup(Behavior::default(), |c| c.max_servers = 1);
    s.mgr.set_idle(Duration::from_millis(300));
    let e = env(&s.root, true);
    let (def, root) = s.mgr.route(&s.root.join("a.foo"), &s.root).unwrap();
    let first = s.mgr.client(&def, &root, &e, true).await.unwrap();
    // A second project pushes the least recently used server out.
    let other = s.root.join("sub");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("foo.toml"), "").unwrap();
    s.mgr.client(&def, &other, &e, true).await.unwrap();
    let first_handle = s.handles.lock()[0].clone();
    assert!(first_handle.saw("shutdown").await);
    assert!(eventually(|| !first.alive()).await);
    assert!(s.mgr.running("fake", &root).is_none());
    // The remaining one stops once idle, and that isn't counted as a crash.
    let second_handle = s.handles.lock()[1].clone();
    assert!(second_handle.saw("exit").await);
    assert!(eventually(|| s.mgr.running("fake", &other).is_none()).await);
    let again = s.mgr.client(&def, &other, &e, true).await;
    assert!(again.is_ok(), "restartable after an idle stop");
}

#[test]
fn secret_looking_variables_are_not_passed_to_servers() {
    for name in [
        "GITHUB_TOKEN",
        "OPENAI_API_KEY",
        "db_password",
        "AWS_SECRET_ACCESS_KEY",
        "NPM_AUTH",
    ] {
        assert!(secret_like(name), "{name}");
    }
    for name in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "SSH_AUTH_SOCK",
        "RUSTUP_TOOLCHAIN",
    ] {
        assert!(!secret_like(name), "{name}");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn lsp_kill_stops_spawn_owned_setsid_descendant() {
    if !crate::tools::bash::tests::cleanup_fixture(
        "lsp::tests::lsp_kill_stops_spawn_owned_setsid_descendant",
    ) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args([
            "-c",
            "setsid /bin/sleep 30 & echo $! > \"$1\"; wait",
            "fixture",
        ])
        .arg(&ready)
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    let child = crate::platform::spawn(&mut command, crate::platform::Start::Agent).unwrap();
    let leader = child.id().unwrap() as i32;
    let tree = child.id().map(crate::tools::bash::ChildTree::capture);
    let (mut transport, _fake) = fake::pair(Behavior::default());
    transport.process = Some(child);
    transport.tree = tree;
    let client = LspClient::start(
        "setsid-fake",
        dir.path(),
        transport,
        serde_json::Value::Null,
        serde_json::Value::Null,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    let descendant: i32 = loop {
        if let Ok(text) = std::fs::read_to_string(&ready)
            && let Ok(pid) = text.trim().parse()
        {
            break pid;
        }
        assert!(
            Instant::now() < until,
            "fake did not publish its setsid child"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        unsafe { (libc::getpgid(descendant), libc::getsid(descendant)) },
        (descendant, descendant)
    );
    client.kill();
    // Give the off-worker tree stop and owning Child wait a chance to finish
    // before the test-only adopter reaps detached descendants.
    let until = Instant::now() + Duration::from_secs(8);
    while Path::new(&format!("/proc/{leader}")).exists() {
        assert!(Instant::now() < until, "LSP kill did not reap its leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    crate::tools::bash::tests::reaped_disappearance(&[leader, descendant]);
}
