//! Running the `tessera` program from the desktop app, with a time limit,
//! and waiting for the program to end, never for its output to close.
//!
//! `tokio::process::Command::output()` returns only when the program's
//! output pipes close. A program it starts that outlives it (`tessera
//! service install` starts the launcher, which starts the server) can hold
//! them open for as long as it runs: Windows hands a child every
//! inheritable handle of its parent, and a shell's `cmd &` does the same on
//! Linux. Setup then waited for ever on "Starting Tessera…". Here the
//! program's end is what counts; its output is whatever arrived by then
//! (plus a short moment for the rest).

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// After the program has ended, how long the rest of its output may take.
const DRAIN: Duration = Duration::from_secs(2);
/// The most output kept from one stream.
const KEEP: usize = 1 << 20;

/// How a run ended.
#[derive(Debug)]
pub struct Ran {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum RunError {
    /// It couldn't be started (or waited on).
    Start(std::io::Error),
    /// Still running when the time allowed ran out: it was ended.
    TimedOut,
}

/// Run `cmd` (stdin: `input`, or nothing) for at most `limit`. Dropping the
/// returned future ends the program too, and the reading of its output.
pub async fn run(
    cmd: &mut tokio::process::Command,
    input: Option<Vec<u8>>,
    limit: Duration,
) -> Result<Ran, RunError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(RunError::Start)?;
    let out = child.stdout.take().map(collect);
    let err = child.stderr.take().map(collect);
    let ended = async {
        if let (Some(data), Some(mut pipe)) = (input, child.stdin.take()) {
            // A program that doesn't read its input still ends; its exit
            // status says how it went.
            let _ = pipe.write_all(&data).await;
            drop(pipe);
        }
        child.wait().await
    };
    let waited = tokio::time::timeout(limit, ended).await;
    let status = match waited {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(RunError::Start(e)),
        Err(_) => {
            let _ = child.start_kill();
            return Err(RunError::TimedOut);
        }
    };
    // Side by side: a child left behind holds both streams, and each would
    // otherwise wait its own moment.
    let (stdout, stderr) = tokio::join!(finish(out), finish(err));
    Ok(Ran {
        success: status.success(),
        stdout,
        stderr,
    })
}

/// A stream read in the background. Dropping it ends the reading, so
/// nothing keeps reading (or keeps the pipe open) once `run` is over, however
/// it ended: done, timed out, or dropped.
struct Reader {
    buf: Arc<Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read `stream` in the background, keeping what arrives.
fn collect<R: AsyncRead + Unpin + Send + 'static>(mut stream: R) -> Reader {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let keep = buf.clone();
    let task = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let mut b = keep.lock();
            let room = KEEP.saturating_sub(b.len());
            b.extend_from_slice(&chunk[..n.min(room)]);
        }
    });
    Reader { buf, task }
}

/// What arrived, once the stream ended or [`DRAIN`] passed.
async fn finish(reader: Option<Reader>) -> Vec<u8> {
    let Some(mut r) = reader else {
        return Vec::new();
    };
    let _ = tokio::time::timeout(DRAIN, &mut r.task).await;
    std::mem::take(&mut *r.buf.lock())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> tokio::process::Command {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", script]);
        c
    }

    /// The setup hang, on Linux: a program that leaves a child behind holding
    /// its output. `output()` waits for that child; `run` doesn't.
    #[tokio::test]
    async fn a_program_that_leaves_a_child_behind_doesnt_hold_the_caller() {
        let script = "echo ready; sleep 20 &";
        let old = tokio::time::timeout(Duration::from_secs(2), sh(script).output()).await;
        assert!(
            old.is_err(),
            "output() should still be waiting for the child"
        );

        let t = Instant::now();
        let ran = run(&mut sh(script), None, Duration::from_secs(10))
            .await
            .expect("it ran");
        assert!(ran.success);
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "ready");
        assert!(
            t.elapsed() < DRAIN + Duration::from_secs(2),
            "took {:?}",
            t.elapsed()
        );
    }

    /// A child left behind that keeps writing, and writes `$MARK` once
    /// nobody reads its output any more (or gives up after ten seconds).
    fn writer_left_behind(mark: &std::path::Path) -> tokio::process::Command {
        let mut c = sh("trap '' PIPE
            ( n=0
              while [ $n -lt 100 ]; do
                /bin/echo tick || { echo gone > \"$MARK\"; exit 0; }
                n=$((n+1)); sleep 0.1
              done ) &
            sleep 30");
        c.env("MARK", mark);
        c
    }

    /// A fresh path for `$MARK`, gone after the test.
    struct Mark(std::path::PathBuf);
    impl Mark {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("tessera-cli-run-{}-{name}", std::process::id()));
            let _ = std::fs::remove_file(&p);
            Self(p)
        }
    }
    impl Drop for Mark {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn let_go(mark: &std::path::Path) -> bool {
        for _ in 0..50 {
            if mark.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_timed_out_run_stops_reading_what_it_left_behind() {
        let mark = Mark::new("timed-out");
        let mark = mark.0.as_path();
        let r = run(
            &mut writer_left_behind(mark),
            None,
            Duration::from_millis(500),
        )
        .await;
        assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
        assert!(let_go(mark).await, "its output is still being read");
    }

    #[tokio::test]
    async fn a_dropped_run_stops_reading_what_it_left_behind() {
        let mark = Mark::new("dropped");
        let mark = mark.0.as_path();
        let mut cmd = writer_left_behind(mark);
        let r = tokio::time::timeout(
            Duration::from_millis(500),
            run(&mut cmd, None, Duration::from_secs(30)),
        )
        .await;
        assert!(r.is_err(), "it should still have been running");
        assert!(let_go(mark).await, "its output is still being read");
    }

    #[tokio::test]
    async fn a_program_that_runs_too_long_is_ended() {
        let t = Instant::now();
        let r = run(&mut sh("sleep 20"), None, Duration::from_millis(300)).await;
        assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "took {:?}",
            t.elapsed()
        );
    }

    #[tokio::test]
    async fn exit_status_output_and_input_come_through() {
        let ran = run(
            &mut sh("read line; echo \"got $line\"; echo oops >&2; exit 3"),
            Some(b"hello\n".to_vec()),
            Duration::from_secs(10),
        )
        .await
        .expect("it ran");
        assert!(!ran.success);
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "got hello");
        assert_eq!(String::from_utf8_lossy(&ran.stderr).trim(), "oops");
    }

    #[tokio::test]
    async fn a_missing_program_is_a_start_error() {
        let mut cmd = tokio::process::Command::new("/nonexistent/tessera-test");
        let r = run(&mut cmd, None, Duration::from_secs(5)).await;
        assert!(matches!(r, Err(RunError::Start(_))), "{r:?}");
    }
}
