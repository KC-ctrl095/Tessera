//! Two private group anchors, born outside joined namespaces. Readiness and
//! completion are bounded. Keep the owned leader unreaped until cleanup, so a
//! fallback group signal is never authorized by a recycled numeric pgid.
use super::metadata;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(2);

// Only syscalls/fixed stack data: called after fork from a multithreaded server.
pub(super) unsafe fn anchor(control: i32, parent: i32, peer: i32, stop_ready: bool) -> ! {
    unsafe {
        let mut keep = [control, parent, if peer >= 0 { peer } else { control }];
        keep.sort_unstable();
        // Supervisor-only sweep. Keep private ownership channels/handles, never
        // host folders, stdio, the Rust exec-error pipe or program IPC channels.
        let mut first = 0u32;
        for fd in keep {
            let fd = fd as u32;
            if fd >= first {
                if fd > first && libc::syscall(libc::SYS_close_range, first, fd - 1, 0u32) < 0 {
                    libc::_exit(126);
                }
                first = fd + 1;
            }
        }
        if libc::syscall(libc::SYS_close_range, first, u32::MAX, 0u32) < 0 {
            libc::_exit(126);
        }
        libc::prctl(libc::PR_SET_NAME, c"tessera-anchor".as_ptr(), 0, 0, 0);
        for signal in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
            libc::SIGWINCH,
            libc::SIGPIPE,
        ] {
            libc::signal(signal, libc::SIG_IGN);
        }
        if stop_ready {
            libc::raise(libc::SIGSTOP);
        }
        let mut poll = [
            libc::pollfd {
                fd: control,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: parent,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            let n = libc::poll(poll.as_mut_ptr(), 2, -1);
            if n < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            }
            if n < 0 || poll[1].revents != 0 {
                if peer >= 0 {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        peer,
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    );
                }
                libc::_exit(0);
            }
            break;
        }
        let mut byte = 0u8;
        let mut ancillary = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&ancillary);
        if libc::recvmsg(control, &mut msg, libc::MSG_DONTWAIT) != 1 || byte != b'S' {
            if peer >= 0 {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    peer,
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
            libc::_exit(0);
        }
        let c = libc::CMSG_FIRSTHDR(&msg);
        if c.is_null()
            || (*c).cmsg_type != libc::SCM_CREDENTIALS
            || (*c).cmsg_level != libc::SOL_SOCKET
            || (*c).cmsg_len != libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) as usize
            || msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
        {
            libc::_exit(126);
        }
        // The kernel translates credentials to THIS (outer) PID namespace.
        // This private channel exists only in trusted pre_exec, never user exec.
        let pid = (*libc::CMSG_DATA(c).cast::<libc::ucred>()).pid;
        let leader = libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32;
        if leader < 0 || pid <= 1 || libc::getpgid(pid) != pid || libc::setpgid(0, pid) != 0 {
            libc::_exit(126);
        }
        libc::close(leader);
        let ready = b'R';
        if libc::send(
            control,
            (&ready as *const u8).cast(),
            1,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        ) != 1
        {
            if peer >= 0 {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    peer,
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
            libc::kill(0, libc::SIGKILL);
            libc::_exit(126);
        }
        loop {
            poll[0].revents = 0;
            poll[1].revents = 0;
            let n = libc::poll(poll.as_mut_ptr(), 2, -1);
            if n < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            }
            if n < 0
                || poll[1].revents != 0
                || libc::recv(
                    control,
                    (&mut byte as *mut u8).cast(),
                    1,
                    libc::MSG_DONTWAIT,
                ) != 1
                || byte != b'C'
            {
                // Protected live anchor; never a stored/reusable group id.
                if peer >= 0 {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        peer,
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    );
                }
                libc::kill(0, libc::SIGKILL);
            }
            libc::_exit(0);
        }
    }
}

fn event(fd: i32, stopped: bool) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags =
        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT | if stopped { libc::WSTOPPED } else { 0 };
    let r = unsafe { libc::waitid(libc::P_PIDFD, fd as u32, &mut info, flags) };
    if r != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { info.si_pid() } != 0)
    }
}
fn exited(fd: i32) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut p, 1, 0) != 0 }
}
fn request_cleanup(private: &metadata::Private) {
    for (control, anchor) in private.controls.iter().zip(&private.anchors) {
        metadata::signal(anchor, libc::SIGCONT);
        let byte = b'K';
        unsafe {
            libc::send(
                control.as_raw_fd(),
                (&byte as *const u8).cast(),
                1,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            );
        }
    }
}
fn finish(private: &metadata::Private) -> bool {
    let end = Instant::now() + BOUND;
    while Instant::now() < end {
        if private.anchors.iter().all(|fd| exited(fd.as_raw_fd())) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}
/// Never release the protected fallback on a mere observation of primary
/// liveness: a queued SIGKILL may not yet be visible. Retain it until the owned
/// primary has actually exited normally, with its status still unreaped.
fn complete(private: &metadata::Private, cmd: &std::process::Command) -> bool {
    #[cfg(debug_assertions)]
    let pause = cmd.get_envs().find_map(|(k, v)| {
        (k == "TESSERA_TEST_PAUSE_AFTER_PRIMARY")
            .then_some(v)
            .flatten()
    });
    #[cfg(not(debug_assertions))]
    let _ = cmd;
    for (control, anchor) in private.controls.iter().zip(&private.anchors) {
        let byte = b'C';
        if unsafe {
            libc::send(
                control.as_raw_fd(),
                (&byte as *const u8).cast(),
                1,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        } != 1
        {
            return false;
        }
        let end = Instant::now() + BOUND;
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let r = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    anchor.as_raw_fd() as u32,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if r != 0 {
                return false;
            }
            if unsafe { info.si_pid() } != 0 {
                if info.si_code != libc::CLD_EXITED || unsafe { info.si_status() } != 0 {
                    return false;
                }
                break;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        #[cfg(debug_assertions)]
        if anchor.as_raw_fd() == private.anchors[0].as_raw_fd()
            && let Some(path) = pause
        {
            if std::fs::write(path, "primary exited normally; fallback retained").is_err() {
                return false;
            }
            unsafe {
                libc::raise(libc::SIGSTOP);
            }
        }
    }
    true
}
fn reap(private: &metadata::Private) {
    for fd in &private.anchors {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::waitid(
                libc::P_PIDFD,
                fd.as_raw_fd() as u32,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            );
        }
    }
}

pub(super) fn cleanup(private: &metadata::Private) {
    if private.anchors.iter().all(|fd| exited(fd.as_raw_fd())) {
        reap(private);
        return;
    }
    request_cleanup(private);
    if !finish(private) {
        for fd in &private.anchors {
            metadata::signal(fd, libc::SIGKILL);
        }
    }
    reap(private);
}
/// Only the internal launcher uses this path: stdio/cwd already belong to the
/// outer launcher, env is explicit, and there are no arbitrary pre_exec hooks.
struct Program {
    pid: i32,
    handle: OwnedFd,
    reaped: bool,
}
impl Program {
    fn id(&self) -> u32 {
        self.pid as u32
    }
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.reaped {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        let mut status = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else if r == 0 {
            Ok(None)
        } else {
            self.reaped = true;
            Ok(Some(std::process::ExitStatus::from_raw(status)))
        }
    }
}
impl Drop for Program {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Our child is still unreaped. This PID/group cannot be recycled.
        unsafe {
            if libc::getpgid(self.pid) == self.pid {
                libc::kill(-self.pid, libc::SIGKILL);
            }
        }
        metadata::signal(&self.handle, libc::SIGKILL);
        let end = Instant::now() + BOUND;
        while Instant::now() < end {
            if self.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Entire child setup is syscall-only with data prepared before fork. The
/// parent, not a stoppable child, owns the exec/readiness deadline.
fn spawn(
    cmd: &std::process::Command,
    private: &metadata::Private,
    parent: i32,
) -> io::Result<Program> {
    use std::ffi::CString;
    let string = |s: &std::ffi::OsStr| {
        CString::new(s.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
    };
    let argv: Vec<_> = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(string)
        .collect::<io::Result<_>>()?;
    let mut env = Vec::new();
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            let mut entry = key.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            env.push(CString::new(entry).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?);
        }
    }
    let pointers = |values: &[CString]| {
        values
            .iter()
            .map(|s| s.as_ptr())
            .chain([std::ptr::null()])
            .collect::<Vec<_>>()
    };
    let args = pointers(&argv);
    let vars = pointers(&env);
    let program = cmd.get_program();
    let paths = if program.as_bytes().contains(&b'/') {
        vec![string(program)?]
    } else {
        let path = cmd
            .get_envs()
            .find_map(|(k, v)| (k == "PATH").then_some(v).flatten())
            .unwrap_or(std::ffi::OsStr::new("/usr/bin:/bin"));
        path.as_bytes()
            .split(|b| *b == b':')
            .map(|dir| {
                let mut candidate = dir.to_vec();
                if !candidate.is_empty() {
                    candidate.push(b'/');
                }
                candidate.extend_from_slice(program.as_bytes());
                CString::new(candidate).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
            })
            .collect::<io::Result<Vec<_>>>()?
    };
    #[cfg(debug_assertions)]
    let pause = cmd
        .get_envs()
        .find_map(|(k, v)| (k == "TESSERA_TEST_PREEXEC_STOP").then_some(v).flatten())
        .map(string)
        .transpose()?;
    #[cfg(not(debug_assertions))]
    let pause: Option<CString> = None;
    let controls = private.controls.each_ref().map(AsRawFd::as_raw_fd);
    let mut pipe = [-1; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let started = Instant::now();
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe {
            libc::close(reader.as_raw_fd());
            let setup = pre_exec(controls, parent, pause.as_ref().map(|s| s.as_ptr()));
            let mut error = setup;
            if error == 0 {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                let mut denied = false;
                for path in &paths {
                    libc::execve(path.as_ptr(), args.as_ptr(), vars.as_ptr());
                    error = *libc::__errno_location();
                    if error == libc::EACCES {
                        denied = true;
                    } else if error != libc::ENOENT && error != libc::ENOTDIR {
                        break;
                    }
                }
                if denied && (error == libc::ENOENT || error == libc::ENOTDIR) {
                    error = libc::EACCES;
                }
            }
            libc::write(
                writer.as_raw_fd(),
                (&error as *const i32).cast(),
                std::mem::size_of::<i32>(),
            );
            libc::_exit(126);
        }
    }
    drop(writer);
    // Immediately bind the forked child's identity, before any wait/reap.
    let handle = match metadata::pidfd(pid) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                // Own unreaped child, so numeric PID is not reusable. Keep
                // even this pidfd-open failure path bounded; never waitpid(0).
                libc::kill(pid, libc::SIGKILL);
                for _ in 0..2000 {
                    if libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) != 0 {
                        break;
                    }
                    libc::poll(std::ptr::null_mut(), 0, 1);
                }
            }
            return Err(error);
        }
    };
    let child = Program {
        pid,
        handle,
        reaped: false,
    };
    let end = started + BOUND;
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        let mut p = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut p, 1, remaining.as_millis().max(1) as i32) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            continue;
        }
        let mut error = 0i32;
        let read = unsafe {
            libc::read(
                reader.as_raw_fd(),
                (&mut error as *mut i32).cast(),
                std::mem::size_of::<i32>(),
            )
        };
        if read == 0 {
            return Ok(child);
        } // CLOEXEC closed it: requested exec succeeded.
        if read == std::mem::size_of::<i32>() as isize {
            return Err(io::Error::from_raw_os_error(error));
        }
        return Err(io::Error::other("invalid internal exec acknowledgement"));
    }
}

unsafe fn pre_exec(controls: [i32; 2], parent: i32, pause: Option<*const libc::c_char>) -> i32 {
    unsafe {
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            || libc::setpgid(0, 0) != 0
            || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
        {
            return *libc::__errno_location();
        }
        let mut p = libc::pollfd {
            fd: parent,
            events: libc::POLLIN,
            revents: 0,
        };
        if libc::poll(&mut p, 1, 0) != 0 {
            return libc::ECHILD;
        }
        if let Some(path) = pause {
            let fd = libc::open(
                path,
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
                0o600,
            );
            if fd < 0 {
                return *libc::__errno_location();
            }
            let mut digits = [0u8; 16];
            let mut at = digits.len();
            let mut pid = libc::getpid() as u32;
            loop {
                at -= 1;
                digits[at] = b'0' + (pid % 10) as u8;
                pid /= 10;
                if pid == 0 {
                    break;
                }
            }
            libc::write(fd, digits.as_ptr().add(at).cast(), digits.len() - at);
            libc::close(fd);
            // Test-only window: a real concurrent namespace peer sends STOP.
            libc::poll(std::ptr::null_mut(), 0, 1000);
        }
        let start = b'S';
        for fd in controls {
            if libc::send(
                fd,
                (&start as *const u8).cast(),
                1,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            ) != 1
            {
                return *libc::__errno_location();
            }
        }
        let mut began: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut began);
        for fd in controls {
            loop {
                let mut now: libc::timespec = std::mem::zeroed();
                libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
                let elapsed =
                    (now.tv_sec - began.tv_sec) * 1000 + (now.tv_nsec - began.tv_nsec) / 1_000_000;
                if elapsed >= 2000 {
                    return libc::ETIMEDOUT;
                }
                let mut p = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let n = libc::poll(&mut p, 1, (2000 - elapsed) as i32);
                if n < 0 && *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                let mut ready = 0u8;
                if n <= 0
                    || libc::recv(fd, (&mut ready as *mut u8).cast(), 1, libc::MSG_DONTWAIT) != 1
                    || ready != b'R'
                {
                    return libc::ETIMEDOUT;
                }
                break;
            }
        }
        if libc::isatty(0) == 1 {
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            let r = libc::tcsetpgrp(0, libc::getpid());
            libc::signal(libc::SIGTTOU, libc::SIG_DFL);
            if r != 0 {
                return *libc::__errno_location();
            }
        }
        0
    }
}
pub(super) fn run(
    cmd: std::process::Command,
    private: metadata::Private,
) -> io::Result<std::process::ExitStatus> {
    let parent = metadata::pidfd(unsafe { libc::getpid() })?;
    metadata::report(
        &private.report,
        &[&parent, &private.anchors[0], &private.anchors[1]],
    )?;
    let mut child = spawn(&cmd, &private, parent.as_raw_fd())?;
    let leader_fd = child.handle.as_raw_fd();
    let mut healthy = true;
    loop {
        if private
            .anchors
            .iter()
            .any(|fd| event(fd.as_raw_fd(), true).unwrap_or(true))
        {
            healthy = false;
            break;
        }
        if event(leader_fd, false)? {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Test-only real reap-to-notify seam. Production NEVER reaps the leader
    // until teardown; this seam additionally exercises the protected anchors.
    #[cfg(debug_assertions)]
    let saved = if let Some(path) = cmd.get_envs().find_map(|(k, v)| {
        (k == "TESSERA_TEST_PAUSE_AFTER_REAP")
            .then_some(v)
            .flatten()
    }) {
        let status = child.try_wait()?;
        std::fs::write(path, "leader reaped; private anchors retained")?;
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
        status
    } else {
        None
    };
    #[cfg(not(debug_assertions))]
    let saved: Option<std::process::ExitStatus> = None;
    healthy &= !private
        .anchors
        .iter()
        .any(|fd| event(fd.as_raw_fd(), true).unwrap_or(true));
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::waitid(
            libc::P_PIDFD,
            leader_fd as u32,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
    }
    let normal = healthy
        && saved
            .as_ref()
            .map_or(info.si_code == libc::CLD_EXITED, |s| s.signal().is_none());
    if !normal || !complete(&private, &cmd) {
        // Leader is OUR unreaped child (WNOWAIT above; no try_wait/wait yet).
        // Its retained pidfd + unreaped PID/group pin authorize this fallback.
        // Kill owned work BEFORE killing/reaping the final anchors.
        if saved.is_none() {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
        }
        request_cleanup(&private);
        let cleaned = finish(&private);
        if !cleaned {
            for fd in &private.anchors {
                metadata::signal(fd, libc::SIGKILL);
            }
        }
    }
    reap(&private);
    if let Some(status) = saved {
        return Ok(status);
    }
    let end = Instant::now() + BOUND;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= end {
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
