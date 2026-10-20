//! The sandbox on kernels whose Landlock is too old to use (ABI 1, e.g.
//! Ubuntu 22.04's 5.15), through the real Bash tool: commands share a chat's
//! namespace and enter their working folder inside it, so paths relative to
//! that folder see the sandbox's view too (hidden folders, read-only rules,
//! its own `/proc`). Its own binary, so the pretend kernel applies to
//! everything in it.

mod common;

use common::*;
use serde_json::json;
use tessera_proto::*;

#[tokio::test]
async fn relative_paths_stay_inside_the_shared_sandbox() {
    // Before anything probes the kernel (and alone in this binary).
    unsafe { std::env::set_var("TESSERA_TEST_LANDLOCK_ABI", "1") };
    let mode = tessera_core::sandbox::test_mode("relative paths in the shared sandbox");
    if mode != Some("shared") {
        eprintln!("SKIP: this needs the shared namespace, not {mode:?}");
        return;
    }
    let h = harness(vec![], 100_000).await;
    let work = h.work.canonicalize().unwrap();
    let project = work.parent().unwrap().to_path_buf();
    std::fs::write(project.join("notes.txt"), "unchanged").unwrap();
    std::fs::write(
        h.dir.path().join("auth.json"),
        "{\"secret\": \"sk-not-for-commands\"}",
    )
    .unwrap();
    // The project read-only, its work folder writable: commands in the work
    // folder run (they used to be refused in this mode), and change nothing
    // above it.
    h.engine
        .handle(
            Request::SetSecurity {
                sandbox: None,
                folders: Some(vec![
                    FolderAccess {
                        path: project.display().to_string(),
                        write: false,
                    },
                    FolderAccess {
                        path: work.display().to_string(),
                        write: true,
                    },
                ]),
                ask_outside: None,
                redact_patterns: None,
                save_secrets_from_chat: None,
                secret_saving: None,
                secret_machines: None,
                secret_machines_picked: None,
                bot_handoff: None,
                default_permission: None,
            },
            None,
        )
        .await
        .unwrap();

    // Tessera's home and `/proc`, reached from the working folder by
    // relative paths (the home's name put together in the shell, so only
    // the sandbox stands in the way).
    let up = "../".repeat(work.components().count() - 1);
    let home = h.dir.path().canonicalize().unwrap();
    let home = home.strip_prefix("/").unwrap().display().to_string();
    let (a, b) = home.split_at(home.len() / 2);
    h.mock.push(Reply::Tools(vec![
        (
            "bash".into(),
            json!({"command": format!(
                "d='{up}{a}''{b}'; cat \"$d/auth.json\" 2>&1; \
                 [ {up}proc -ef /proc ] && echo proc-same-view; ls {up}proc | grep -c '^[0-9]'"
            )}),
        ),
        (
            "bash".into(),
            json!({"command": "echo changed 2>/dev/null > ../notes.txt && echo WROTE || echo refused-ro; \
                               echo ok > made && echo wrote-work; pwd"}),
        ),
    ]));
    h.mock.push(Reply::Text("ok".into()));
    let s = h.session().await;
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: s.id.clone(),
                title: None,
                model: None,
                thinking: None,
                permission: Some(PermissionMode::Auto),
                pinned: None,
                archived: None,
                cwd: None,
                members: None,
                fast: None,
                subagents: None,
            },
            None,
        )
        .await
        .unwrap();
    h.send(&s.id, "go").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let out = &results[0].2;
    assert!(!out.contains("sk-not-for-commands"), "{out}");
    assert!(out.contains("proc-same-view"), "{out}");
    let visible: usize = out
        .lines()
        .rev()
        .find_map(|l| l.trim().parse().ok())
        .unwrap();
    assert!(visible < 20, "only the namespace's processes: {out}");
    let out = &results[1].2;
    assert!(!results[1].1, "{out}");
    assert!(
        out.contains("refused-ro") && out.contains("wrote-work") && !out.contains("WROTE"),
        "{out}"
    );
    assert!(out.contains(&work.display().to_string()), "{out}");
    assert_eq!(
        std::fs::read_to_string(project.join("notes.txt")).unwrap(),
        "unchanged"
    );
    assert!(work.join("made").is_file());
}
