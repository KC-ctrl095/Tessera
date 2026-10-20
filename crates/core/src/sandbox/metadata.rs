//! Private, server-issued launch registration. Names and /proc cmdline are not
//! authority for auxiliary classification: retain the reported kernel pidfds.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

pub(super) const VAR: &str = "TESSERA_INTERNAL_SUPERVISION";
const MAGIC: &[u8] = b"T0-AUX-1";
struct Registration {
    reader: UnixDatagram,
    owner: Weak<()>,
    handles: Vec<OwnedFd>,
    controls: [OwnedFd; 2],
    cleanup_started: Option<Instant>,
}
fn registry() -> &'static Mutex<Vec<Registration>> {
    static REGISTRY: OnceLock<Mutex<Vec<Registration>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

pub(super) fn pidfd(pid: i32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
pub(super) fn signal(fd: &OwnedFd, signal: i32) {
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        );
    }
}
fn number(fd: &OwnedFd) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Pid:\t")?.parse().ok())
}
fn live(fd: &OwnedFd) -> bool {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut p, 1, 0) == 0 }
}
fn high(fd: i32) -> io::Result<OwnedFd> {
    let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 64) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(n) })
    }
}

// One server thread owns all launches through the LAST anchor's exit. It is
// outside the joined namespace and cannot be stopped by a sandbox peer.
fn supervise() -> io::Result<()> {
    static WORKER: OnceLock<Result<(), String>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            std::thread::Builder::new()
                .name("tessera-launch-owner".into())
                .spawn(|| {
                    loop {
                        std::thread::sleep(Duration::from_millis(20));
                        let mut all = registry().lock().unwrap();
                        all.iter_mut().for_each(|r| {
                            collect(r);
                            if r.handles.len() != 3 || live(&r.handles[0]) {
                                return;
                            }
                            let began = *r.cleanup_started.get_or_insert_with(Instant::now);
                            for (anchor, control) in r.handles[1..].iter().zip(&r.controls) {
                                if !live(anchor) {
                                    continue;
                                }
                                signal(anchor, libc::SIGCONT);
                                let cleanup = b'K';
                                unsafe {
                                    libc::send(
                                        control.as_raw_fd(),
                                        (&cleanup as *const u8).cast(),
                                        1,
                                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                                    );
                                }
                                // First let the continued joined anchor do kill(0), which
                                // cleans its owned group without authorizing a stale pgid.
                                if began.elapsed() >= Duration::from_secs(2) {
                                    signal(anchor, libc::SIGKILL);
                                }
                            }
                        });
                        prune(&mut all);
                    }
                })
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| io::Error::other(e.clone()))
        .copied()
}

/// Failure-only cleanup. The constructing child will never exec a program.
/// Let the kernel reap children that exit after SIGCHLD ignore. Already-exited
/// zombies get one nonblocking reap, never a poll/wait loop that would block
/// synchronous spawn on the server. Syscalls and stack data only after fork.
unsafe fn rollback(slots: &[i32], unpinned: Option<i32>) {
    unsafe {
        if let Some(pid) = unpinned {
            // Kill before SIGCHLD ignore can reap it and release its PID.
            libc::kill(pid, libc::SIGKILL);
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut());
        for fd in slots {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                *fd,
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
            let mut info: libc::siginfo_t = std::mem::zeroed();
            libc::waitid(
                libc::P_PIDFD,
                *fd as u32,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            );
        }
        if let Some(pid) = unpinned {
            libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
        }
    }
}
/// The fallback anchors are forked BEFORE nsenter enters user/PID namespaces.
/// They cannot be stopped or inspected by peers in the joined namespace. Only
/// private control channels and pidfd slots reach the outer native launcher.
pub(super) fn protect(
    cmd: &mut std::process::Command,
    stop_ready: Option<usize>,
    fail_after: Option<usize>,
    probe_fd: Option<i32>,
) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    supervise()?;
    let (reader, writer) = UnixDatagram::pair()?;
    reader.set_nonblocking(true)?;
    let writer = high(writer.as_raw_fd())?;
    let mut controls = Vec::new();
    let mut peers = Vec::new();
    let mut slots = Vec::new();
    for _ in 0..2 {
        let mut pair = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                pair.as_mut_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let a = unsafe { OwnedFd::from_raw_fd(pair[0]) };
        let b = unsafe { OwnedFd::from_raw_fd(pair[1]) };
        let enabled = 1i32;
        if unsafe {
            libc::setsockopt(
                b.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&enabled as *const i32).cast(),
                4,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        controls.push(high(a.as_raw_fd())?);
        peers.push(high(b.as_raw_fd())?);
        slots.push(high(std::fs::File::open("/dev/null")?.as_raw_fd())?);
    }
    cmd.env(
        VAR,
        format!(
            "{}:{}:{}:{}:{}",
            writer.as_raw_fd(),
            controls[0].as_raw_fd(),
            controls[1].as_raw_fd(),
            slots[0].as_raw_fd(),
            slots[1].as_raw_fd()
        ),
    );
    let owner = Arc::new(());
    let mut all = registry().lock().unwrap();
    prune(&mut all);
    all.push(Registration {
        reader,
        owner: Arc::downgrade(&owner),
        handles: Vec::new(),
        controls: [
            high(controls[0].as_raw_fd())?,
            high(controls[1].as_raw_fd())?,
        ],
        cleanup_started: None,
    });
    drop(all);
    // A private test-only identity stream is installed before fork. Do not use
    // names or a surviving /proc scan as evidence of construction cleanup.
    #[cfg(debug_assertions)]
    let probe = probe_fd.map(high).transpose()?;
    #[cfg(not(debug_assertions))]
    let _ = probe_fd;
    unsafe {
        cmd.pre_exec(move || {
            let _owner = &owner;
            let opened = libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) as i32;
            if opened < 0 {
                return Err(io::Error::last_os_error());
            }
            let parent = libc::fcntl(opened, libc::F_DUPFD_CLOEXEC, 16);
            libc::close(opened);
            if parent < 0 {
                return Err(io::Error::last_os_error());
            }
            let slots_raw = [slots[0].as_raw_fd(), slots[1].as_raw_fd()];
            for i in 0..2 {
                let pid = if fail_after == Some(i) {
                    -1
                } else {
                    libc::fork()
                };
                if pid < 0 {
                    let error = if fail_after == Some(i) {
                        io::Error::from_raw_os_error(libc::EAGAIN)
                    } else {
                        io::Error::last_os_error()
                    };
                    rollback(&slots_raw[..i], None);
                    libc::close(parent);
                    return Err(error);
                }
                if pid == 0 {
                    let peer = if i == 1 { slots[0].as_raw_fd() } else { -1 };
                    super::supervision::anchor(
                        peers[i].as_raw_fd(),
                        parent,
                        peer,
                        stop_ready == Some(i),
                    );
                }
                #[cfg(debug_assertions)]
                if let Some(probe) = &probe {
                    // SOCK_DGRAM preserves one identity per successfully forked
                    // anchor, even if rollback reaps it before spawn returns.
                    libc::send(
                        probe.as_raw_fd(),
                        (&pid as *const i32).cast(),
                        std::mem::size_of_val(&pid),
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                    );
                }
                let fd = libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32;
                if fd < 0 || libc::dup2(fd, slots[i].as_raw_fd()) < 0 {
                    let error = io::Error::last_os_error();
                    rollback(&slots_raw[..i], Some(pid));
                    if fd >= 0 {
                        libc::close(fd);
                    }
                    libc::close(parent);
                    return Err(error);
                }
                libc::close(fd);
            }
            if fail_after == Some(2) {
                rollback(&slots_raw, None);
                libc::close(parent);
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            let reported = report_raw(
                writer.as_raw_fd(),
                [parent, slots[0].as_raw_fd(), slots[1].as_raw_fd()],
            );
            libc::close(parent);
            if let Err(error) = reported {
                rollback(&slots_raw, None);
                return Err(error);
            }
            for fd in [
                writer.as_raw_fd(),
                controls[0].as_raw_fd(),
                controls[1].as_raw_fd(),
            ] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    let error = io::Error::last_os_error();
                    rollback(&slots_raw, None);
                    return Err(error);
                }
            }
            Ok(())
        });
    }
    Ok(())
}

pub(super) struct Private {
    pub controls: [OwnedFd; 2],
    pub anchors: [OwnedFd; 2],
    pub report: OwnedFd,
}
impl Drop for Private {
    fn drop(&mut self) {
        super::supervision::cleanup(self);
    }
}
pub(super) fn receive() -> io::Result<Private> {
    let value =
        std::env::var(VAR).map_err(|_| io::Error::other("missing private supervision metadata"))?;
    let fds: Vec<i32> = value
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| io::Error::other("invalid supervision descriptors"))?;
    if fds.len() != 5
        || fds
            .iter()
            .any(|f| *f < 3 || unsafe { libc::fcntl(*f, libc::F_GETFD) } < 0)
        || fds
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != 5
    {
        return Err(io::Error::other("invalid supervision descriptors"));
    }
    let mut handles = fds
        .into_iter()
        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
    let report = handles.next().unwrap();
    let controls = [handles.next().unwrap(), handles.next().unwrap()];
    let anchors = [handles.next().unwrap(), handles.next().unwrap()];
    for fd in controls.iter().chain(anchors.iter()).chain([&report]) {
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Private {
        controls,
        anchors,
        report,
    })
}
pub(super) fn report(fd: &OwnedFd, handles: &[&OwnedFd; 3]) -> io::Result<()> {
    report_raw(fd.as_raw_fd(), handles.map(AsRawFd::as_raw_fd))
}
fn report_raw(fd: i32, handles: [i32; 3]) -> io::Result<()> {
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: MAGIC.as_ptr().cast_mut().cast(),
        iov_len: MAGIC.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(12) as usize };
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(12) as usize;
        for (i, handle) in handles.iter().enumerate() {
            libc::CMSG_DATA(c).cast::<i32>().add(i).write(*handle);
        }
        if libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) != MAGIC.len() as isize
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
fn collect(registration: &mut Registration) {
    if !registration.handles.is_empty() {
        return;
    }
    let mut bytes = [0u8; 16];
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control);
    let n = unsafe {
        libc::recvmsg(
            registration.reader.as_raw_fd(),
            &mut msg,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if n < 0 {
        return;
    }
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        if !c.is_null() && (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS
        {
            let len = ((*c).cmsg_len - libc::CMSG_LEN(0) as usize) / 4;
            for i in 0..len {
                registration.handles.push(OwnedFd::from_raw_fd(
                    libc::CMSG_DATA(c).cast::<i32>().add(i).read(),
                ));
            }
        }
    }
    if n as usize != MAGIC.len()
        || &bytes[..MAGIC.len()] != MAGIC
        || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
        || registration.handles.len() != 3
    {
        registration.handles.clear();
    }
}
fn prune(all: &mut Vec<Registration>) {
    all.iter_mut().for_each(collect);
    all.retain(|r| {
        if r.handles.is_empty() {
            r.owner.upgrade().is_some()
        } else {
            r.handles.iter().any(live)
        }
    });
}
fn registered(pid: u32, auxiliary: bool) -> bool {
    let mut all = registry().lock().unwrap();
    prune(&mut all);
    all.iter().any(|r| {
        let range = if auxiliary {
            1..r.handles.len()
        } else {
            0..r.handles.len().min(1)
        };
        range
            .into_iter()
            .any(|i| number(&r.handles[i]) == Some(pid))
    })
}
pub(super) fn auxiliary(pid: u32) -> bool {
    registered(pid, true)
}
pub(super) fn launcher(pid: u32) -> bool {
    registered(pid, false)
}
