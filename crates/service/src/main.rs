//! `tessera-service.exe`: keeps Tessera running in the background on
//! Windows, the way systemd does on Linux. Windows starts it at sign-in
//! (`tessera service install` and `tessera node service install` add it to
//! the user's startup list). It has no window and no console of its own, and
//! runs `tessera.exe` from its own folder with the arguments it was given:
//!
//! ```text
//! tessera-service.exe --log <file> [--home <dir>] serve --bind 127.0.0.1:7878
//! ```
//!
//! Tessera's output goes to the log file (the previous one is kept as
//! `<file>.old` once it passes 5 MB). Like `Restart=on-failure`: an exit
//! with code 0 (stopped on purpose) ends it, as does finding another server
//! already running on the same data (code 69); a failure (a crash, or the
//! server's watchdog ending a server that was stuck) starts Tessera again
//! after 5 seconds. After 5 failures within 2 minutes it waits 10 minutes
//! before the next try, and so on: a computer left on to serve gets its
//! server back once whatever stopped it (a busy port, a full disk) is gone,
//! without starting it over and over. While it runs, `<file>.pid` holds its
//! own process id and Tessera's.

#![cfg_attr(windows, windows_subsystem = "windows")]
// Elsewhere it only explains itself; the rest is for Windows (and tests).
#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// What the command line asked for.
#[derive(Debug, PartialEq)]
struct Args {
    log: PathBuf,
    home: Option<PathBuf>,
    rest: Vec<OsString>,
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let (mut log, mut home) = (None, None);
    let mut rest = Vec::new();
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--log") => log = args.next().map(PathBuf::from),
            Some("--home") => home = args.next().map(PathBuf::from),
            _ => {
                rest.push(a);
                rest.extend(args);
                break;
            }
        }
    }
    let log = log.ok_or("--log <file> is missing")?;
    if rest.is_empty() {
        return Err("nothing to run: give tessera's arguments (e.g. `serve`)".into());
    }
    Ok(Args { log, home, rest })
}

/// Failures within this window count towards a longer pause.
const WINDOW: Duration = Duration::from_secs(120);
const MAX_FAILURES: usize = 5;
/// The wait before the next start after a failure, and after too many.
const AGAIN: Duration = Duration::from_secs(5);
const PAUSE: Duration = Duration::from_secs(600);
const LOG_LIMIT: u64 = 5 << 20;
/// The exit code of a server its watchdog ended because it was stuck.
const STUCK: i32 = 75;
/// The exit code of a server that found another one already running on
/// the same data: trying again would only fail again, so it stops.
const ALREADY_RUNNING: i32 = 69;

/// How long to wait before starting it again after a failure at `now`,
/// given the earlier ones (kept: those within [`WINDOW`]; a long pause
/// starts the count over).
fn next_try(failures: &mut Vec<Duration>, now: Duration) -> Duration {
    failures.retain(|t| now.saturating_sub(*t) < WINDOW);
    failures.push(now);
    if failures.len() < MAX_FAILURES {
        AGAIN
    } else {
        failures.clear();
        PAUSE
    }
}

/// Why it stopped, for the log.
fn why(code: i32) -> String {
    match code {
        STUCK => "its watchdog ended it because it was stuck".into(),
        c => format!("it stopped with code {c}"),
    }
}

fn pid_file(log: &std::path::Path) -> PathBuf {
    let mut p = log.as_os_str().to_owned();
    p.push(".pid");
    PathBuf::from(p)
}

fn claim_file(log: &std::path::Path) -> PathBuf {
    let mut p = log.as_os_str().to_owned();
    p.push(".lock");
    PathBuf::from(p)
}

/// Whether a pid file's text names launcher `me` (another one's stays).
fn names(text: &str, me: u32) -> bool {
    text.split_whitespace()
        .next()
        .and_then(|n| n.parse::<u32>().ok())
        == Some(me)
}

/// One launcher per log, that is per startup entry. None means an
/// existing launcher holds it; every other failure refuses to spawn.
#[cfg(windows)]
fn claim(log: &std::path::Path) -> std::io::Result<Option<std::fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    claim_with(
        || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .share_mode(0)
                .open(claim_file(log))
        },
        || std::thread::sleep(Duration::from_millis(250)),
    )
}

fn claim_with<T>(
    mut open: impl FnMut() -> std::io::Result<T>,
    mut wait: impl FnMut(),
) -> std::io::Result<Option<T>> {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    // One that was just ended can take a moment to let go of it. Sharing
    // control is part of Windows file opening, not an optional feature:
    // there is no safe unsupported-feature path that skips ownership.
    for _ in 0..40 {
        match open() {
            Ok(file) => return Ok(Some(file)),
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => wait(),
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

fn claim_error(log: &std::path::Path, error: &std::io::Error) -> String {
    format!(
        "--- tessera-service: couldn't claim {}; refusing to start Tessera: {error}. Check this folder's permissions and free space.",
        claim_file(log).display()
    )
}

fn log_claim(log: &std::path::Path, message: &str) {
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        Ok(mut file) => {
            if writeln!(file, "{message}").is_err() {
                eprintln!("{message}");
            }
        }
        // The same folder may be unwritable. Best effort, but never start
        // without the claim or print the child's arguments (credentials).
        Err(_) => eprintln!("{message}"),
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(match parse(std::env::args_os().skip(1)) {
        Ok(args) => run(args),
        // No console to say it on: the startup entry Tessera writes is
        // always well-formed, so this is someone starting it by hand.
        Err(_) => 2,
    });
}

#[cfg(not(windows))]
fn main() {
    let _ = parse(std::env::args_os().skip(1));
    eprintln!(
        "tessera-service runs Tessera in the background on Windows. Here, use `tessera service install`."
    );
    std::process::exit(2);
}

#[cfg(windows)]
fn run(args: Args) -> i32 {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("tessera.exe")))
    else {
        return 1;
    };
    if let Some(dir) = args.log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // A second launcher for the same entry (started twice, or by hand
    // beside it) would start a second Tessera and take over the pid file.
    let _claim = match claim(&args.log) {
        Ok(Some(file)) => file,
        Ok(None) => {
            log_claim(
                &args.log,
                "--- tessera-service: another one already runs this, so this one stops",
            );
            return 0;
        }
        Err(e) => {
            log_claim(&args.log, &claim_error(&args.log, &e));
            return 1;
        }
    };
    let started = std::time::Instant::now();
    let mut failures = Vec::new();
    loop {
        if std::fs::metadata(&args.log).is_ok_and(|m| m.len() > LOG_LIMIT) {
            let mut old = args.log.as_os_str().to_owned();
            old.push(".old");
            let _ = std::fs::rename(&args.log, PathBuf::from(old));
        }
        let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&args.log)
        else {
            return 1;
        };
        let _ = writeln!(log, "--- tessera-service: starting {}", exe.display());
        let (Ok(out), Ok(err)) = (log.try_clone(), log.try_clone()) else {
            return 1;
        };
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(&args.rest)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .env("TESSERA_BACKGROUND", "1")
            // A console program, with no console window.
            .creation_flags(CREATE_NO_WINDOW);
        if let Some(home) = &args.home {
            cmd.env("TESSERA_HOME", home);
        }
        if std::env::var_os("TESSERA_LOG").is_none() {
            cmd.env("TESSERA_LOG", "info");
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(log, "--- tessera-service: can't start it: {e}");
                return 1;
            }
        };
        let _ = std::fs::write(
            pid_file(&args.log),
            format!("{} {}\n", std::process::id(), child.id()),
        );
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
        if code == 0 || code == ALREADY_RUNNING {
            if code == ALREADY_RUNNING {
                let _ = writeln!(
                    log,
                    "--- tessera-service: another Tessera server already runs on this data, so this one stops"
                );
            }
            if std::fs::read_to_string(pid_file(&args.log))
                .is_ok_and(|t| names(&t, std::process::id()))
            {
                let _ = std::fs::remove_file(pid_file(&args.log));
            }
            return 0;
        }
        let wait = next_try(&mut failures, started.elapsed());
        let _ = if wait == PAUSE {
            writeln!(
                log,
                "--- tessera-service: {}; it failed {MAX_FAILURES} times in {} minutes, so it waits {} minutes before the next try (the lines above say why)",
                why(code),
                WINDOW.as_secs() / 60,
                PAUSE.as_secs() / 60
            )
        } else {
            writeln!(
                log,
                "--- tessera-service: {}; starting it again in {} seconds",
                why(code),
                wait.as_secs()
            )
        };
        drop(log);
        std::thread::sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> impl Iterator<Item = OsString> {
        v.iter().map(OsString::from).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn arguments() {
        let a = parse(os(&[
            "--log",
            r"C:\Users\Ann Lee\AppData\Roaming\tessera\logs\server.log",
            "--home",
            r"D:\tessera",
            "serve",
            "--bind",
            "127.0.0.1:7878",
        ]))
        .unwrap();
        assert_eq!(
            a.log,
            PathBuf::from(r"C:\Users\Ann Lee\AppData\Roaming\tessera\logs\server.log")
        );
        assert_eq!(a.home, Some(PathBuf::from(r"D:\tessera")));
        assert_eq!(a.rest, vec!["serve", "--bind", "127.0.0.1:7878"]);
        // Tessera's own `--log`-looking arguments after the first stay its own.
        let a = parse(os(&["--log", "x.log", "node", "run", "--home", "y"])).unwrap();
        assert_eq!(a.home, None);
        assert_eq!(a.rest, vec!["node", "run", "--home", "y"]);
        assert!(parse(os(&["serve"])).is_err());
        assert!(parse(os(&["--log", "x.log"])).is_err());
    }

    #[test]
    fn pauses_after_five_quick_failures_and_never_gives_up() {
        let s = Duration::from_secs;
        let mut f = Vec::new();
        for t in [0, 10, 20, 30] {
            assert_eq!(next_try(&mut f, s(t)), AGAIN);
        }
        assert_eq!(next_try(&mut f, s(40)), PAUSE);
        // After the pause it tries again, quickly at first.
        assert_eq!(next_try(&mut f, s(650)), AGAIN);
        // Failures spread out keep it going without pauses.
        let mut f = Vec::new();
        for t in (0..20).map(|i| i * 60) {
            assert_eq!(next_try(&mut f, s(t)), AGAIN);
        }
        assert_eq!(why(STUCK), "its watchdog ended it because it was stuck");
        assert_eq!(why(1), "it stopped with code 1");
    }

    #[test]
    fn the_pid_file_sits_next_to_the_log() {
        assert_eq!(
            pid_file(std::path::Path::new("/x/logs/server.log")),
            PathBuf::from("/x/logs/server.log.pid")
        );
        assert_eq!(
            claim_file(std::path::Path::new("/x/logs/server.log")),
            PathBuf::from("/x/logs/server.log.lock")
        );
    }

    #[test]
    fn a_launcher_removes_only_its_own_pid_file() {
        assert!(names("4120 4188\n", 4120));
        // Another launcher's (it took over, or was started since).
        assert!(!names("5000 5012\n", 4120));
        assert!(!names("", 4120));
        assert!(!names("garbage", 4120));
    }

    #[test]
    fn sharing_contention_retries_and_an_existing_claim_stops() {
        let mut opened = 0;
        let mut waits = 0;
        let claim = claim_with(
            || {
                opened += 1;
                if opened < 3 {
                    Err(std::io::Error::from_raw_os_error(32))
                } else {
                    Ok(7)
                }
            },
            || waits += 1,
        )
        .unwrap();
        assert_eq!(claim, Some(7));
        assert_eq!(waits, 2);
        let mut waits = 0;
        let claim =
            claim_with::<()>(|| Err(std::io::Error::from_raw_os_error(32)), || waits += 1).unwrap();
        assert!(claim.is_none());
        assert_eq!(waits, 40);
    }

    #[test]
    fn unexpected_or_unsupported_claim_errors_fail_closed_immediately() {
        // Windows: access denied, missing directory, invalid handle,
        // unsupported operation, I/O device error, disk full.
        for code in [5, 3, 6, 50, 1117, 112] {
            let mut calls = 0;
            let e = claim_with::<()>(
                || {
                    calls += 1;
                    Err(std::io::Error::from_raw_os_error(code))
                },
                || panic!("unexpected claim failure must not retry"),
            )
            .unwrap_err();
            assert_eq!(e.raw_os_error(), Some(code));
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn claim_failure_log_names_the_lock_and_error_not_startup_arguments() {
        let log = std::env::temp_dir().join(format!("tessera-claim-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let e = std::io::Error::from_raw_os_error(5);
        let message = claim_error(&log, &e);
        log_claim(&log, &message);
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains(&claim_file(&log).display().to_string()));
        assert!(text.contains(&e.to_string()));
        assert!(text.contains("refusing to start"));
        assert!(!text.contains("--token") && !text.contains("tsr_"));
        std::fs::remove_file(log).unwrap();
    }
}
