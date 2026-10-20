//! These regressions run in separate processes: a blocked runtime cannot
//! enforce its own timeout, and deadlock detection is process-wide.
use super::*;
use std::process::{Command, Stdio};
use std::sync::Barrier;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Hooks {
    list_state: Mutex<Option<(String, Arc<Gate>)>>,
    session_insert: Mutex<Option<Arc<Gate>>>,
}

#[derive(Default)]
struct Gate {
    reached: BarrierPair,
}

// Both barriers have two participants, never a process-global hook.
struct BarrierPair(Barrier, Barrier);
impl Default for BarrierPair {
    fn default() -> Self {
        Self(Barrier::new(2), Barrier::new(2))
    }
}
impl Gate {
    fn pause(&self) {
        self.reached.0.wait();
        self.reached.1.wait();
    }
    fn wait(&self) {
        self.reached.0.wait();
    }
    fn release(&self) {
        self.reached.1.wait();
    }
}
impl Hooks {
    pub(super) fn before_list_state(&self, id: &str) {
        let hook = self.list_state.lock().clone();
        if let Some((_, gate)) = hook.filter(|(target, _)| target == id) {
            gate.pause();
        }
    }
    pub(super) fn before_session_insert(&self) {
        let gate = self.session_insert.lock().clone();
        if let Some(gate) = gate {
            gate.pause();
        }
    }
}

fn subprocess(case: &str) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "engine::lock_order_tests::lock_order_subprocess",
            "--ignored",
            "--nocapture",
        ])
        .env("TESSERA_LOCK_ORDER_CASE", case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("{case}: external deadline killed owned subprocess {pid}: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{case}: subprocess {pid} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{case}: subprocess {pid} passed");
}

#[test]
fn list_and_effective_permission_do_not_deadlock() {
    subprocess("permission");
}

#[test]
fn list_and_helper_creation_do_not_deadlock() {
    subprocess("creation");
}

#[test]
fn permission_publication_and_queued_commits_do_not_invert_state_locks() {
    subprocess("admission");
}
fn fixture() -> (tempfile::TempDir, tokio::runtime::Runtime, Engine, Arc<Rt>) {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::at(home.path().join("data"));
    paths.ensure().unwrap();
    // A configured, non-discovering fixture prevents startup probing. No
    // completion or provider request is made, and no real config is read.
    std::fs::write(
        paths.config_file(),
        "default_model = \"fixture/model\"\n[lsp]\nenabled = false\n[[provider]]\nid = \"fixture\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:1/v1\"\ndiscover = false\n[[provider.models]]\nid = \"model\"\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let engine = runtime
        .block_on(Engine::open(paths, EngineOptions::default()))
        .unwrap();
    let project = home.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let parent = engine
        .0
        .create_session(
            Some(project.display().to_string()),
            None,
            Some("fixture parent".into()),
            SessionKind::Chat,
            None,
            Some(PermissionMode::Auto),
            None,
        )
        .unwrap();
    (home, runtime, engine, parent)
}

#[test]
#[ignore = "only invoked by the externally bounded subprocess regressions"]
fn lock_order_subprocess() {
    let case = std::env::var("TESSERA_LOCK_ORDER_CASE").unwrap();
    let (_home, _runtime, engine, parent) = fixture();
    let inner = engine.0.clone();
    if case == "admission" {
        let child = inner.start_lock_order_test_child(&parent);
        let admission = inner.action_admission(&child.st.lock());
        let gate = Arc::new(Gate::default());
        let g = gate.clone();
        let p = parent.clone();
        let writer = std::thread::spawn(move || {
            admission
                .run(|| {
                    g.pause();
                    Ok(())
                })
                .unwrap();
            // Any state access is outside the effect gate, including
            // cache/history updates after a committed file operation.
            let _state = p.st.lock();
        });
        gate.wait();
        let (locked, reached) = std::sync::mpsc::channel();
        let publisher = std::thread::spawn(move || {
            let mut st = parent.st.lock();
            st.info.permission = PermissionMode::ReadOnly;
            locked.send(()).unwrap();
            inner.publish_permission(&st);
        });
        reached.recv_timeout(Duration::from_secs(2)).unwrap();
        gate.release();
        writer.join().unwrap();
        publisher.join().unwrap();
        assert!(parking_lot::deadlock::check_deadlock().is_empty());
        return;
    }
    let writer_gate = Arc::new(Gate::default());
    let reader_gate = Arc::new(Gate::default());
    let (done, completion) = std::sync::mpsc::channel();
    let (writer, target) = if case == "permission" {
        let child = inner.start_lock_order_test_child(&parent);
        let target = child.st.lock().info.id.clone();
        let gate = writer_gate.clone();
        let done = done.clone();
        let me = inner.clone();
        (
            std::thread::spawn(move || {
                let st = child.st.lock();
                gate.pause();
                assert_eq!(me.effective_permission(&st), PermissionMode::Auto);
                done.send("permission completed").unwrap();
            }),
            target,
        )
    } else {
        assert_eq!(case, "creation");
        *inner.lock_order_hooks.session_insert.lock() = Some(writer_gate.clone());
        let target = parent.st.lock().info.id.clone();
        let done = done.clone();
        let me = inner.clone();
        (
            std::thread::spawn(move || {
                let child = me.start_lock_order_test_child(&parent);
                assert_eq!(child.st.lock().info.kind, SessionKind::Subagent);
                done.send("helper creation completed").unwrap();
            }),
            target,
        )
    };
    // The writer already owns the state when listing reaches that same
    // session. On the old code listing still owns the map at this gate.
    writer_gate.wait();
    *inner.lock_order_hooks.list_state.lock() = Some((target, reader_gate.clone()));
    let me = inner.clone();
    let reader = std::thread::spawn(move || {
        assert_eq!(me.list_sessions(false).unwrap().len(), 1);
        done.send("list completed").unwrap();
    });
    reader_gate.wait();
    reader_gate.release();
    writer_gate.release();
    for _ in 0..2 {
        match completion.recv_timeout(Duration::from_secs(2)) {
            Ok(message) => eprintln!("{case}: {message}"),
            Err(error) => {
                let cycles = parking_lot::deadlock::check_deadlock();
                let sizes: Vec<_> = cycles.iter().map(Vec::len).collect();
                panic!("{case}: {error}; independently detected lock cycles: {sizes:?}");
            }
        }
    }
    writer.join().unwrap();
    reader.join().unwrap();
    assert!(parking_lot::deadlock::check_deadlock().is_empty());
}

#[test]
fn snapshots_do_not_block_current_thread_timers() {
    let (_home, runtime, engine, parent) = fixture();
    let id = parent.st.lock().info.id.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _state = parent.st.lock();
        held_tx.send(()).unwrap();
        // The old implementation stalls its own timeout: release anyway
        // so a failed assertion never strands the test process.
        let _ = release_rx.recv_timeout(Duration::from_secs(2));
    });
    held_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let began = Instant::now();
    let (delay, request) = runtime.block_on(async move {
        let request = tokio::spawn(async move {
            engine
                .handle(
                    Request::GetSession {
                        session_id: id,
                        since_seq: None,
                    },
                    None,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        (began.elapsed(), request)
    });
    let _ = release_tx.send(());
    holder.join().unwrap();
    runtime.block_on(request).unwrap().unwrap();
    assert!(
        delay < Duration::from_millis(500),
        "snapshot blocked the timer for {delay:?}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sqlite_write_waits_do_not_own_the_live_session_map() {
    let (_home, _runtime, engine, parent) = fixture();
    let other = rusqlite::Connection::open(engine.0.paths.db_file()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    parent.st.lock().internal.system_prompt = Some("synthetic changed prompt".into());
    let me = engine.0.clone();
    let (writer_tx, writer_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let tid = unsafe { libc::syscall(libc::SYS_gettid) as u64 };
        writer_tx.send(tid).unwrap();
        me.save(&parent);
    });
    let tid = writer_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let label = format!("  {tid}: active=store:update_session");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !crate::activity::report().contains(&label) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let reached = crate::activity::report().contains(&label);
    let map_available = engine.0.sessions.try_lock().is_some();
    other.execute_batch("ROLLBACK").unwrap();
    writer.join().unwrap();
    assert!(reached, "save never reached the held SQLite writer");
    assert!(
        map_available,
        "a SQLite write wait held the live-session map"
    );
}

#[test]
fn parent_ceiling_stays_fail_closed() {
    let (_home, _runtime, engine, parent) = fixture();
    let child = engine.0.start_lock_order_test_child(&parent);
    {
        let mut st = parent.st.lock();
        st.info.permission = PermissionMode::ReadOnly;
        engine.0.publish_permission(&st);
    }
    assert_eq!(
        engine.0.effective_permission(&child.st.lock()),
        PermissionMode::ReadOnly
    );
    {
        let mut st = parent.st.lock();
        st.info.permission = PermissionMode::Auto;
        engine.0.publish_permission(&st);
    }
    // Contention is not a downgrade. Neither the state nor session-map
    // lock is needed to read the live parent ceiling.
    let _busy = parent.st.lock();
    let _map = engine.0.sessions.lock();
    assert_eq!(
        engine.0.effective_permission(&child.st.lock()),
        PermissionMode::Auto
    );
    drop(_busy);
    drop(_map);
    child.st.lock().info.parent_id = Some("missing-parent".into());
    assert_eq!(
        engine.0.effective_permission(&child.st.lock()),
        PermissionMode::ReadOnly
    );
}

#[test]
fn parent_ceiling_survives_reload_and_is_revoked_on_delete() {
    let (_home, _runtime, engine, parent) = fixture();
    let child = engine.0.start_lock_order_test_child(&parent);
    let parent_id = parent.st.lock().info.id.clone();
    let child_id = child.st.lock().info.id.clone();
    engine.0.save(&child);
    drop(parent);
    assert_eq!(
        engine.0.evict_idle(0, u64::MAX),
        0,
        "child pins its parent's ceiling"
    );
    // Simulate a fresh runtime cache (a restart) without retaining old ceilings.
    drop(child);
    engine.0.sessions.lock().clear();
    let child = engine.0.rt(&child_id).unwrap();
    let parent = engine.0.rt(&parent_id).unwrap();
    {
        let mut st = parent.st.lock();
        st.info.permission = PermissionMode::Ask;
        engine.0.publish_permission(&st);
        assert_eq!(
            engine.0.effective_permission(&child.st.lock()),
            PermissionMode::Ask
        );
    }
    engine.0.remove_session(parent_id).unwrap();
    {
        let mut st = parent.st.lock();
        st.info.permission = PermissionMode::Auto;
        engine.0.publish_permission(&st);
    }
    assert_eq!(
        engine.0.effective_permission(&child.st.lock()),
        PermissionMode::ReadOnly
    );
}

#[test]
fn native_and_claude_tools_ignore_parent_lock_contention_but_obey_downgrades() {
    let (_home, _runtime, engine, parent) = fixture();
    let child = engine.0.start_lock_order_test_child(&parent);
    let mut pst = parent.st.lock();
    for mode in [
        PermissionMode::Auto,
        PermissionMode::Ask,
        PermissionMode::ReadOnly,
    ] {
        pst.info.permission = mode;
        engine.0.publish_permission(&pst);
        let st = child.st.lock();
        let native = engine.0.callable_tools(&st);
        let claude = engine.0.claude_capabilities(&st);
        let writable = mode != PermissionMode::ReadOnly;
        assert_eq!(native.iter().any(|t| t.name == "bash"), writable);
        assert_eq!(claude.iter().any(|t| t == "bash"), writable);
        assert!(
            !native
                .iter()
                .any(|t| matches!(t.name.as_str(), "agent" | "agents"))
        );
        assert!(
            !crate::claude_code::limits_for(Some(&claude))
                .only
                .contains(&"Agent")
        );
    }
}

#[test]
fn save_after_delete_cannot_recreate_a_session() {
    let (_home, _runtime, engine, parent) = fixture();
    let id = parent.st.lock().info.id.clone();
    parent.st.lock().info.title = "latest live title".into();
    engine.0.save(&parent);
    assert_eq!(
        engine.0.store.get_session(&id).unwrap().unwrap().0.title,
        "latest live title"
    );
    engine.0.remove_session(id.clone()).unwrap();
    engine.0.save(&parent);
    assert!(engine.0.store.get_session(&id).unwrap().is_none());
    assert!(engine.0.list_sessions(false).unwrap().is_empty());
}

#[test]
fn snapshots_keep_live_info_and_planned_restart_ids() {
    let (_home, _runtime, engine, parent) = fixture();
    let child = engine.0.start_lock_order_test_child(&parent);
    let parent_id = {
        let mut st = parent.st.lock();
        st.info.title = "live parent title".into();
        st.running = true;
        st.info.id.clone()
    };
    let child_id = {
        let mut st = child.st.lock();
        st.info.title = "live helper title".into();
        st.info.id.clone()
    };
    let listed = engine.0.list_sessions(false).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "live parent title");
    assert_eq!(
        engine.0.subagents_of(&parent_id)[0].title,
        "live helper title"
    );
    engine.0.note_planned_restart();
    let resumed: Vec<String> =
        serde_json::from_slice(&std::fs::read(engine.0.planned_restart_file()).unwrap()).unwrap();
    assert!(resumed.contains(&parent_id) && resumed.contains(&child_id));
}

#[test]
fn list_snapshots_preserve_private_device_filtering() {
    let (_home, runtime, engine, _parent) = fixture();
    let private = engine
        .0
        .create_session_ext(
            None,
            None,
            Some("private fixture".into()),
            SessionKind::Chat,
            None,
            Some(PermissionMode::ReadOnly),
            None,
            true,
            false,
            false,
            Some("fixture-owner"),
        )
        .unwrap();
    let id = private.st.lock().info.id.clone();
    let list = |device| -> Vec<SessionInfo> {
        serde_json::from_value(
            runtime
                .block_on(engine.handle(Request::ListSessions { archived: false }, Some(device)))
                .unwrap(),
        )
        .unwrap()
    };
    assert!(list("fixture-owner").iter().any(|s| s.id == id));
    assert!(!list("fixture-other").iter().any(|s| s.id == id));
    let database = rusqlite::Connection::open(engine.0.paths.db_file()).unwrap();
    let saved: u32 = database
        .query_row(
            "SELECT count(*) FROM sessions WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, 0, "the private session stays off disk");
}

#[test]
fn helper_executes_a_native_action_while_parent_state_is_busy() {
    struct Action;
    impl crate::tools::Tool for Action {
        fn name(&self) -> &'static str {
            "fixture_action"
        }
        fn description(&self) -> String {
            "Fixture action".into()
        }
        fn schema(&self) -> Value {
            json!({"type":"object"})
        }
        fn kind(&self) -> crate::tools::ToolKind {
            crate::tools::ToolKind::Exec
        }
        fn run<'a>(&'a self, _: Value, _: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async { ToolOutput::text("Action ran") })
        }
    }
    let (_home, runtime, engine, parent) = fixture();
    let child = engine.0.start_lock_order_test_child(&parent);
    let mut pst = parent.st.lock();
    let act = || {
        runtime.block_on(engine.0.execute_tool(
            &child,
            Arc::new(Action),
            "fixture-action",
            json!({}),
            0,
            &CancellationToken::new(),
        ))
    };
    let out = act();
    assert!(!out.is_error, "{out:?}");
    assert_eq!(plain_text(&out.content), "Action ran");
    pst.info.permission = PermissionMode::ReadOnly;
    engine.0.publish_permission(&pst);
    let out = act();
    assert!(
        out.is_error && plain_text(&out.content).contains("read-only"),
        "{out:?}"
    );
}

#[test]
fn either_helper_entry_point_is_on_only_when_actually_offered() {
    let (_home, _runtime, engine, parent) = fixture();
    let mut st = parent.st.lock();
    for only in [vec!["agent"], vec!["agents"], vec!["read"]] {
        st.tools_only = Some(only);
        for enabled in [true, false] {
            st.info.subagents = enabled;
            let specs = engine.0.base_tool_specs(&st);
            assert_eq!(
                engine.0.helpers_offered(&st),
                specs
                    .iter()
                    .any(|t| matches!(t.name.as_str(), "agent" | "agents"))
            );
        }
    }
    st.tools_only = None;
    st.info.private = true;
    st.info.subagents = true;
    assert!(!engine.0.helpers_offered(&st));
    assert!(engine.0.system_prompt(&mut st).contains("Helpers are off"));
}

#[test]
fn a_helper_write_queued_on_the_blocking_pool_cannot_commit_after_parent_downgrade() {
    let (_home, _runtime, engine, parent) = fixture();
    let child = engine.0.start_lock_order_test_child(&parent);
    let path = PathBuf::from(&child.st.lock().info.cwd).join("fixture.txt");
    std::fs::write(&path, "before").unwrap();
    let destination = crate::tools::file_object::Destination::prepare_guarded(
        &path,
        |_| Ok(()),
        engine.0.action_admission(&child.st.lock()),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (entered, reached) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    runtime.block_on(async {
        let busy = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        reached.recv_timeout(Duration::from_secs(2)).unwrap();
        let write = tokio::task::spawn_blocking(move || destination.write(Some("before"), "after"));
        {
            let mut st = parent.st.lock();
            st.info.permission = PermissionMode::ReadOnly;
            engine.0.publish_permission(&st);
        }
        release.send(()).unwrap();
        busy.await.unwrap();
        let err = write.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1,
        "no staged write remains"
    );
}

#[test]
fn worktree_waiting_for_a_slot_is_cancelled_on_parent_downgrade() {
    let (_home, runtime, engine, parent) = fixture();
    engine.0.config.write().security.sandbox = SandboxMode::Off;
    let cwd = PathBuf::from(&parent.st.lock().info.cwd);
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "--allow-empty",
        "-qm",
        "fixture",
    ]);
    let child = engine.0.start_lock_order_test_child(&parent);
    let id = child.st.lock().info.id.clone();
    child.st.lock().internal.subagent.as_mut().unwrap().worktree =
        Some(crate::worktree::WorktreeState {
            repo: cwd.display().to_string(),
            ..Default::default()
        });
    let cancel = CancellationToken::new();
    let parent = parent.clone();
    runtime.block_on(async {
        // No room on the server: the helper waits for its turn.
        engine.0.helper_gates.server.set_limit(Some(0));
        let inner = engine.0.clone();
        let task = tokio::spawn(inner.finish_child(child.clone(), cancel));
        // Poll finish_child into its slot wait, without freeing the slot.
        tokio::task::yield_now().await;
        assert!(child.st.lock().cancel.is_some());
        {
            let mut st = parent.st.lock();
            st.info.permission = PermissionMode::ReadOnly;
            engine.0.publish_permission(&st);
        }
        let _outcome = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.st.lock().info.status, SessionStatus::Interrupted);
        assert!(!child.st.lock().running);
        assert!(!child.st.lock().info.helper_queued);
        engine.0.refresh_helper_gates();
    });
    assert!(!cwd.join(".git/tessera/worktrees").join(id).exists());
    assert!(crate::worktree::registered(&engine.0.store).is_empty());
}

#[test]
fn stale_helper_context_cannot_write_delete_or_start_a_shell_after_downgrade() {
    use crate::tools::Tool;
    let (_home, runtime, engine, parent) = fixture();
    engine.0.config.write().security.sandbox = SandboxMode::Off;
    let child = engine.0.start_lock_order_test_child(&parent);
    let ctx = {
        let st = child.st.lock();
        ToolCtx {
            session_id: st.info.id.clone(),
            call_id: "fixture".into(),
            seq: 0,
            cwd: PathBuf::from(&st.info.cwd),
            cancel: CancellationToken::new(),
            host: Arc::new(Host(engine.0.clone(), false)),
            progress: Arc::new(|_| {}),
            depth: st.depth,
            cleared: None,
            admission: engine.0.action_admission(&st),
        }
    };
    std::fs::write(ctx.cwd.join("keep.txt"), "before\n").unwrap();
    {
        let mut st = parent.st.lock();
        st.info.permission = PermissionMode::ReadOnly;
        engine.0.publish_permission(&st);
        // Restoring Auto must not revive the old context either. Current
        // preflight now permits these targets; only commit admission blocks.
        st.info.permission = PermissionMode::Auto;
        engine.0.publish_permission(&st);
    }
    runtime.block_on(async {
        let out = crate::tools::files::Write
            .run(json!({"path":"new/fixture.txt","content":"after"}), &ctx)
            .await;
        assert!(
            out.is_error && plain_text(&out.content).contains("permissions changed"),
            "{out:?}"
        );
        let out = crate::tools::patch::run(
            "*** Begin Patch\n*** Delete File: keep.txt\n*** End Patch",
            &ctx,
        )
        .await;
        assert!(
            out.is_error && plain_text(&out.content).contains("permissions changed"),
            "{out:?}"
        );
        let out = crate::tools::bash::Bash
            .run(json!({"command":"echo after > shell-marker"}), &ctx)
            .await;
        assert!(
            out.is_error && plain_text(&out.content).contains("permissions changed"),
            "{out:?}"
        );
    });
    assert_eq!(
        std::fs::read_to_string(ctx.cwd.join("keep.txt")).unwrap(),
        "before\n"
    );
    assert!(!ctx.cwd.join("new").exists());
    assert!(!ctx.cwd.join("shell-marker").exists());
}
