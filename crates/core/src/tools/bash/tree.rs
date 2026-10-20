//! Linux best-effort job cleanup through retained process handles.
//! Numeric pid/start-tick tuples never authorize the handle-backed path.

use std::collections::HashMap;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use super::Stopped;

#[cfg(test)]
#[derive(Default)]
struct ProcReads {
    scans: usize,
    stats: HashMap<i32, usize>,
    markers: HashMap<i32, usize>,
}
#[cfg(test)]
std::thread_local! {
    static PROC_READS: std::cell::RefCell<ProcReads> = std::cell::RefCell::default();
}
struct Proc {
    ppid: i32,
    start: u64,
}

/// Only ENOENT/ESRCH mean disappearance. Permission, fd exhaustion and malformed
/// proc data are uncertainty, never evidence that a process was checked/gone.
fn proc_read<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => Ok(None),
        Err(error) => Err(error),
    }
}
fn stat(pid: i32) -> io::Result<Option<Proc>> {
    #[cfg(test)]
    PROC_READS.with_borrow_mut(|reads| *reads.stats.entry(pid).or_default() += 1);
    // Normal /proc stats fit in one page. Read them without metadata or
    // growable buffers, retaining the full-read path for long/partial records.
    let mut buffer = [0; 4096];
    let read = std::fs::File::open(format!("/proc/{pid}/stat")).and_then(|mut file| {
        loop {
            match file.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result,
            }
        }
    });
    let Some(len) = proc_read(read)? else {
        return Ok(None);
    };
    let text = if len < buffer.len() && buffer[..len].ends_with(b"\n") {
        std::borrow::Cow::Borrowed(
            std::str::from_utf8(&buffer[..len])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?,
        )
    } else {
        let Some(text) = proc_read(std::fs::read_to_string(format!("/proc/{pid}/stat")))? else {
            return Ok(None);
        };
        std::borrow::Cow::Owned(text)
    };
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid process stat");
    let end = text.rfind(')').ok_or_else(invalid)?;
    let mut fields = text[end + 1..].split_whitespace();
    let state = fields.next().ok_or_else(invalid)?;
    if matches!(state, "Z" | "X" | "x") {
        return Ok(None); // exited, but tests check /proc disappearance separately
    }
    Ok(Some(Proc {
        ppid: fields
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?,
        start: fields
            .nth(17)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?,
    }))
}
fn marker_read(
    result: io::Result<Vec<u8>>,
    verify_exited: impl FnOnce() -> io::Result<Option<Proc>>,
) -> io::Result<Option<Vec<u8>>> {
    match proc_read(result) {
        // Zombie /proc entries can deny environ after their pidfd reports
        // exit. Resolve that denial only with an independently verified exit;
        // a live process or unreadable stat still means unknown cleanup.
        Err(error) => {
            if matches!(verify_exited(), Ok(None)) {
                Ok(None)
            } else {
                Err(error)
            }
        }
        other => other,
    }
}
fn carries(pid: i32, marker: &str) -> io::Result<Option<bool>> {
    #[cfg(test)]
    PROC_READS.with_borrow_mut(|reads| *reads.markers.entry(pid).or_default() += 1);
    marker_read(std::fs::read(format!("/proc/{pid}/environ")), || stat(pid))
        .map(|env| env.map(|env| env.split(|b| *b == 0).any(|kv| kv == marker.as_bytes())))
}

fn processes() -> io::Result<Vec<i32>> {
    #[cfg(test)]
    PROC_READS.with_borrow_mut(|reads| reads.scans += 1);
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        if let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            found.push(pid);
        }
    }
    Ok(found)
}

const PER_TREE_HANDLES: usize = 64;
static HELD_HANDLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct Pidfd(OwnedFd);
impl AsRawFd for Pidfd {
    fn as_raw_fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
}
impl Drop for Pidfd {
    fn drop(&mut self) {
        HELD_HANDLES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}
fn global_handle_limit() -> usize {
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return 32;
    }
    (limit.rlim_cur / 8).clamp(4, 256) as usize
}
fn open_pidfd(pid: i32) -> io::Result<Pidfd> {
    use std::sync::atomic::Ordering::Relaxed;
    HELD_HANDLES
        .fetch_update(Relaxed, Relaxed, |count| {
            (count < global_handle_limit()).then_some(count + 1)
        })
        .map_err(|_| io::Error::from_raw_os_error(libc::EMFILE))?;
    // SAFETY: scalar arguments; a successful syscall returns a new owned fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        HELD_HANDLES.fetch_sub(1, Relaxed);
        return Err(io::Error::last_os_error());
    }
    Ok(Pidfd(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
}

fn send_pidfd(fd: &Pidfd, signal: i32) -> io::Result<()> {
    // SAFETY: owned process handle, scalar signal, null siginfo, no flags.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn fd_alive(fd: &Pidfd) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut p, 1, 0) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else if p.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        Err(io::Error::other("process handle could not be checked"))
    } else {
        Ok(result == 0)
    }
}

#[cfg(test)]
pub(super) fn real_capability_for_test() -> bool {
    match open_pidfd(std::process::id() as i32).and_then(|fd| send_pidfd(&fd, 0)) {
        Ok(()) => true,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::EPERM | libc::EACCES | libc::EINVAL)
            ) =>
        {
            eprintln!("SKIP real-pidfd lifecycle case: capability unavailable: {error}");
            false
        }
        Err(error) => panic!("unexpected pidfd probe failure: {error}"),
    }
}
/// Production fallback classification applies ONLY to pidfd_open errors.
fn open_fallback(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM))
}

enum Handle {
    Pidfd(Pidfd),
    /// Explicitly disclosed best-effort compatibility path, not pidfd authority.
    Numeric,
}

struct Held<H> {
    pid: i32,
    handle: H,
    marker_only: bool,
    // Diagnostic only. Never compared to authorize an edge or a signal.
    _start_tick: Option<u64>,
}

/// Narrow injected seam for adversarial interleavings. A candidate list is
/// untrusted until its child handle and still-live parent authenticate the edge.
trait Discovery {
    type Handle;
    fn live(&mut self, process: &Held<Self::Handle>) -> bool;
    fn children(&mut self, parent: i32) -> Vec<i32>;
    fn open(&mut self, pid: i32) -> io::Result<Self::Handle>;
    fn parent(&mut self, pid: i32) -> Option<i32>;
    fn start_tick(&mut self, pid: i32) -> Option<u64>;
    fn authority(&mut self, process: &Held<Self::Handle>) -> bool {
        self.live(process)
    }
    fn incomplete(&mut self) {}
}

fn discover<D: Discovery>(held: &mut Vec<Held<D::Handle>>, source: &mut D) {
    let mut next = 0;
    // Top-down, including children accepted this round. Every retained live
    // member seeds the next round, even if the original leader has died.
    while next < held.len() {
        if source.authority(&held[next]) {
            let parent = held[next].pid;
            for pid in source.children(parent) {
                if pid <= 1 || held.iter().any(|p| p.pid == pid && source.live(p)) {
                    continue;
                }
                if held.len() >= PER_TREE_HANDLES {
                    source.incomplete();
                    continue;
                }
                let handle = match source.open(pid) {
                    Ok(handle) => handle,
                    Err(error) => {
                        if error.raw_os_error() != Some(libc::ESRCH) {
                            source.incomplete();
                        }
                        continue;
                    }
                };
                let child = Held {
                    pid,
                    handle,
                    marker_only: false,
                    _start_tick: source.start_tick(pid),
                };
                // Open FIRST; re-read ppid, then test the held parent's handle.
                // A dead/recycled parent's numeric pid never grants authority.
                if source.parent(pid) == Some(parent)
                    && source.authority(&held[next])
                    && source.live(&child)
                {
                    held.push(child);
                }
            }
        }
        next += 1;
    }
}

struct Linux<'a> {
    marker: Option<&'a str>,
    numeric_fallback: bool,
    incomplete: bool,
    // Candidate data only. Authorization still uses fresh post-open reads.
    scan: Option<HashMap<i32, Option<Proc>>>,
}
impl<'a> Linux<'a> {
    fn new(marker: Option<&'a str>) -> Self {
        Self {
            marker,
            numeric_fallback: false,
            incomplete: false,
            scan: None,
        }
    }
    fn scan(&mut self) -> &HashMap<i32, Option<Proc>> {
        if self.scan.is_none() {
            let pids = self.checked(processes().map(Some)).unwrap_or_default();
            let mut scan = HashMap::with_capacity(pids.len());
            for pid in pids {
                scan.insert(pid, self.checked(stat(pid)));
            }
            self.scan = Some(scan);
        }
        self.scan.as_ref().unwrap()
    }
    fn checked<T>(&mut self, result: io::Result<Option<T>>) -> Option<T> {
        match result {
            Ok(value) => value,
            Err(_) => {
                self.incomplete = true;
                None
            }
        }
    }
    fn marked(&mut self, pid: i32) -> bool {
        self.marker
            .is_some_and(|marker| self.checked(carries(pid, marker)) == Some(true))
    }
}
impl Discovery for Linux<'_> {
    type Handle = Handle;

    fn live(&mut self, process: &Held<Handle>) -> bool {
        let alive = match &process.handle {
            Handle::Pidfd(fd) => self.checked(fd_alive(fd).map(Some)) == Some(true),
            Handle::Numeric => {
                self.numeric_fallback = true;
                self.incomplete = true;
                self.checked(stat(process.pid))
                    .is_some_and(|p| Some(p.start) == process._start_tick)
                    && self.marked(process.pid)
            }
        };
        alive && (!process.marker_only || self.marked(process.pid))
    }

    fn authority(&mut self, process: &Held<Handle>) -> bool {
        matches!(process.handle, Handle::Pidfd(_)) && self.live(process)
    }
    fn incomplete(&mut self) {
        self.incomplete = true;
    }
    fn children(&mut self, parent: i32) -> Vec<i32> {
        let mut children = Vec::new();
        match proc_read(std::fs::read_dir(format!("/proc/{parent}/task"))) {
            Ok(Some(tasks)) => {
                for task in tasks {
                    match task {
                        Ok(task) => {
                            if let Some(text) = self.checked(proc_read(std::fs::read_to_string(
                                task.path().join("children"),
                            ))) {
                                for field in text.split_whitespace() {
                                    match field.parse::<i32>() {
                                        Ok(pid) => children.push(pid),
                                        Err(_) => self.incomplete = true,
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            self.checked::<()>(proc_read(Err(error)));
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(_) => self.incomplete = true,
        }
        // Scan errors are uncertainty. These are candidates only; every child
        // still needs a held pidfd plus post-open ppid/live-parent validation.
        for (&pid, process) in self.scan() {
            if process.as_ref().is_some_and(|p| p.ppid == parent) {
                children.push(pid);
            }
        }
        children.sort_unstable();
        children.dedup();
        children
    }

    fn open(&mut self, pid: i32) -> io::Result<Handle> {
        match open_pidfd(pid) {
            Ok(fd) => Ok(Handle::Pidfd(fd)),
            Err(error) if open_fallback(&error) => {
                self.numeric_fallback = true;
                Ok(Handle::Numeric)
            }
            Err(error) => {
                if error.raw_os_error() != Some(libc::ESRCH) {
                    self.incomplete = true;
                }
                Err(error)
            }
        }
    }

    fn parent(&mut self, pid: i32) -> Option<i32> {
        self.checked(stat(pid)).map(|p| p.ppid)
    }
    fn start_tick(&mut self, pid: i32) -> Option<u64> {
        self.checked(stat(pid)).map(|p| p.start)
    }
}

pub(super) struct Tree {
    held: Vec<Held<Handle>>,
    marker: Option<String>,
    numeric_fallback: bool,
    incomplete: bool,
}

impl Tree {
    pub(super) fn with_marker(mut self, marker: String) -> Self {
        self.marker = Some(marker);
        self
    }
    /// Rediscover from retained live handles; completed histories drop dead fds.
    pub(super) fn observe(&mut self) -> bool {
        self.discover();
        !self.held.is_empty()
    }
    fn release_dead(&mut self) {
        self.held.retain(|p| match &p.handle {
            Handle::Pidfd(fd) => match fd_alive(fd) {
                Ok(alive) => alive,
                Err(_) => {
                    self.incomplete = true;
                    true
                }
            },
            Handle::Numeric => match stat(p.pid) {
                Ok(Some(stat)) => Some(stat.start) == p._start_tick,
                Ok(None) => false,
                Err(_) => {
                    self.incomplete = true;
                    true
                }
            },
        });
    }
    fn discover(&mut self) {
        self.release_dead();
        let mut source = Linux::new(self.marker.as_deref());
        discover(&mut self.held, &mut source);
        if self.marker.is_some() {
            let pids: Vec<_> = source.scan().keys().copied().collect();
            for pid in pids {
                if pid <= 1
                    || pid == std::process::id() as i32
                    || self.held.iter().any(|p| p.pid == pid && source.live(p))
                    || !source.marked(pid)
                {
                    continue;
                }
                if self.held.len() >= PER_TREE_HANDLES {
                    source.incomplete = true;
                    continue;
                }
                let Ok(handle) = source.open(pid) else {
                    continue;
                };
                let process = Held {
                    pid,
                    handle,
                    marker_only: true,
                    _start_tick: source.start_tick(pid),
                };
                // Marker-only leftovers must still carry the marker after open.
                if source.live(&process) {
                    self.held.push(process);
                }
            }
        }
        // Marker roots accepted above may also own descendants. The same
        // handle-backed edge validation applies to them on the next discovery.
        self.numeric_fallback |= source.numeric_fallback;
        self.incomplete |= source.incomplete || self.numeric_fallback;
    }

    fn signal(&mut self, from: usize, signal: i32) {
        let mut source = Linux::new(self.marker.as_deref());
        for process in &self.held[from..] {
            if !source.live(process) {
                continue;
            }
            match &process.handle {
                Handle::Pidfd(fd) => {
                    // ALL send errors are terminal for this attempt. No numeric retry.
                    source.checked(proc_read(send_pidfd(fd, signal)));
                }
                Handle::Numeric => {
                    // Compatibility path only; start+marker rechecked above on
                    // EVERY attempt. It never seeds ancestry and isn't race-free.
                    let result = unsafe { libc::kill(process.pid, signal) };
                    if result != 0 {
                        source.checked::<()>(proc_read(Err(io::Error::last_os_error())));
                    }
                }
            }
        }
        self.incomplete |= source.incomplete;
        self.numeric_fallback |= source.numeric_fallback;
    }

    fn left(&mut self) -> Vec<i32> {
        let mut source = Linux::new(self.marker.as_deref());
        let left = self
            .held
            .iter()
            .filter(|p| source.live(p))
            .map(|p| p.pid)
            .collect();
        self.incomplete |= source.incomplete;
        self.numeric_fallback |= source.numeric_fallback;
        left
    }

    pub(super) fn stop(&mut self, grace: Duration) -> Stopped {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            self.discover();
            let before = seen.len();
            seen.extend(self.held.iter().map(|p| p.pid));
            self.signal(0, libc::SIGSTOP);
            if before == seen.len() {
                break;
            }
        }
        self.signal(0, libc::SIGTERM);
        self.signal(0, libc::SIGCONT);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            self.discover();
            seen.extend(self.held.iter().map(|p| p.pid));
            self.signal(0, libc::SIGTERM);
            self.signal(0, libc::SIGCONT);
            if self.left().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for _ in 0..4 {
            self.discover();
            seen.extend(self.held.iter().map(|p| p.pid));
            if self.left().is_empty() {
                break;
            }
            self.signal(0, libc::SIGKILL);
            std::thread::sleep(Duration::from_millis(150));
        }
        self.discover();
        if self.numeric_fallback {
            tracing::warn!("{}", super::CLEANUP_FALLBACK);
        }
        let mut left = self.left();
        if self.incomplete {
            left.push(-1);
        } // unknown remainder, never empty
        Stopped {
            signalled: seen.len(),
            left,
            numeric_fallback: self.numeric_fallback,
            incomplete: self.incomplete,
            ..Default::default()
        }
    }
}

/// Pin an external direct child before its caller can wait/reap it. Kept generic
/// so replacement after reaping is deterministic in the ownership regression.
fn capture_external<D: Discovery>(me: i32, pid: i32, source: &mut D) -> Option<Held<D::Handle>> {
    let parent = Held {
        pid: me,
        handle: source.open(me).ok()?,
        marker_only: false,
        _start_tick: None,
    };
    let child = Held {
        pid,
        handle: source.open(pid).ok()?,
        marker_only: false,
        _start_tick: source.start_tick(pid),
    };
    (source.parent(pid) == Some(me) && source.live(&parent) && source.live(&child)).then_some(child)
}

/// External callers must still hold an unreaped Child. Acquire the child
/// handle synchronously, before the caller can resume and reap it.
/// Authenticate the edge against our held live server handle, not a tick.
pub(super) fn external_tree(pid: i32) -> Option<Tree> {
    let me = std::process::id() as i32;
    let mut source = Linux::new(None);
    let child = capture_external(me, pid, &mut source)?;
    let tree = Tree {
        held: vec![child],
        marker: None,
        numeric_fallback: source.numeric_fallback,
        incomplete: source.incomplete,
    };
    Some(tree)
}

pub(super) fn stop_marker(marker: String) -> Stopped {
    let mut tree = Tree {
        held: Vec::new(),
        marker: Some(marker),
        numeric_fallback: false,
        incomplete: false,
    };
    tree.stop(Duration::from_millis(1500))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    #[test]
    fn observe_scans_proc_once_and_skips_held_marker_reads() {
        if !super::super::tests::cleanup_fixture(
            "tools::bash::tree::tests::observe_scans_proc_once_and_skips_held_marker_reads",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let marker = format!("b28-scan-{}", std::process::id());
        let mut leader = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "for i in 1 2 3 4 5 6 7 8; do sleep 3050 & echo $! >> '{}'; done; wait",
                ready.display()
            ))
            .env("TESSERA_JOB", &marker)
            .spawn()
            .unwrap();
        let mut tree = external_tree(leader.id() as i32).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let descendants = loop {
            let pids: Vec<i32> = std::fs::read_to_string(&ready)
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if pids.len() == 8 {
                break pids;
            }
            assert!(Instant::now() < deadline, "fixture children did not start");
            std::thread::sleep(Duration::from_millis(5));
        };
        tree.observe();
        assert_eq!(tree.held.len(), 9);
        PROC_READS.with_borrow_mut(|reads| *reads = ProcReads::default());
        tree.observe();
        let reads = PROC_READS.with_borrow_mut(std::mem::take);
        assert_eq!(reads.scans, 1, "observe scanned /proc once per parent");
        assert!(reads.stats.values().all(|&count| count == 1));
        assert!(reads.markers.is_empty());

        tree = tree.with_marker(format!("TESSERA_JOB={marker}"));
        PROC_READS.with_borrow_mut(|reads| *reads = ProcReads::default());
        tree.observe();
        let reads = PROC_READS.with_borrow_mut(std::mem::take);
        assert_eq!(reads.scans, 1, "marker discovery rescanned /proc");
        assert!(reads.markers.values().all(|&count| count == 1));
        assert!(
            tree.held
                .iter()
                .all(|p| !reads.markers.contains_key(&p.pid)),
            "retained ancestry handles caused redundant marker candidate reads"
        );
        let stopped = tree.stop(Duration::from_millis(50));
        leader.wait().unwrap();
        super::super::tests::reaped_disappearance(&descendants);
        assert!(stopped.left.is_empty(), "{stopped:?}");
    }

    #[test]
    fn cached_proc_candidate_does_not_replace_fresh_post_open_parent_check() {
        if !real_capability_for_test() {
            return;
        }
        let mut leader = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut unrelated = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut tree = external_tree(leader.id() as i32).unwrap();
        let mut source = Linux::new(None);
        source.scan = Some(HashMap::from([(
            unrelated.id() as i32,
            Some(Proc {
                ppid: leader.id() as i32,
                start: 0,
            }),
        )]));
        discover(&mut tree.held, &mut source);
        let held = tree.held.len();
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        let stopped = tree.stop(Duration::from_millis(50));
        leader.wait().unwrap();
        assert!(stopped.left.is_empty(), "{stopped:?}");
        assert_eq!(
            held, 1,
            "cached ppid granted ownership of an unrelated process"
        );
    }
    #[test]
    fn marker_denial_requires_verified_exit_to_resolve_uncertainty() {
        let denied = || Err(io::Error::from_raw_os_error(libc::EACCES));
        assert!(marker_read(denied(), || Ok(None)).unwrap().is_none());
        assert_eq!(
            marker_read(denied(), || Ok(Some(Proc { ppid: 1, start: 2 })))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        assert_eq!(
            marker_read(denied(), || Err(io::Error::from_raw_os_error(libc::EIO)))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
    }

    #[derive(Clone)]
    struct FakeHandle(u64);
    struct Fake {
        live: HashSet<u64>,
        generation: HashMap<i32, u64>,
        ppid: HashMap<i32, i32>,
        edges: HashMap<i32, Vec<i32>>,
        replace_on_open: Option<(i32, i32, u64)>,
        dead_parent_on_open: Option<u64>,
        open_error: Option<i32>,
        incomplete: bool,
    }
    impl Discovery for Fake {
        type Handle = FakeHandle;
        fn live(&mut self, p: &Held<FakeHandle>) -> bool {
            self.live.contains(&p.handle.0)
        }
        fn children(&mut self, pid: i32) -> Vec<i32> {
            self.edges.get(&pid).cloned().unwrap_or_default()
        }
        fn incomplete(&mut self) {
            self.incomplete = true;
        }
        fn open(&mut self, pid: i32) -> io::Result<FakeHandle> {
            if let Some(error) = self.open_error {
                return Err(io::Error::from_raw_os_error(error));
            }
            if let Some(dead) = self.dead_parent_on_open.take() {
                self.live.remove(&dead);
            }
            if let Some((target, parent, generation)) = self.replace_on_open.take() {
                assert_eq!(target, pid);
                self.ppid.insert(pid, parent);
                self.generation.insert(pid, generation);
                self.live.insert(generation);
            }
            Ok(FakeHandle(self.generation[&pid]))
        }
        fn parent(&mut self, pid: i32) -> Option<i32> {
            self.ppid.get(&pid).copied()
        }
        fn start_tick(&mut self, _: i32) -> Option<u64> {
            Some(100)
        }
    }
    fn fixture() -> (Vec<Held<FakeHandle>>, Fake) {
        (
            vec![Held {
                pid: 10,
                handle: FakeHandle(1),
                marker_only: false,
                _start_tick: Some(100),
            }],
            Fake {
                live: HashSet::from([1, 2]),
                generation: HashMap::from([(10, 1), (20, 2)]),
                ppid: HashMap::from([(20, 10)]),
                edges: HashMap::from([(10, vec![20])]),
                replace_on_open: None,
                dead_parent_on_open: None,
                open_error: None,
                incomplete: false,
            },
        )
    }

    #[test]
    fn typed_proc_errors_are_not_misclassified_as_disappearance() {
        for error in [
            libc::EMFILE,
            libc::ENFILE,
            libc::EACCES,
            libc::EPERM,
            libc::EIO,
        ] {
            let mut source = Linux::new(None);
            let checked =
                source.checked::<Proc>(proc_read(Err(io::Error::from_raw_os_error(error))));
            assert!(checked.is_none());
            assert!(
                source.incomplete,
                "post-open stat/environ error {error} became disappearance"
            );
        }
        let mut source = Linux::new(None);
        source.checked::<Proc>(Err(io::Error::new(io::ErrorKind::InvalidData, "bad stat")));
        assert!(source.incomplete);
        for error in [libc::ENOENT, libc::ESRCH] {
            let mut source = Linux::new(None);
            assert!(
                source
                    .checked::<Proc>(proc_read(Err(io::Error::from_raw_os_error(error))))
                    .is_none()
            );
            assert!(
                !source.incomplete,
                "disappearance {error} became a coverage failure"
            );
        }
    }
    #[test]
    fn discovery_rejects_child_of_recycled_parent_after_parent_handle_dies() {
        let (mut held, mut fake) = fixture();
        // Ppid still equals 10, but the real held parent dies during open;
        // numeric parent 10 now represents an unrelated forker. Same tick too.
        fake.dead_parent_on_open = Some(1);
        fake.replace_on_open = Some((20, 10, 3));
        discover(&mut held, &mut fake);
        assert_eq!(
            held.len(),
            1,
            "a dead parent's pid authorized an unrelated child"
        );
    }

    #[test]
    fn discovery_rejects_replacement_before_open_even_with_same_start_tick() {
        let (mut held, mut fake) = fixture();
        fake.replace_on_open = Some((20, 99, 3));
        discover(&mut held, &mut fake);
        assert_eq!(
            held.len(),
            1,
            "tick-resolution tuple authorized a replacement"
        );
        // A replacement really forked by our held live parent is ours anyway.
        fake.ppid.insert(20, 10);
        discover(&mut held, &mut fake);
        assert_eq!(held.len(), 2);
        assert_eq!(held[1].handle.0, 3);
    }

    #[test]
    fn discovery_repeats_from_retained_child_after_leader_exits() {
        let (mut held, mut fake) = fixture();
        discover(&mut held, &mut fake);
        fake.live.remove(&1);
        fake.generation.insert(30, 3);
        fake.live.insert(3);
        fake.ppid.insert(30, 20);
        fake.edges.insert(20, vec![30]);
        discover(&mut held, &mut fake);
        assert_eq!(held.iter().map(|p| p.pid).collect::<Vec<_>>(), [10, 20, 30]);
    }

    #[test]
    fn external_pending_output_never_reopens_a_reaped_leader_replacement() {
        let (_, mut fake) = fixture();
        // Server 10 captures its unreaped direct child 20 (handle generation 2)
        // before output/wait may suspend. Child 30 keeps the output pipe open.
        let leader = capture_external(10, 20, &mut fake).unwrap();
        let mut held = vec![leader];
        fake.generation.insert(30, 3);
        fake.live.insert(3);
        fake.ppid.insert(30, 20);
        fake.edges.insert(20, vec![30]);
        discover(&mut held, &mut fake);
        assert_eq!(held.iter().map(|p| p.handle.0).collect::<Vec<_>>(), [2, 3]);

        // wait has reaped leader 20 while output is still pending. Its numeric
        // pid now belongs to another live direct child of this same server.
        fake.live.remove(&2);
        fake.generation.insert(20, 4);
        fake.live.insert(4);
        fake.generation.insert(40, 5);
        fake.live.insert(5);
        fake.ppid.insert(40, 20);
        fake.edges.insert(20, vec![40]);
        // Our retained pipe holder can still fork a late descendant safely.
        fake.generation.insert(50, 6);
        fake.live.insert(6);
        fake.ppid.insert(50, 30);
        fake.edges.insert(30, vec![50]);
        discover(&mut held, &mut fake);
        assert_eq!(
            held.iter().map(|p| p.handle.0).collect::<Vec<_>>(),
            [2, 3, 6]
        );
        assert!(
            !held.iter().any(|p| p.handle.0 == 4 || p.pid == 40),
            "pending output replaced original ownership with a new direct child"
        );
    }
    #[test]
    fn only_enosys_and_open_eperm_allow_numeric_fallback() {
        for code in [libc::ENOSYS, libc::EPERM] {
            assert!(open_fallback(&io::Error::from_raw_os_error(code)));
        }
        for code in [
            libc::EINVAL,
            libc::EACCES,
            libc::ESRCH,
            libc::EBADF,
            libc::EMFILE,
        ] {
            assert!(
                !open_fallback(&io::Error::from_raw_os_error(code)),
                "errno {code}"
            );
        }
    }

    #[test]
    fn held_pidfd_does_not_signal_a_replacement_after_target_exit() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let result = open_pidfd(child.id() as i32).and_then(|fd| {
            send_pidfd(&fd, 0)?;
            Ok(fd)
        });
        let fd = match result {
            Ok(fd) => fd,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS | libc::EPERM | libc::EACCES | libc::EINVAL)
                ) =>
            {
                let _ = child.kill();
                let _ = super::super::tests::wait_child(&mut child);
                eprintln!("SKIP real-pidfd case: open/send capability unavailable: {error}");
                return;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = super::super::tests::wait_child(&mut child);
                panic!("unexpected pidfd error: {error}");
            }
        };
        child.kill().unwrap();
        super::super::tests::wait_child(&mut child);
        assert!(!fd_alive(&fd).unwrap());
        assert_eq!(
            send_pidfd(&fd, libc::SIGKILL).unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    #[test]
    fn exec_failure_subprocess_case() {
        let Ok(case) = std::env::var("TESSERA_EXEC_FAILURE_CASE") else {
            return;
        };
        let mut command = tokio::process::Command::new(if case == "missing" {
            "/no/tessera/such-shell"
        } else {
            "/bin/true"
        });
        if case == "preexec" {
            unsafe {
                command.pre_exec(|| Err(io::Error::from_raw_os_error(libc::EACCES)));
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let error = command
            .spawn()
            .expect_err("fixture launch unexpectedly succeeded");
        let expected = if case == "preexec" {
            libc::EACCES
        } else {
            libc::ENOENT
        };
        assert_eq!(
            error.raw_os_error(),
            Some(expected),
            "wrong launch-error origin for {case}: {error}"
        );
    }

    #[test]
    fn preexec_and_missing_shell_failures_return_within_subprocess_deadline() {
        for case in ["preexec", "missing"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tools::bash::tree::tests::exec_failure_subprocess_case",
                    "--nocapture",
                ])
                .env("TESSERA_EXEC_FAILURE_CASE", case)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            let exit = loop {
                if let Some(exit) = child.try_wait().unwrap() {
                    break exit;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = super::super::tests::wait_child(&mut child);
                    panic!("{case} launch deadlocked waiting for attachment");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(exit.success(), "{case}: {exit}");
        }
    }

    #[test]
    fn failed_open_discovery_reports_incomplete_not_empty_left() {
        let (mut held, mut fake) = fixture();
        fake.open_error = Some(libc::EMFILE);
        discover(&mut held, &mut fake);
        assert_eq!(held.len(), 1);
        assert!(fake.incomplete);
        let mut tree = Tree {
            held: Vec::new(),
            marker: None,
            numeric_fallback: false,
            incomplete: true,
        };
        let stopped = tree.stop(Duration::ZERO);
        assert_eq!(stopped.left, [-1]);
        assert!(stopped.incomplete);
        let text = super::super::killed_text("budget-test", false, &stopped);
        assert!(
            text.contains("some processes could not be checked") && !text.contains("pid -1"),
            "{text}"
        );
    }

    #[test]
    fn numeric_records_never_seed_discovery_and_recheck_start_marker_each_attempt() {
        let mut child = std::process::Command::new("sleep")
            .arg("3034")
            .env("TESSERA_JOB", "numeric-fixture")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let start = stat(pid).unwrap().unwrap().start;
        let mut source = Linux::new(Some("TESSERA_JOB=numeric-fixture"));
        source.numeric_fallback = true;
        let mut numeric = Held {
            pid,
            handle: Handle::Numeric,
            marker_only: false,
            _start_tick: Some(start),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while carries(pid, "TESSERA_JOB=numeric-fixture").unwrap() != Some(true)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(source.live(&numeric));
        assert!(!source.authority(&numeric));
        numeric._start_tick = Some(start + 1);
        assert!(!source.live(&numeric));
        let mut tree = Tree {
            held: vec![numeric],
            marker: Some("TESSERA_JOB=numeric-fixture".into()),
            numeric_fallback: true,
            incomplete: true,
        };
        tree.signal(0, libc::SIGKILL); // stale birth must not be signalled
        tree.held[0]._start_tick = Some(start);
        tree.marker = Some("TESSERA_JOB=wrong".into());
        tree.signal(0, libc::SIGKILL); // changed marker must not be signalled
        let alive = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = super::super::tests::wait_child(&mut child);
        assert!(alive);
    }

    #[test]
    fn descriptor_budget_subprocess_case() {
        if std::env::var_os("TESSERA_FD_BUDGET_CASE").is_none() {
            return;
        }
        let limit = libc::rlimit {
            rlim_cur: 64,
            rlim_max: 64,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        if !real_capability_for_test() {
            return;
        }
        for _ in 0..12 {
            let mut handles = Vec::new();
            loop {
                match open_pidfd(std::process::id() as i32) {
                    Ok(fd) => handles.push(fd),
                    Err(error) if open_fallback(&error) => {
                        eprintln!("SKIP real-pidfd budget: {error}");
                        return;
                    }
                    Err(error) => {
                        assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                        break;
                    }
                }
            }
            assert!(handles.len() <= 8);
            drop(handles);
            assert_eq!(HELD_HANDLES.load(std::sync::atomic::Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn low_rlimit_repeated_handle_budget_releases_descriptors() {
        super::super::tests::subprocess_check(
            "tools::bash::tree::tests::descriptor_budget_subprocess_case",
            &[("TESSERA_FD_BUDGET_CASE", "1")],
            Duration::from_secs(10),
        );
    }

    #[test]
    fn external_capture_retains_descendant_before_wait_reaps_leader() {
        if !super::super::tests::cleanup_fixture(
            "tools::bash::tree::tests::external_capture_retains_descendant_before_wait_reaps_leader",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let release = temp.path().join("release");
        let mut leader = std::process::Command::new("/bin/sh")
            .arg("-c").arg(format!("env -i /bin/sh -c 'trap \"\" TERM; exec /bin/sleep 3036' & echo $! > '{}'; while test ! -f '{}'; do sleep 0.01; done", ready.display(), release.display())).spawn().unwrap();
        let mut tree = external_tree(leader.id() as i32).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let descendant = loop {
            if let Ok(text) = std::fs::read_to_string(&ready)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "child did not publish a complete ready pid"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        tree.observe();
        assert!(tree.held.len() >= 2);
        std::fs::write(release, b"release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while leader.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "external leader did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        let stopped = tree.stop(Duration::from_millis(50));
        assert!(stopped.left.is_empty(), "{stopped:?}");
        super::super::tests::reaped_disappearance(&[leader.id() as i32, descendant]);
    }
}
