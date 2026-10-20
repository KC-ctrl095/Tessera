//! End-to-end environment isolation through both shell tool entry points.
#![cfg(unix)]

mod common;

use common::*;
use serde_json::json;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};
use tessera_proto::EntryBody;

#[test]
fn bash_and_read_only_runner_do_not_inherit_unrequested_server_environment() {
    // Set the sentinel on an isolated server subprocess, never by mutating the
    // parallel test runner's environment or the user's real configuration.
    for mode in ["list", "confined"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sentinel_server_subprocess", "--nocapture"])
            .env("UNREQUESTED_SERVER_SENTINEL", "fixture-not-a-secret")
            .env("TESSERA_SENTINEL_MODE", mode)
            .process_group(0)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= until {
                // The immediately preceding try_wait returned None: the
                // unreaped leader still owns this numeric group. No delayed
                // group signal is allowed after it has been reaped.
                unsafe {
                    libc::killpg(child.id() as i32, libc::SIGKILL);
                }
                let _ = child.kill();
                let reap_until = Instant::now() + Duration::from_secs(5);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => panic!(
                            "{mode}: sentinel fixture exceeded 60s; group killed and reaped ({status})"
                        ),
                        Ok(None) if Instant::now() < reap_until => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Ok(None) => panic!(
                            "{mode}: sentinel fixture group killed but final reap exceeded 5s"
                        ),
                        Err(error) => panic!("{mode}: sentinel fixture final reap failed: {error}"),
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "{mode}: sentinel server fixture: {status}"
        );
    }
}

#[tokio::test]
async fn sentinel_server_subprocess() {
    if std::env::var_os("UNREQUESTED_SERVER_SENTINEL").is_none() {
        return;
    }
    let mode = std::env::var("TESSERA_SENTINEL_MODE").unwrap_or_else(|_| "list".into());
    if mode == "confined"
        && (!tessera_core::sandbox::support().available
            || !tessera_core::sandbox::confines_writes())
    {
        eprintln!("SKIP confined sentinel mode: namespaces/Landlock write confinement unavailable");
        return;
    }
    let security = if mode == "confined" {
        "[security]\nsandbox = \"strict\""
    } else {
        "[security]\nsandbox = \"off\""
    };
    let h = harness_agent(
        vec![
            Reply::Tools(vec![(
                "bash".into(),
                json!({"command": "test -z \"${UNREQUESTED_SERVER_SENTINEL+x}\" && echo bash-clean"}),
            )]),
            Reply::Tools(vec![(
                "agent".into(),
                json!({"wait": true, "tasks": [{"name": "env-check", "prompt": "SENTINEL-READONLY-FIXTURE", "readonly": true}]}),
            )]),
            Reply::Text("done".into()),
        ],
        100_000,
        security,
    )
    .await;
    // A listed cargo command runs actual project code through bash_readonly,
    // not a second copy of the environment-filter helper.
    std::fs::create_dir(h.work.join("src")).unwrap();
    std::fs::write(
        h.work.join("Cargo.toml"),
        "[package]\nname = \"sentinel_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
    )
    .unwrap();
    // Pre-generated dependency-free lockfile: the read-only project must not
    // need any Cargo writes. Only target/temp/cache belong in private scratch.
    std::fs::write(
        h.work.join("Cargo.lock"),
        "version = 3\n\n[[package]]\nname = \"sentinel_fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        h.work.join("src/lib.rs"),
        "#[test]\nfn server_sentinel_is_absent() { assert!(std::env::var_os(\"UNREQUESTED_SERVER_SENTINEL\").is_none(), \"server environment leaked into read-only runner\"); }\n",
    )
    .unwrap();
    h.mock.route(
        "SENTINEL-READONLY-FIXTURE",
        Reply::Tools(vec![(
            "bash_readonly".into(),
            json!({"command": "cargo test --locked --offline --quiet", "timeout": 30}),
        )]),
    );
    h.mock
        .route("SENTINEL-READONLY-FIXTURE", Reply::Text("checked".into()));
    let session = h.session().await;
    h.send(&session.id, "Check both shell environments.").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(40), h.engine.wait_idle(&session.id))
        .await
        .expect("sentinel check did not finish");
    let entries = h.entries(&session.id).await;
    let parent = tool_results(&entries);
    assert!(
        parent
            .iter()
            .any(|(name, error, text)| { name == "bash" && !error && text.contains("bash-clean") }),
        "{parent:?}"
    );
    let child_id = entries
        .iter()
        .find_map(|entry| match &entry.body {
            EntryBody::ToolResult {
                tool,
                details: Some(details),
                ..
            } if tool == "agent" => details["subagents"][0]["session_id"]
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .expect("read-only fixture did not start");
    let results = tool_results(&h.entries(&child_id).await);
    assert!(
        results.iter().any(|(name, error, text)| {
            name == "bash_readonly" && !error && text.contains("1 passed")
        }),
        "read-only test did not execute successfully: {results:?}"
    );
}
