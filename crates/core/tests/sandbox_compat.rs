//! The sandbox on kernels whose Landlock is too old to use (ABI 1, e.g.
//! Ubuntu 22.04's 5.15): commands share a private process namespace instead.
//! Its own binary, so the pretend kernel applies to everything in it.

mod common;

use common::*;
use serde_json::json;
use tessera_proto::*;

#[tokio::test]
async fn commands_are_isolated_without_landlock() {
    // Before anything probes the kernel (and alone in this binary).
    unsafe { std::env::set_var("TESSERA_TEST_LANDLOCK_ABI", "1") };
    let support = tessera_core::sandbox::support();
    assert!(support.landlock <= 1);
    if tessera_core::sandbox::test_mode("commands_are_isolated_without_landlock") != Some("shared")
    {
        eprintln!(
            "SKIP: no private process view here: {} ({:?})",
            tessera_core::sandbox::describe(),
            tessera_core::sandbox::hint()
        );
        return;
    }
    let h = harness(vec![], 100_000).await;
    let info: SecurityInfo =
        serde_json::from_value(h.engine.handle(Request::GetSecurity, None).await.unwrap()).unwrap();
    assert_eq!(
        info.sandbox_status,
        "on (namespaces + a private process view)"
    );
    assert!(
        info.sandbox_complete && info.sandbox_hint.is_none(),
        "{info:?}"
    );

    let home = h.dir.path().display().to_string();
    let (a, b) = home.split_at(home.len() / 2);
    std::fs::write(
        h.dir.path().join("auth.json"),
        "{\"secret\": \"sk-not-for-commands\"}",
    )
    .unwrap();
    let me = std::process::id();
    h.mock.push(Reply::Tools(vec![
        // What Landlock ABI 1 would break: moving files between directories (git does it for every object).
        ("bash".into(), json!({"command": "mkdir -p a b && echo hi > a/f && mv a/f b/f && cat b/f && git init -q r && cd r && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m m && echo committed"})),
        // Tessera's files are out of reach, directly or through this process (the server).
        // (The path is put together in the shell, so only the sandbox stands in the way.)
        ("bash".into(), json!({"command": format!("d='{a}''{b}'; cat \"$d/auth.json\" 2>&1; cat /proc/{me}/root\"$d\"/auth.json 2>&1; ls /proc | grep -c '^[0-9]'")})),
        // A server one command starts, the next can stop.
        ("bash".into(), json!({"command": "sleep 300 >/dev/null 2>&1 & echo $! > pid; echo started"})),
    ]));
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "kill $(cat pid) && echo stopped"}),
    )]));
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
    assert!(
        results[0].2.contains("hi") && results[0].2.contains("committed"),
        "{:?}",
        results[0]
    );
    let out = &results[1].2;
    assert!(!out.contains("sk-not-for-commands"), "{out}");
    let visible: usize = out
        .lines()
        .rev()
        .find_map(|l| l.trim().parse().ok())
        .unwrap();
    assert!(visible < 20, "only the namespace's processes: {out}");
    assert!(results[2].2.contains("started"), "{:?}", results[2]);
    assert!(results[3].2.contains("stopped"), "{:?}", results[3]);

    // Another chat has a namespace of its own: it can't see (or read the
    // environment and pipes of) this chat's processes.
    let sleeps = "for p in /proc/[0-9]*; do tr '\\0' ' ' < $p/cmdline 2>/dev/null; echo; done | grep -c '^sleep 30[1]'; true";
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({"command": "sleep 301 >/dev/null 2>&1 & echo $! > pid; echo started"}),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "again").await;
    h.settle(&s.id).await;
    let other = h.session().await;
    auto(&h, &other.id).await;
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        json!({ "command": sleeps }),
    )]));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&other.id, "look").await;
    h.settle(&other.id).await;
    let theirs = tool_results(&h.entries(&other.id).await);
    assert_eq!(theirs[0].2.lines().next(), Some("0"), "{theirs:?}");
    h.mock.push(Reply::Tools(vec![
        ("bash".into(), json!({ "command": sleeps })),
        (
            "bash".into(),
            json!({"command": "kill $(cat pid) && echo stopped"}),
        ),
    ]));
    h.mock.push(Reply::Text("ok".into()));
    h.send(&s.id, "and stop it").await;
    h.settle(&s.id).await;
    let mine = tool_results(&h.entries(&s.id).await);
    let n = mine.len();
    assert_eq!(mine[n - 2].2.lines().next(), Some("1"), "{mine:?}");
    assert!(mine[n - 1].2.contains("stopped"), "{mine:?}");
}

async fn auto(h: &Harness, id: &str) {
    h.engine
        .handle(
            Request::UpdateSession {
                session_id: id.into(),
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
}
