//! Two servers on the same data, started at the same moment: the second
//! refuses before opening anything, even though the first hasn't written
//! `server.json` yet (it is still starting). Here the "first" is this test
//! holding the data lock.
#![cfg(unix)]

use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn serve(home: &std::path::Path, background: bool) -> (Option<i32>, String, Duration) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tessera"));
    cmd.arg("--home")
        .arg(home)
        .args(["serve", "--bind", "127.0.0.1:0", "--no-scheduler"])
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .env_remove("TESSERA_BACKGROUND")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if background {
        cmd.env("TESSERA_BACKGROUND", "1");
    }
    let t = Instant::now();
    let out = cmd.output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        t.elapsed(),
    )
}

#[test]
fn a_second_server_on_the_same_data_refuses_before_opening_it() {
    let home = tempfile::tempdir().unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.path().join("server.lock"))
        .unwrap();
    // SAFETY: a valid descriptor while `held` lives.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );

    let (code, stderr, took) = serve(home.path(), false);
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("already running here"), "{stderr}");
    // It waited a little for a server that might be letting go, no more.
    assert!(took < Duration::from_secs(30), "took {took:?}");
    // Started by the background launcher: the code that makes it stop.
    let (code, stderr, _) = serve(home.path(), true);
    assert_eq!(code, Some(69), "{stderr}");
    // Nothing was opened: no database, no `server.json`.
    assert!(!home.path().join("tessera.db").exists());
    assert!(!home.path().join("server.json").exists());
    drop(held);
}
