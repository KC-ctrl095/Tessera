//! Native Windows-only Host lifecycle acceptance, in throwaway folders.
use super::*;
use std::path::PathBuf;

struct Rig {
    host: Host,
    dir: tempfile::TempDir,
    _dog: Watchdog,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.host.close_all("test over");
    }
}
fn rig(limits: Limits) -> Rig {
    Rig {
        host: Host::new(limits, Arc::new(|_| {})),
        dir: tempfile::tempdir().unwrap(),
        _dog: watchdog("Windows terminal Host test", 40),
    }
}
fn opening(r: &Rig, id: &str, owner: &str) -> Opening {
    let a = crate::pty::account().unwrap();
    let cmd = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap()
        .join("System32/cmd.exe");
    Opening {
        id: id.into(),
        owner: owner.into(),
        owner_name: owner.into(),
        place: "test".into(),
        sandboxed: false,
        start: crate::pty::Start {
            program: cmd.into_os_string(),
            args: ["/d", "/q", "/k", "chcp 65001 >nul"]
                .map(Into::into)
                .to_vec(),
            cwd: r.dir.path().to_owned(),
            env: a.environment(),
            cols: 80,
            rows: 24,
            sandbox: None,
        },
    }
}
fn sink() -> (Sink, mpsc::Receiver<Out>) {
    let (tx, rx) = mpsc::channel(32);
    (Sink { conn: 1, tx }, rx)
}
async fn output(
    host: &Host,
    id: &str,
    owner: &str,
    rx: &mut mpsc::Receiver<Out>,
    marker: &str,
) -> String {
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await.unwrap() {
                Out::Output { seq, data, .. } => {
                    host.ack(id, owner, seq + data.len() as u64).unwrap();
                    text.push_str(&String::from_utf8_lossy(&data));
                    if text.contains(marker) {
                        return;
                    }
                }
                other => panic!("expected output: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    text
}

#[tokio::test]
async fn ownership_counts_input_resize_exit_and_reconnect_replay() {
    let r = rig(Limits {
        max_open: 1,
        tick: Duration::from_millis(20),
        ..Limits::default()
    });
    let (s, mut rx) = sink();
    r.host.open(opening(&r, "w1", "phone"), s.clone()).unwrap();
    assert!(
        r.host
            .open(opening(&r, "w2", "phone"), s.clone())
            .unwrap_err()
            .contains("most allowed")
    );
    assert!(
        r.host
            .open(opening(&r, "w1", "other"), s.clone())
            .unwrap_err()
            .contains("already open")
    );
    assert!(r.host.input("w1", "other", b"exit\r").await.is_err());
    assert!(r.host.close("w1", "other", "no").is_err());
    assert!(r.host.attach("w1", "other", s.clone(), None).is_err());
    assert!(
        r.host
            .input("w1", "phone", &vec![b'x'; MAX_INPUT + 1])
            .await
            .unwrap_err()
            .contains("paste it in parts")
    );
    r.host.resize("w1", "phone", 120, 40).unwrap();
    r.host
        .input("w1", "phone", b"echo replay^_marker\r")
        .await
        .unwrap();
    output(&r.host, "w1", "phone", &mut rx, "replay_marker").await;
    r.host.detach("w1", "phone").unwrap();
    let (s2, mut rx2) = sink();
    r.host.attach("w1", "phone", s2, Some((80, 24))).unwrap();
    output(&r.host, "w1", "phone", &mut rx2, "replay_marker").await;
    r.host.input("w1", "phone", b"exit 7\r").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(out) = rx2.recv().await {
            match out {
                Out::Output { seq, data, .. } => {
                    r.host.ack("w1", "phone", seq + data.len() as u64).unwrap()
                }
                Out::Exit { code, reason, .. } => {
                    assert_eq!(code, Some(7));
                    assert!(reason.contains("code 7"));
                    return;
                }
            }
        }
        panic!("no exit");
    })
    .await
    .unwrap();
    assert_eq!(r.host.count(), 0);
}

#[tokio::test]
async fn grace_expiry_and_signout_close_jobs() {
    let r = rig(Limits {
        grace: Duration::from_millis(100),
        tick: Duration::from_millis(20),
        ..Limits::default()
    });
    let (s, _rx) = sink();
    r.host
        .open(opening(&r, "grace", "phone"), s.clone())
        .unwrap();
    r.host.detach("grace", "phone").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.host.count() != 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        r.host
            .ended("grace", "phone")
            .unwrap()
            .1
            .contains("no device showed")
    );
    r.host.open(opening(&r, "out", "phone"), s).unwrap();
    r.host.close_owner("phone", "signed out");
    assert_eq!(r.host.count(), 0);
    assert!(
        r.host
            .ended("out", "phone")
            .unwrap()
            .1
            .contains("signed out")
    );
}

#[tokio::test]
async fn flood_close_does_not_block_tokio_and_closes_the_pty() {
    let r = rig(Limits {
        window: 4096,
        ..Limits::default()
    });
    let (s, _rx) = sink();
    r.host.open(opening(&r, "flood", "phone"), s).unwrap();
    // Native process identities use handles, not reusable PIDs, for assertions.
    let term = r.host.get("flood", "phone").unwrap();
    r.host
        .input("flood", "phone", b"for /L %i in (1,0,2) do @echo flood\r")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let start = Instant::now();
    r.host.close("flood", "phone", "test close").unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(r.host.count(), 0);
    assert!(term.pty.write_all(b"no").await.is_err());
    assert!(term.pty.resize(80, 24).is_err());
}
