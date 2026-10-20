//! Bound private speech subprocesses, including their pipes and descendants.
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::Failure;

pub(super) const AUDIO_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const AUDIO_SECONDS: f64 = 120.0;
pub(super) const UNPACK_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub time: Duration,
    pub output: usize,
    pub memory: u64,
    pub files: u64,
}
impl Limits {
    pub fn engine(time: Duration) -> Self {
        Self {
            time,
            output: 256 * 1024,
            memory: 2 * 1024 * 1024 * 1024,
            files: AUDIO_BYTES * 2,
        }
    }
}

struct Tree {
    #[cfg(unix)]
    pid: u32,
    #[cfg(windows)]
    job: crate::platform::windows::TreeJob,
}
impl Drop for Tree {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: our own child's dedicated process group, never another job.
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        self.job.end();
    }
}

async fn bounded(mut pipe: impl AsyncRead + Unpin, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buf = [0; 8192];
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(n) > cap {
            return Err(std::io::Error::other(
                "speech engine output is larger than allowed",
            ));
        }
        bytes.extend_from_slice(&buf[..n]);
    }
}

fn folder_bytes(path: &Path, remaining: &mut usize) -> std::io::Result<u64> {
    let mut size = 0u64;
    for entry in std::fs::read_dir(path)? {
        if *remaining == 0 {
            return Err(std::io::Error::other("too many speech engine files"));
        }
        *remaining -= 1;
        let entry = entry?;
        let meta = entry.path().symlink_metadata()?;
        size = size.saturating_add(if meta.is_dir() {
            folder_bytes(&entry.path(), remaining)?
        } else {
            meta.len()
        });
    }
    Ok(size)
}

pub(super) async fn run(
    bin: &Path,
    args: &[String],
    stdin: Option<String>,
    tmp: &Path,
    limits: Limits,
) -> Result<Vec<u8>, Failure> {
    run_checked(bin, args, stdin, tmp, limits, &|used| {
        super::space::admit(
            super::space::available(tmp)?,
            limits.files.saturating_sub(used),
        )
    })
    .await
}

pub(super) async fn run_checked(
    bin: &Path,
    args: &[String],
    stdin: Option<String>,
    tmp: &Path,
    limits: Limits,
    check: &(dyn Fn(u64) -> Result<(), String> + Sync),
) -> Result<Vec<u8>, Failure> {
    use std::process::Stdio;
    check(0).map_err(Failure::Failed)?;
    let started = tokio::time::Instant::now();
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .current_dir(tmp)
        .env_clear()
        .env("TMPDIR", tmp)
        .env("TMP", tmp)
        .env("TEMP", tmp)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
        // SAFETY: async-signal-safe calls only between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                crate::tools::bash::yield_to_server();
                for (resource, value) in [
                    (libc::RLIMIT_AS, limits.memory),
                    (libc::RLIMIT_FSIZE, limits.files),
                    (libc::RLIMIT_CPU, limits.time.as_secs().max(1)),
                    (libc::RLIMIT_CORE, 0),
                ] {
                    let limit = libc::rlimit {
                        rlim_cur: value as _,
                        rlim_max: value as _,
                    };
                    if libc::setrlimit(resource, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    for k in ["SystemRoot", "windir"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let name = bin.file_name().unwrap_or_default().to_string_lossy();
    #[cfg(windows)]
    let child = crate::platform::windows::spawn_in_job(
        &mut cmd,
        crate::platform::windows::CREATE_NO_WINDOW
            | crate::platform::windows::BELOW_NORMAL_PRIORITY_CLASS,
        crate::platform::windows::Job::Voice {
            memory: limits.memory as usize,
        },
    );
    #[cfg(not(windows))]
    let child = cmd.spawn();
    let mut child = child.map_err(|e| Failure::Failed(format!("{name} didn't start: {e}")))?;
    let pid = child
        .id()
        .ok_or_else(|| Failure::Failed("speech engine has no process id".into()))?;
    let tree = Tree {
        #[cfg(unix)]
        pid,
        #[cfg(windows)]
        job: crate::platform::windows::take_job(pid)
            .ok_or_else(|| Failure::Failed("speech engine has no Windows job".into()))?,
    };
    let mut input = child.stdin.take();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let work = async {
        tokio::try_join!(
            async {
                if let (Some(text), Some(input)) = (stdin, input.as_mut()) {
                    input.write_all(text.as_bytes()).await?;
                }
                drop(input);
                Ok::<_, std::io::Error>(())
            },
            bounded(stdout, limits.output),
            bounded(stderr, limits.output.min(64 * 1024)),
            child.wait(),
        )
    };
    let scratch = PathBuf::from(tmp);
    let disk = async {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            let used = folder_bytes(&scratch, &mut 4096)?;
            check(used).map_err(std::io::Error::other)?;
            if used > limits.files {
                return Err::<(), _>(std::io::Error::other(
                    "speech engine files are larger than allowed",
                ));
            }
        }
    };
    // One absolute deadline covers stdin, stdout/stderr drainage and exit.
    let result = tokio::time::timeout_at(started + limits.time, async {
        tokio::select! { out = work => out, bad = disk => Err(bad.unwrap_err()) }
    })
    .await;
    drop(tree); // also stops descendants after an otherwise successful exit
    if !matches!(&result, Ok(Ok(_))) {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let (_, out, err, status) = result
        .map_err(|_| {
            Failure::Failed(format!(
                "{name} took longer than {} ms",
                limits.time.as_millis()
            ))
        })?
        .map_err(|e| Failure::Failed(format!("{name} failed: {e}")))?;
    let used = folder_bytes(tmp, &mut 4096)
        .map_err(|e| Failure::Failed(format!("can't check speech engine files: {e}")))?;
    check(used).map_err(Failure::Failed)?;
    if used > limits.files {
        return Err(Failure::Failed(
            "speech engine files are larger than allowed".into(),
        ));
    }
    if !status.success() {
        let err = String::from_utf8_lossy(&err);
        let tail: Vec<_> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        return Err(Failure::Failed(format!(
            "{name} stopped ({status}): {}",
            crate::util::truncate(&tail[tail.len().saturating_sub(3)..].join(" / "), 300)
        )));
    }
    Ok(out)
}
