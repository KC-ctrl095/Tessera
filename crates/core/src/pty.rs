//! Pseudo-terminals for the user's own terminals (see `terminal.rs`): a
//! login shell with a controlling terminal, its window size, and whether it
//! waits at its prompt. Unix only (Windows would need ConPTY).

use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub type Child = tokio::process::Child;
use tokio::io::unix::AsyncFd;

/// The terminal's end Tessera holds; the program has the other one.
pub struct Pty {
    master: AsyncFd<OwnedFd>,
}

/// What to start, and where.
pub struct Start {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// The whole environment (nothing else is inherited).
    pub env: Vec<(OsString, OsString)>,
    pub cols: u16,
    pub rows: u16,
    /// Confinement, entered between fork and exec.
    pub sandbox: Option<crate::sandbox::Prepared>,
}

/// A started program and its terminal.
pub struct Spawned {
    pub pty: Pty,
    pub child: tokio::process::Child,
    /// The process started (`nsenter` when the sandbox joins a shared
    /// process namespace; see [`crate::sandbox::program_pid`]).
    pub pid: u32,
}

fn last() -> io::Error {
    io::Error::last_os_error()
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r == -1 { Err(last()) } else { Ok(r) }
}

/// Window sizes a terminal accepts (anything else is clamped).
pub fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(2, 1000), rows.clamp(1, 500))
}

fn set_size(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let (cols, rows) = clamp_size(cols, rows);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: a valid descriptor and a winsize that outlives the call.
    check(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws as *const libc::winsize) })?;
    Ok(())
}

/// A new terminal pair: the master (close-on-exec) and the path of the
/// program's end.
fn open_pair() -> io::Result<(OwnedFd, PathBuf)> {
    // SAFETY: plain system calls; the descriptor is owned right away.
    unsafe {
        let fd = check(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY))?;
        let master = OwnedFd::from_raw_fd(fd);
        let flags = check(libc::fcntl(fd, libc::F_GETFD))?;
        check(libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC))?;
        check(libc::grantpt(fd))?;
        check(libc::unlockpt(fd))?;
        let path = slave_name(fd)?;
        Ok((master, path))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn slave_name(fd: RawFd) -> io::Result<PathBuf> {
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: the buffer's length is passed along.
    let r = unsafe { libc::ptsname_r(fd, buf.as_mut_ptr(), buf.len()) };
    if r != 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    // SAFETY: ptsname_r wrote a terminated string into the buffer.
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Ok(PathBuf::from(name.to_string_lossy().into_owned()))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn slave_name(fd: RawFd) -> io::Result<PathBuf> {
    // `ptsname` shares one buffer: one caller at a time.
    static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _g = LOCK.lock();
    // SAFETY: the returned string is copied before the lock is released.
    unsafe {
        let p = libc::ptsname(fd);
        if p.is_null() {
            return Err(last());
        }
        Ok(PathBuf::from(
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned(),
        ))
    }
}

/// Start `start.program` on a new terminal: its own session with the
/// terminal as its controlling one, at lower priority than the server (like
/// the agent's commands), then the sandbox if any.
pub fn spawn(start: Start) -> io::Result<Spawned> {
    let (master, path) = open_pair()?;
    set_size(master.as_raw_fd(), start.cols, start.rows)?;
    let c_path = CString::new(path.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| io::Error::other("bad terminal name"))?;
    // SAFETY: a terminated path; the descriptor is owned right away.
    let slave = unsafe {
        OwnedFd::from_raw_fd(check(libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        ))?)
    };
    let (program, args) = match &start.sandbox {
        Some(s) => s.argv(start.program, start.args),
        None => (start.program, start.args),
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(start.env)
        .current_dir(&start.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .kill_on_drop(false);
    // Establish the terminal session before forking protected group anchors.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            crate::tools::bash::yield_to_server();
            Ok(())
        });
    }
    if let Some(s) = &start.sandbox {
        s.protect_environment(cmd.as_std_mut())?;
    }
    let sandbox = start.sandbox;
    // SAFETY: only async-signal-safe system calls in the child (the
    // sandbox's `enter` is written for exactly this place).
    unsafe {
        cmd.pre_exec(move || match &sandbox {
            Some(s) => s.enter(),
            None => Ok(()),
        });
    }
    let child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    // SAFETY: a descriptor we own.
    unsafe {
        let fd = master.as_raw_fd();
        let flags = check(libc::fcntl(fd, libc::F_GETFL))?;
        check(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
    }
    Ok(Spawned {
        pty: Pty {
            master: AsyncFd::new(master)?,
        },
        child,
        pid,
    })
}

impl Pty {
    /// Read what the program wrote; `Ok(0)` once every program holding the
    /// terminal has closed it.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.master.readable().await?;
            // SAFETY: the buffer's length is passed along.
            match guard.try_io(|fd| {
                let n = unsafe {
                    libc::read(
                        fd.as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                if n < 0 { Err(last()) } else { Ok(n as usize) }
            }) {
                Ok(Ok(n)) => return Ok(n),
                // Linux says EIO once the other end is closed for good.
                Ok(Err(e)) if e.raw_os_error() == Some(libc::EIO) => return Ok(0),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }

    /// Type `data` into the terminal (waits while the program doesn't read).
    pub async fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let mut guard = self.master.writable().await?;
            // SAFETY: the slice's length is passed along.
            match guard.try_io(|fd| {
                let n = unsafe {
                    libc::write(
                        fd.as_raw_fd(),
                        data.as_ptr() as *const libc::c_void,
                        data.len(),
                    )
                };
                if n < 0 { Err(last()) } else { Ok(n as usize) }
            }) {
                Ok(Ok(n)) => data = &data[n..],
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_size(self.master.as_raw_fd(), cols, rows)
    }

    /// The process group in the foreground (the shell itself while it
    /// waits at its prompt).
    pub fn foreground(&self) -> Option<i32> {
        // SAFETY: a valid descriptor.
        let g = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        (g > 0).then_some(g)
    }

    /// Ask the program in front to draw itself again (after a reconnect).
    pub fn redraw(&self) {
        if let Some(g) = self.foreground() {
            // SAFETY: a signal to a process group.
            unsafe {
                libc::killpg(g, libc::SIGWINCH);
            }
        }
    }
}

/// The account's home folder (`$HOME` as the service has it, else the
/// password database's) and login shell (the password database's, which a
/// service's environment may lack).
pub fn account() -> (Option<PathBuf>, PathBuf) {
    // SAFETY: getpwuid_r with a buffer it may fill; strings are copied out.
    let (home, shell) = unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let mut buf = vec![0 as libc::c_char; 4096];
        let r = libc::getpwuid_r(
            libc::geteuid(),
            &mut pw,
            buf.as_mut_ptr(),
            buf.len(),
            &mut out,
        );
        if r == 0 && !out.is_null() {
            let s = |p: *const libc::c_char| {
                (!p.is_null())
                    .then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
            };
            (s(pw.pw_dir), s(pw.pw_shell))
        } else {
            (None, None)
        }
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute() && h.is_dir())
        .or(home.map(PathBuf::from))
        .or_else(dirs::home_dir);
    let usable = |p: &Path| {
        p.is_absolute() && p.is_file() && !p.ends_with("nologin") && !p.ends_with("false")
    };
    let shell = shell
        .map(PathBuf::from)
        .filter(|p| usable(p))
        .or_else(|| {
            std::env::var_os("SHELL")
                .map(PathBuf::from)
                .filter(|p| usable(p))
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    (home, shell)
}

/// How a terminal starts the account's shell: a login shell, so the
/// account's profile sets up the environment as at a login (a service has
/// little of it), and interactive, so the shell's own settings apply as in a
/// native terminal (the prompt, aliases such as `ls --color`, completion).
/// bash reads `~/.bashrc` only when it is not a login shell, unless the
/// profile sources it (Debian's `~/.profile` does; many `~/.bash_profile`s
/// don't): there a login bash sets up the environment and then becomes an
/// interactive one, which reads `~/.bashrc`, as a desktop's terminals do.
/// zsh, fish and others read both kinds of files as interactive login shells.
pub fn shell_start(shell: &Path) -> (OsString, Vec<OsString>) {
    let program = shell.as_os_str().to_owned();
    let name = shell.file_name().map(|n| n.to_string_lossy().into_owned());
    match name.as_deref() {
        // `$0` is the shell's own path (the word after the command), so
        // nothing needs quoting.
        Some("bash") => (
            program.clone(),
            vec!["-l".into(), "-c".into(), "exec \"$0\" -i".into(), program],
        ),
        _ => (program, vec!["-l".into()]),
    }
}

/// Variables a terminal never gets from the process starting it: Tessera's
/// own, and anything named like a credential (a service's API keys).
pub fn withheld(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
    ];
    let upper = name.to_ascii_uppercase();
    upper.starts_with("TESSERA") || WORDS.iter().any(|w| upper.contains(w))
}

/// What a sandboxed terminal gets from the process starting it (nothing
/// else; `HOME`, `SHELL`, `TERM` and `COLORTERM` are set here), and why:
///
/// * `USER`, `LOGNAME`: who the shell runs as (prompts, `ssh`, `git`).
/// * `PATH`: where programs are found.
/// * `LANG`, `LANGUAGE`, `LC_*`, `TZ`: language, characters and time zone.
/// * `EDITOR`, `VISUAL`, `PAGER`: the user's editor and pager.
/// * `TMPDIR`: where temporary files go.
/// * `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`,
///   `XDG_STATE_HOME`, `XDG_CONFIG_DIRS`, `XDG_DATA_DIRS`: where programs
///   keep their settings and data, if moved.
/// * `SSH_AUTH_SOCK`: the SSH agent, which signs but can't run anything (as
///   for the agent's commands; the sandbox keeps its socket reachable).
/// * `LS_COLORS`: the colours `ls` gives files and folders, if set.
///
/// Never the session bus, display, window manager or service manager
/// variables (the sandbox hides their sockets anyway), nor credentials.
const SANDBOXED_ENV: &[&str] = &[
    "USER",
    "LOGNAME",
    "PATH",
    "LANG",
    "LANGUAGE",
    "TZ",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_DIRS",
    "SSH_AUTH_SOCK",
    "LS_COLORS",
];

/// A sandboxed terminal may get `name` ([`SANDBOXED_ENV`], or `LC_*`).
fn sandboxed_may(name: &str) -> bool {
    (SANDBOXED_ENV.contains(&name) || name.starts_with("LC_")) && !withheld(name)
}

/// A terminal's environment, for `home` and `shell`, with a capable `TERM`:
/// sandboxed, only what [`SANDBOXED_ENV`] lists of this process's; with
/// full access, all of it without what [`withheld`] keeps back.
pub fn environment(
    home: Option<&Path>,
    shell: &Path,
    sandboxed: bool,
) -> Vec<(OsString, OsString)> {
    environment_from(std::env::vars_os(), home, shell, sandboxed)
}

/// [`environment`] from `vars` (this process's).
fn environment_from(
    vars: impl Iterator<Item = (OsString, OsString)>,
    home: Option<&Path>,
    shell: &Path,
    sandboxed: bool,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = vars
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            if sandboxed {
                sandboxed_may(&k)
            } else {
                !withheld(&k)
            }
        })
        .filter(|(k, _)| {
            !matches!(
                k.to_str(),
                Some("TERM" | "COLORTERM" | "SHELL" | "HOME" | "INVOCATION_ID" | "JOURNAL_STREAM")
            )
        })
        .collect();
    env.push(("TERM".into(), "xterm-256color".into()));
    env.push(("COLORTERM".into(), "truecolor".into()));
    env.push(("SHELL".into(), shell.as_os_str().to_owned()));
    if let Some(h) = home {
        env.push(("HOME".into(), h.as_os_str().to_owned()));
    }
    // The grid decodes UTF-8. Services often inherit LANG=C, which otherwise
    // makes readline and TUIs count bytes instead of cells. Keep UTF-8 user
    // locales and unrelated LC_* categories, but repair encoding overrides.
    let utf8 = if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    };
    for (key, value) in &mut env {
        if matches!(key.to_str(), Some("LANG" | "LC_ALL" | "LC_CTYPE")) {
            let locale = value
                .to_string_lossy()
                .to_ascii_lowercase()
                .replace('-', "");
            if !locale.contains("utf8") && (key.to_str() == Some("LANG") || !locale.is_empty()) {
                *value = utf8.into();
            }
        }
    }
    if !env.iter().any(|(k, _)| k == "LANG") {
        env.push(("LANG".into(), utf8.into()));
    }
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push((
            "PATH".into(),
            "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin".into(),
        ));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn read_until(pty: &Pty, want: &str) -> String {
        let mut all = String::new();
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !all.contains(want) {
            let n = tokio::time::timeout_at(deadline, pty.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no {want:?} in {all:?}"))
                .unwrap();
            if n == 0 {
                break;
            }
            all.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        all
    }

    #[tokio::test]
    async fn a_sandboxed_terminal_keeps_foreground_input_and_process_accounting() {
        if crate::sandbox::test_mode("native shared PTY foreground").is_none() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        let plan = crate::sandbox::Prepared::new(
            &crate::sandbox::Spec {
                folders: vec![(work.clone(), true)],
                group: work.display().to_string(),
                ..Default::default()
            },
            &work,
        )
        .unwrap();
        let mut s = spawn(Start {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "stty size; echo ready; read -r x; echo got:$x".into(),
            ],
            cwd: work,
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cols: 91,
            rows: 33,
            sandbox: Some(plan),
        })
        .unwrap();
        let out = read_until(&s.pty, "ready").await;
        assert!(out.contains("33 91"), "{out:?}");
        assert_eq!(
            s.pty.foreground(),
            Some(crate::sandbox::program_pid(s.pid) as i32)
        );
        s.pty.write_all(b"hello\r").await.unwrap();
        let out = read_until(&s.pty, "got:hello").await;
        assert!(out.contains("got:hello"), "{out:?}");
        assert!(
            tokio::time::timeout(Duration::from_secs(5), s.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    #[tokio::test]
    async fn a_program_gets_a_real_terminal_of_the_right_size() {
        let _dog = crate::terminal::watchdog("a pty test", 60);
        let s = spawn(Start {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "stty size; tty; read x; echo got:$x".into()],
            cwd: std::env::temp_dir(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cols: 91,
            rows: 33,
            sandbox: None,
        })
        .unwrap();
        let out = read_until(&s.pty, "/dev/").await;
        assert!(out.contains("33 91"), "{out:?}");
        s.pty.write_all(b"hello\r").await.unwrap();
        let out = read_until(&s.pty, "got:hello").await;
        assert!(out.contains("got:hello"), "{out:?}");
        let mut child = s.child;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        if status.is_err() {
            crate::node::exec::stop_tree(s.pid);
        }
        assert!(status.unwrap().unwrap().success());
    }

    /// bash as in a native terminal: its login profile and then `~/.bashrc`,
    /// even when the profile doesn't read it (the colours of `ls` come from
    /// there); other shells start as interactive login shells.
    #[tokio::test]
    async fn bash_reads_its_profile_and_its_bashrc() {
        let _dog = crate::terminal::watchdog("a pty test", 60);
        let Some(bash) = ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.is_file())
        else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".bash_profile"),
            "export FROM_PROFILE=yes\n",
        )
        .unwrap();
        std::fs::write(
            home.path().join(".bashrc"),
            "alias ls='ls --color=auto'\nPS1='\\[\\e[32m\\]ready> \\[\\e[0m\\]'\n",
        )
        .unwrap();
        let (program, args) = shell_start(bash);
        let s = spawn(Start {
            program,
            args,
            cwd: home.path().to_path_buf(),
            env: environment_from(
                [("PATH".into(), "/usr/bin:/bin".into())].into_iter(),
                Some(home.path()),
                bash,
                true,
            ),
            cols: 80,
            rows: 24,
            sandbox: None,
        })
        .unwrap();
        let prompt = read_until(&s.pty, "ready> ").await;
        assert!(prompt.contains("\x1b[32m"), "{prompt:?}");
        s.pty
            .write_all(b"echo profile=$FROM_PROFILE; type ls; printf 'env=%s,%s,%s\\n' \"$TERM\" \"$COLORTERM\" \"$LANG\"; case $- in *i*) echo interactive=yes;; esac; pwd; printf '\\nterminal-test-done\\n'; exit\r")
            .await
            .unwrap();
        // The command line is echoed first, then its result.
        let out = read_until(&s.pty, "\r\nterminal-test-done\r\n").await;
        assert!(out.contains("profile=yes"), "{out:?}");
        assert!(out.contains("ls --color=auto"), "{out:?}");
        assert!(out.contains("env=xterm-256color,truecolor,"), "{out:?}");
        assert!(out.contains("\r\ninteractive=yes\r\n"), "{out:?}");
        assert!(
            out.contains(&format!("\r\n{}\r\n", home.path().display())),
            "{out:?}"
        );
        let mut child = s.child;
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_err()
        {
            crate::node::exec::stop_tree(s.pid);
        }
        // Others: an interactive login shell, as before.
        let (_, args) = shell_start(Path::new("/usr/bin/zsh"));
        assert_eq!(args, vec![OsString::from("-l")]);
        let (_, args) = shell_start(Path::new("/usr/bin/fish"));
        assert_eq!(args, vec![OsString::from("-l")]);
    }

    #[test]
    fn terminals_repair_non_utf8_service_locales_without_changing_other_categories() {
        for sandboxed in [false, true] {
            for vars in [
                vec![("LANG", "C"), ("LC_ALL", "POSIX")],
                vec![("LANG", "en_US.UTF-8"), ("LC_CTYPE", "C")],
                vec![("LANG", "de_DE.utf8"), ("LC_ALL", ""), ("LC_TIME", "nl_NL")],
                vec![],
            ] {
                let env = environment_from(
                    vars.iter().map(|(k, v)| ((*k).into(), (*v).into())),
                    None,
                    Path::new("/bin/bash"),
                    sandboxed,
                );
                let get = |key: &str| {
                    env.iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| v.to_string_lossy().to_string())
                };
                let lang = get("LANG").unwrap();
                assert!(lang.to_lowercase().replace('-', "").contains("utf8"));
                for key in ["LC_ALL", "LC_CTYPE"] {
                    if let Some(value) = get(key) {
                        assert!(
                            value.is_empty()
                                || value.to_lowercase().replace('-', "").contains("utf8")
                        );
                    }
                }
                if vars.contains(&("LANG", "de_DE.utf8")) {
                    assert_eq!(lang, "de_DE.utf8");
                    assert_eq!(get("LC_TIME").as_deref(), Some("nl_NL"));
                    assert_eq!(get("LC_ALL").as_deref(), Some(""));
                }
            }
        }
    }

    /// Sandboxed: only what the shell needs, from a list; never the
    /// session bus or the desktop. Full access: everything but credentials
    /// and Tessera's own settings.
    #[test]
    fn a_sandboxed_terminal_gets_only_the_listed_variables() {
        let vars = || {
            [
                ("PATH", "/usr/bin:/bin"),
                ("USER", "me"),
                ("LANG", "en_US.UTF-8"),
                ("LC_TIME", "nl_NL.UTF-8"),
                ("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.socket"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
                ("DISPLAY", ":0"),
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
                ("SYSTEMD_EXEC_PID", "42"),
                ("MANAGERPID", "7"),
                ("GITHUB_TOKEN", "ghp_x"),
                ("TESSERA_HOME", "/srv/tessera"),
                ("SOME_TOOL_SETTING", "1"),
                ("TERM", "dumb"),
                ("HOME", "/elsewhere"),
                ("LS_COLORS", "di=01;34"),
            ]
            .into_iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        };
        let names = |env: &[(OsString, OsString)]| -> Vec<String> {
            env.iter()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect()
        };
        let home = Path::new("/home/me");
        let shell = Path::new("/bin/bash");
        let sandboxed = environment_from(vars(), Some(home), shell, true);
        let got = names(&sandboxed);
        for want in [
            "PATH",
            "USER",
            "LANG",
            "LC_TIME",
            "SSH_AUTH_SOCK",
            "TERM",
            "COLORTERM",
            "SHELL",
            "HOME",
            "LS_COLORS",
        ] {
            assert!(got.iter().any(|k| k == want), "{want}: {got:?}");
        }
        for never in [
            "DBUS_SESSION_BUS_ADDRESS",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "SYSTEMD_EXEC_PID",
            "MANAGERPID",
            "GITHUB_TOKEN",
            "TESSERA_HOME",
            "SOME_TOOL_SETTING",
        ] {
            assert!(!got.iter().any(|k| k == never), "{never}: {got:?}");
        }
        let value = |env: &[(OsString, OsString)], k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(value(&sandboxed, "TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(value(&sandboxed, "HOME").as_deref(), Some("/home/me"));
        // Full access keeps the rest, credentials and Tessera's own aside.
        let full = environment_from(vars(), Some(home), shell, false);
        let got = names(&full);
        for want in ["DBUS_SESSION_BUS_ADDRESS", "SOME_TOOL_SETTING", "DISPLAY"] {
            assert!(got.iter().any(|k| k == want), "{want}: {got:?}");
        }
        for never in ["GITHUB_TOKEN", "TESSERA_HOME"] {
            assert!(!got.iter().any(|k| k == never), "{never}: {got:?}");
        }
    }

    #[test]
    fn credentials_and_tessera_settings_stay_out_of_the_environment() {
        for k in [
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "TESSERA_HOME",
            "DB_PASSWORD",
        ] {
            assert!(withheld(k), "{k}");
        }
        for k in ["PATH", "LANG", "SSH_AUTH_SOCK", "XDG_SESSION_ID", "USER"] {
            assert!(!withheld(k), "{k}");
        }
    }
}
