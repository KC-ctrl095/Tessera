//! Externally bounded starvation regressions. The probe runs on the same
//! four-worker runtime as callers contending on the synchronous database.
//! The raw case is the old wait pattern and must miss its heartbeat.

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

fn subprocess(case: &str) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "store::stall_tests::stall_subprocess",
            "--ignored",
            "--nocapture",
        ])
        .env("TESSERA_STORE_STALL_CASE", case)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= end {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("owned store-stall subprocess exceeded external deadline: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{case}: {output:?}");
    eprintln!("{case}: {}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn raw_database_waits_reproduce_an_all_worker_stall() {
    subprocess("raw");
}

#[test]
fn database_contention_leaves_runtime_workers_available() {
    subprocess("fixed");
}

#[test]
fn history_cache_contention_leaves_runtime_workers_available() {
    subprocess("cache");
}

#[test]
fn local_login_retirement_leaves_runtime_workers_available() {
    subprocess("retire");
}

#[test]
#[ignore = "run by externally bounded starvation regressions only"]
fn stall_subprocess() {
    let case = std::env::var("TESSERA_STORE_STALL_CASE").unwrap();
    let raw = case == "raw";
    let cache = case == "cache";
    let retire = case == "retire";
    let store = Arc::new(Store::in_memory().unwrap());
    let (locked, held) = std::sync::mpsc::channel();
    let (release, unlock) = std::sync::mpsc::channel();
    let s = store.clone();
    let holder = std::thread::spawn(move || {
        if cache {
            let _histories = s.histories.lock();
            locked.send(()).unwrap();
            unlock.recv_timeout(Duration::from_secs(5)).unwrap();
        } else {
            let _conn = s.conn.lock();
            locked.send(()).unwrap();
            unlock.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    });
    held.recv_timeout(Duration::from_secs(2)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let mut work = Vec::new();
    for i in 0..4 {
        let s = store.clone();
        let started = started.clone();
        work.push(rt.spawn(crate::activity::monitor(
            &crate::activity::TURN,
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                if cache {
                    s.history_cache_stats();
                } else if retire {
                    // The Windows startup writer adds this database entry point.
                    // A missing fixture login still contends on the same lock.
                    assert!(s.retire_older_devices("fixture").is_err());
                } else if raw {
                    // Exactly the old boundary: a worker waits synchronously
                    // before entering SQLite, with no Tokio core handoff.
                    let _conn = s.conn.lock();
                } else {
                    match i {
                        0 => {
                            s.entries("fixture", 0).unwrap();
                        }
                        1 => {
                            s.kv_set("fixture", "value").unwrap();
                        }
                        2 => {
                            s.history("fixture").unwrap();
                        }
                        _ => {
                            s.take_due_notifications(0).unwrap();
                        }
                    }
                }
            },
        )));
    }
    let end = Instant::now() + Duration::from_secs(2);
    while started.load(Ordering::SeqCst) != 4 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(started.load(Ordering::SeqCst), 4);
    let (answer, probe) = std::sync::mpsc::channel();
    let heartbeat = rt.spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        answer.send(()).unwrap();
    });
    let responsive = probe.recv_timeout(Duration::from_millis(500)).is_ok();
    let report = crate::activity::report();
    release.send(()).unwrap();
    holder.join().unwrap();
    rt.block_on(async {
        for w in work {
            w.await.unwrap();
        }
        heartbeat.await.unwrap();
    });
    assert_eq!(responsive, !raw, "raw={raw}, runtime heartbeat; {report}");
    assert!(
        report.contains(if raw {
            "active=agent:turn"
        } else {
            "active=store:"
        }),
        "{report}"
    );
    println!(
        "4 contending callers: heartbeat responsive={responsive}; watchdog retained active operations"
    );
}
