//! The server's watchdog. A task on the async runtime notes the time every
//! second; a thread of its own checks it. While the runtime runs tasks, the
//! thread tells systemd so (when systemd runs the server; the watchdog is
//! switched on from here, so an older binary under the same unit is never
//! restarted by it). When the runtime stops running tasks (a deadlock, or
//! every worker blocked), the thread writes down where each thread waits and
//! which of the server's locks are deadlocked, then stops telling systemd,
//! which restarts the server. Without systemd it ends a server stuck for
//! minutes, so whatever started it can start it again. It also asks the
//! server's own address for `/api/health` now and then, as a device would:
//! a server whose runtime runs but that no longer answers counts as stuck too.
//!
//! A blocked runtime can leave every worker asleep, ignore the stop signal
//! and write nothing to the log while systemd still sees a healthy process.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The runtime ran no task for this long: it counts as stuck.
const STALL: Duration = Duration::from_secs(20);
/// systemd restarts the server this long after the last "still running".
const LIMIT: Duration = Duration::from_secs(60);
/// Without systemd: a server stuck this long ends itself.
const GIVE_UP: Duration = Duration::from_secs(180);
/// Reports kept in `<data>/stalls`.
const KEEP: usize = 5;
/// How often the server's own address is asked for `/api/health`, how long
/// an answer may take, and how many missed answers in a row count as stuck.
const PROBE_EVERY: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_MISSES: u32 = 4;

/// Start watching the runtime this is called on, and the server listening
/// on `listen`.
pub fn start(data_dir: &Path, listen: SocketAddr) {
    let start = Instant::now();
    let beat = Arc::new(AtomicU64::new(0));
    let b = beat.clone();
    // Checking the watchdog itself: `TESSERA_WATCHDOG_TEST_STALL=<s>` stops
    // the ticks after that many seconds, as a stuck runtime would.
    let stall_after = std::env::var("TESSERA_WATCHDOG_TEST_STALL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if stall_after.is_some_and(|s| start.elapsed() >= s) {
                continue;
            }
            b.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
    });
    let dir = data_dir.join("stalls");
    let spawned = std::thread::Builder::new()
        .name("tessera-watchdog".into())
        .spawn(move || watch(start, &beat, &dir, probe_addr(listen)));
    if let Err(e) = spawned {
        tracing::warn!("no watchdog: {e}");
    }
}

fn watch(start: Instant, beat: &AtomicU64, dir: &Path, addr: SocketAddr) {
    let notify = Notify::from_env();
    if let Some(n) = &notify {
        n.send(&format!("WATCHDOG_USEC={}", LIMIT.as_micros()));
    }
    let (mut stuck_since, mut misses) = (None::<Instant>, 0u32);
    let mut probed = Instant::now();
    loop {
        let nap = Instant::now();
        std::thread::sleep(Duration::from_secs(2));
        // The computer slept (Windows' clock counts that time), or this
        // thread itself didn't get to run: the runtime gets a moment to
        // catch up before its silence counts.
        if reset_after_wake(nap.elapsed(), &mut probed, &mut misses, &mut stuck_since) {
            continue;
        }
        let ran = Duration::from_millis(beat.load(Ordering::Relaxed));
        let quiet = start.elapsed().saturating_sub(ran);
        if probed.elapsed() >= PROBE_EVERY {
            probed = Instant::now();
            misses = if answers(addr) { 0 } else { misses + 1 };
        }
        let trouble = if quiet >= STALL {
            Some(format!("no task ran for {} s", quiet.as_secs()))
        } else if misses >= PROBE_MISSES {
            Some(format!(
                "it didn't answer on {addr} {misses} times in a row (each waited {} s)",
                PROBE_TIMEOUT.as_secs()
            ))
        } else {
            None
        };
        let Some(trouble) = trouble else {
            if stuck_since.take().is_some() {
                tracing::warn!("the server runs and answers again");
            }
            if let Some(n) = &notify {
                n.send("WATCHDOG=1");
            }
            continue;
        };
        let since = *stuck_since.get_or_insert_with(|| {
            // The file first: a log line may be what is stuck. Then the
            // whole report in the log too, where log collectors find it.
            let text = report(&trouble);
            let file = save(dir, &text);
            eprintln!(
                "Tessera: the server is stuck ({trouble}); report saved in {}. {}\n{text}",
                file.as_deref()
                    .map_or("(not saved)".into(), |f| f.display().to_string()),
                if notify.is_some() {
                    "systemd restarts it within a minute."
                } else {
                    "It ends itself if this lasts a few minutes."
                }
            );
            Instant::now()
        });
        if notify.is_none() && since.elapsed() >= GIVE_UP {
            eprintln!(
                "Tessera: the server is still stuck ({trouble}); ending it so it can be started again."
            );
            end_stuck_server();
        }
    }
}

/// Whether a 2-second nap took this long: the computer was asleep, or the
/// watchdog's own thread didn't get to run.
fn woke(since_last_look: Duration) -> bool {
    since_last_look >= Duration::from_secs(10)
}

/// A sleep gap must not count toward the three-minute restart timer.
/// Restart that timer too, not only health probing, when the PC wakes.
fn reset_after_wake(
    nap: Duration,
    probed: &mut Instant,
    misses: &mut u32,
    stuck_since: &mut Option<Instant>,
) -> bool {
    if !woke(nap) {
        return false;
    }
    *probed = Instant::now();
    *misses = 0;
    *stuck_since = None;
    true
}

/// End a server stuck for minutes, so whatever started it starts it again:
/// on Windows `tessera-service.exe`, which restarts it after any exit but a
/// clean one (code 0). There it ends at once with a code of its own
/// (`abort` may bring up an error report box that keeps the process alive
/// until someone clicks it, on a computer nobody watches); elsewhere it
/// aborts.
fn end_stuck_server() -> ! {
    #[cfg(windows)]
    tessera_core::platform::windows::end_now(STUCK_EXIT_CODE);
    #[cfg(not(windows))]
    std::process::abort()
}

/// The exit code of a server the watchdog ended on Windows (75, "try
/// again later"; `tessera-service.exe` says so in its log).
#[cfg(windows)]
const STUCK_EXIT_CODE: u32 = 75;

/// Where to ask: the listening address, or this computer when it listens on
/// every address.
fn probe_addr(listen: SocketAddr) -> SocketAddr {
    let ip = match listen.ip() {
        std::net::IpAddr::V4(v) if v.is_unspecified() => std::net::Ipv4Addr::LOCALHOST.into(),
        std::net::IpAddr::V6(v) if v.is_unspecified() => std::net::Ipv6Addr::LOCALHOST.into(),
        ip => ip,
    };
    SocketAddr::new(ip, listen.port())
}

/// The server answers `/api/health` with 200 in time.
fn answers(addr: SocketAddr) -> bool {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = s.set_write_timeout(Some(PROBE_TIMEOUT));
    let req = format!("GET /api/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 12];
    let mut n = 0;
    while n < head.len() {
        match s.read(&mut head[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200")
}

/// What a stuck server is doing: each thread's state and system call, the
/// files and sockets it waits on, and deadlocks among the server's locks.
fn report(trouble: &str) -> String {
    let mut out = format!(
        "Tessera {} (process {}): {trouble}.\n\n",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
    );
    out.push_str(&threads());
    out.push_str(&tessera_core::activity::report());
    out.push_str(&deadlocks());
    out
}

#[cfg(target_os = "linux")]
fn threads() -> String {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let mut out = String::from("Threads (name, state, waiting in, system call and arguments):\n");
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return out + "  (can't read /proc/self/task)\n\n";
    };
    let mut tids: Vec<String> = tasks
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    tids.sort_by_key(|t| t.parse::<u64>().unwrap_or(0));
    for tid in tids {
        let base = format!("/proc/self/task/{tid}");
        let name = read(&format!("{base}/comm")).trim().to_string();
        // The state is the field after the name, which is in parentheses.
        let stat = read(&format!("{base}/stat"));
        let state = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or("?")
            .to_string();
        let wchan = read(&format!("{base}/wchan"));
        let syscall = read(&format!("{base}/syscall"));
        let mut call = syscall.split_whitespace();
        let (nr, arg0) = (call.next().unwrap_or(""), call.next().unwrap_or(""));
        let named = nr.parse::<i64>().ok().and_then(syscall_name).unwrap_or(nr);
        let mut line = format!("  {tid:>7} {name:<24} {state} {wchan:<24} {named}");
        // What a read, write or wait on one file is waiting for.
        if matches!(
            named,
            "read" | "write" | "recvfrom" | "sendto" | "accept4" | "connect"
        ) && let Ok(fd) = i64::from_str_radix(arg0.trim_start_matches("0x"), 16)
            && let Ok(target) = std::fs::read_link(format!("/proc/self/fd/{fd}"))
        {
            line.push_str(&format!(" fd {fd} → {}", target.display()));
        } else if !arg0.is_empty() {
            line.push_str(&format!(" {arg0}"));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(not(target_os = "linux"))]
fn threads() -> String {
    "Threads: only listed on Linux.\n\n".into()
}

/// The names of the system calls a stuck thread is usually in.
#[cfg(target_os = "linux")]
fn syscall_name(nr: i64) -> Option<&'static str> {
    #[cfg(target_arch = "x86_64")]
    let name = match nr {
        0 => "read",
        1 => "write",
        7 => "poll",
        23 => "select",
        35 => "nanosleep",
        43 => "accept",
        42 => "connect",
        44 => "sendto",
        45 => "recvfrom",
        61 => "wait4",
        202 => "futex",
        230 => "clock_nanosleep",
        232 => "epoll_wait",
        270 => "pselect6",
        271 => "ppoll",
        281 => "epoll_pwait",
        288 => "accept4",
        _ => return None,
    };
    #[cfg(target_arch = "aarch64")]
    let name = match nr {
        63 => "read",
        64 => "write",
        22 => "epoll_pwait",
        72 => "pselect6",
        73 => "ppoll",
        98 => "futex",
        101 => "nanosleep",
        115 => "clock_nanosleep",
        202 => "accept",
        203 => "connect",
        206 => "sendto",
        207 => "recvfrom",
        242 => "accept4",
        260 => "wait4",
        _ => return None,
    };
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let name = {
        let _ = nr;
        return None;
    };
    Some(name)
}

/// Threads waiting on each other's locks (the server's locks are
/// parking_lot ones, built with deadlock detection), with where each waits.
fn deadlocks() -> String {
    let cycles = parking_lot::deadlock::check_deadlock();
    if cycles.is_empty() {
        return "Deadlocks among the server's locks: none newly detected. This does not rule out an already reported cycle, other locks, or worker starvation; see worker operations and thread waits above.\n".into();
    }
    let mut out = format!("Deadlocks among the server's locks: {}\n", cycles.len());
    for (i, cycle) in cycles.iter().enumerate() {
        out.push_str(&format!("\nDeadlock {}:\n", i + 1));
        for t in cycle {
            out.push_str(&format!(
                "  thread {:?}:\n{:?}\n",
                t.thread_id(),
                t.backtrace()
            ));
        }
    }
    out
}

/// A stall report written since the last start (the newest), told once:
/// the title and text of a notification for the user's devices.
pub fn previous_stall(data_dir: &Path) -> Option<(String, String)> {
    let dir = data_dir.join("stalls");
    let newest = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("stall-") && n.ends_with(".txt"))
        .max()?;
    let seen = dir.join("told");
    if std::fs::read_to_string(&seen).is_ok_and(|s| s.trim() == newest) {
        return None;
    }
    std::fs::write(&seen, &newest).ok()?;
    let when = newest
        .trim_start_matches("stall-")
        .trim_end_matches(".txt")
        .parse::<i64>()
        .ok()
        .map(|t| {
            let (h, m) = ((t / 3600) % 24, (t / 60) % 60);
            format!(" at {h:02}:{m:02} UTC")
        })
        .unwrap_or_default();
    Some((
        "Tessera got stuck and restarted itself".into(),
        format!(
            "Your server stopped answering{when} and was started again. A report of where it was stuck is in {} ({newest}): ask an agent to read it, or send it with a bug report.",
            dir.display()
        ),
    ))
}

/// Keep the report in `dir` (the last few).
fn save(dir: &Path, text: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let file = dir.join(format!("stall-{secs}.txt"));
    std::fs::write(&file, text).ok()?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut old: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("stall-"))
            })
            .collect();
        old.sort();
        let extra = old.len().saturating_sub(KEEP);
        for p in old.into_iter().take(extra) {
            let _ = std::fs::remove_file(p);
        }
    }
    Some(file)
}

/// systemd's notification socket (`NOTIFY_SOCKET`), when systemd runs the
/// server with `NotifyAccess=main`.
struct Notify {
    #[cfg(target_os = "linux")]
    sock: std::os::unix::net::UnixDatagram,
    #[cfg(target_os = "linux")]
    addr: std::os::unix::net::SocketAddr,
}

impl Notify {
    #[cfg(target_os = "linux")]
    fn from_env() -> Option<Self> {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        let path = std::env::var_os("NOTIFY_SOCKET")?;
        let path = path.to_string_lossy();
        let addr = match path.strip_prefix('@') {
            Some(name) => SocketAddr::from_abstract_name(name.as_bytes()).ok()?,
            None => SocketAddr::from_pathname(path.as_ref()).ok()?,
        };
        let sock = UnixDatagram::unbound().ok()?;
        Some(Self { sock, addr })
    }

    #[cfg(not(target_os = "linux"))]
    fn from_env() -> Option<Self> {
        None
    }

    fn send(&self, msg: &str) {
        #[cfg(target_os = "linux")]
        {
            let _ = self.sock.send_to_addr(msg.as_bytes(), &self.addr);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = msg;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: a deadlock is reported only once, to whoever asks first.
    #[test]
    fn a_report_lists_the_threads_and_the_deadlocked_locks() {
        // A synchronous wait without a parking_lot cycle must still
        // identify its operation. Keep all detector calls in this one
        // test: another test could consume the one-shot cycle report.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _operation = tessera_core::activity::enter(&tessera_core::activity::SAVE);
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let text = report("synthetic waiter");
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(text.contains("active=session:save"), "{text}");
        assert!(text.contains("none newly detected"), "{text}");

        let a = Arc::new(parking_lot::Mutex::new(()));
        let b = Arc::new(parking_lot::Mutex::new(()));
        let both = Arc::new(std::sync::Barrier::new(2));
        for (first, second) in [(a.clone(), b.clone()), (b, a)] {
            let both = both.clone();
            std::thread::spawn(move || {
                let _one = first.lock();
                both.wait();
                // The other thread holds it and waits for this one's: for ever.
                let _two = second.lock();
            });
        }
        std::thread::sleep(Duration::from_millis(500));
        let r = report("no task ran for 25 s");
        assert!(r.contains("no task ran for 25 s"), "{r}");
        #[cfg(target_os = "linux")]
        assert!(r.contains("Threads (name") && r.contains("futex"), "{r}");
        assert!(r.contains("Deadlocks among the server's locks: 1"), "{r}");
        assert!(r.contains("Deadlock 1:"), "{r}");
        assert!(r.contains("Worker operations"), "{r}");
        // parking_lot reports each cycle once, not every time it is asked.
        let second = report("same stall");
        assert!(second.contains("none newly detected"), "{second}");
        assert!(second.contains("does not rule out"), "{second}");
    }

    #[test]
    fn the_health_probe_wants_a_200_in_time() {
        use std::io::{Read, Write};
        let serve = |reply: &'static [u8]| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((mut c, _)) = l.accept() {
                    let mut req = [0u8; 256];
                    let n = c.read(&mut req).unwrap_or(0);
                    assert!(String::from_utf8_lossy(&req[..n]).starts_with("GET /api/health "));
                    let _ = c.write_all(reply);
                }
            });
            addr
        };
        assert!(answers(serve(
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok"
        )));
        assert!(!answers(serve(b"HTTP/1.1 503 Service Unavailable\r\n\r\n")));
        // Nobody listening there.
        let gone = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(!answers(gone));
        assert_eq!(
            probe_addr("0.0.0.0:7878".parse().unwrap()),
            "127.0.0.1:7878".parse().unwrap()
        );
        assert_eq!(
            probe_addr("192.168.1.5:7878".parse().unwrap()),
            "192.168.1.5:7878".parse().unwrap()
        );
    }

    #[test]
    fn a_new_stall_report_is_told_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(previous_stall(dir.path()).is_none());
        let stalls = dir.path().join("stalls");
        std::fs::create_dir_all(&stalls).unwrap();
        std::fs::write(stalls.join("stall-1791335166.txt"), "report").unwrap();
        let (title, body) = previous_stall(dir.path()).unwrap();
        assert!(title.contains("restarted itself"), "{title}");
        assert!(
            body.contains("at 01:06 UTC") && body.contains("stall-1791335166.txt"),
            "{body}"
        );
        assert!(previous_stall(dir.path()).is_none(), "told once");
        std::fs::write(stalls.join("stall-1791339999.txt"), "report").unwrap();
        assert!(previous_stall(dir.path()).is_some(), "a newer one is told");
    }

    #[test]
    fn a_computer_that_slept_isnt_a_stuck_server() {
        assert!(!woke(Duration::from_secs(2)));
        assert!(!woke(Duration::from_millis(9_900)));
        assert!(woke(Duration::from_secs(10)));
        assert!(woke(Duration::from_secs(3_600)), "an hour asleep");
    }

    #[test]
    fn sleep_does_not_age_an_existing_restart_timer() {
        let old = Instant::now() - Duration::from_secs(3600);
        let mut probed = old;
        let mut misses = 4;
        let mut stuck = Some(old);
        assert!(!reset_after_wake(
            Duration::from_secs(2),
            &mut probed,
            &mut misses,
            &mut stuck
        ));
        assert_eq!((probed, misses, stuck), (old, 4, Some(old)));
        assert!(reset_after_wake(
            Duration::from_secs(3600),
            &mut probed,
            &mut misses,
            &mut stuck
        ));
        assert_eq!(misses, 0);
        assert_eq!(stuck, None);
        assert!(probed > old);
    }

    #[test]
    fn old_reports_are_cleared() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..8 {
            std::fs::write(dir.path().join(format!("stall-{i}.txt")), "x").unwrap();
        }
        save(dir.path(), "report").unwrap();
        let n = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(n, KEEP);
    }
}
