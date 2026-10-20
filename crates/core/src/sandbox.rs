//! Confinement for the commands the agent runs.
//!
//! Commands run as the user, so path checks on the command line can only
//! catch honest mistakes. They can't stop a model that has been talked into
//! fetching Tessera's own credentials (for example via a prompt injection in
//! a web page). On Linux every command therefore also gets:
//!
//! * a private user and mount namespace in which Tessera's data directory,
//!   config directory and the app's saved servers are covered by empty,
//!   read-only tmpfs mounts, with the skills directory put back read-only;
//! * the same for the user session's control sockets ([`session_hides`]):
//!   `systemd-run --user`, the session bus, tmux and friends start programs
//!   *outside* any sandbox, so one command could otherwise read everything;
//! * the folders the user made read-only (Settings → Privacy & safety) mounted
//!   read-only, with writable folders inside them left writable;
//! * the folders holding any of those mounted on themselves, so none can be
//!   moved away with what it holds and another put in its place (a made-up
//!   `~/.config/tessera` that the next start would read);
//! * no way around those mounts through another process's
//!   `/proc/<pid>/root`, `environ`, `mem` or `fd`. With Landlock ABI 2+
//!   (Linux 5.19+) each command gets a Landlock domain, which only allows
//!   that for processes inside the same sandbox, forbids mounting and
//!   unmounting and (Linux 6.12+) connecting to abstract Unix sockets made
//!   outside it (X11, old-style session buses). Older kernels (Landlock ABI 1
//!   can't be used: it forbids moving files between directories, which git
//!   needs) get a private process namespace instead, one per chat
//!   ([`Spec::group`]) shared by its commands and entered with `nsenter`:
//!   its own `/proc` shows only processes started there, so background
//!   servers can still be listed and stopped by later commands, but nothing
//!   outside, other chats' commands included.
//!
//! All of it is unprivileged kernel features. Where they are missing (very
//! old kernels, containers with strict seccomp profiles, Ubuntu's AppArmor
//! limit on user namespaces) Automatic requires single-use command approval;
//! Strict refuses incomplete confinement and Off is an explicit opt-out.
//! [`hint`] says what to change. Everything else about the command is
//! unchanged: same user, same files, same network. The visible differences
//! are that `sudo` and other setuid programs can't gain privileges and the
//! desktop session (D-Bus, `systemctl --user`, the GPG agent) is out of
//! reach; the SSH agent stays usable.
//!
//! Startup files and reachable cron spools are kept read-only by the server.
//! Not covered: other ways the account can run code elsewhere, such as an
//! accessible Docker socket. This is not a container.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What to hide from a command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spec {
    /// Directories replaced by an empty, read-only view; single files
    /// (another program's credentials) read as empty.
    pub hide: Vec<PathBuf>,
    /// Directories or single files (e.g. an agent socket) inside `hide` that
    /// stay visible (read-only).
    pub keep: Vec<PathBuf>,
    /// The user's folder rules: `false` = read-only, `true` = writable (only
    /// matters inside a read-only one). The most specific folder wins. A
    /// single file can be made read-only too (Tessera's own program): it
    /// then can't be changed, replaced or deleted.
    pub folders: Vec<(PathBuf, bool)>,
    /// When set, the command may create, change or delete files only
    /// beneath these directories, and change (not replace) these single
    /// files (and write to devices such as `/dev/null` and shared memory in
    /// `/dev/shm`); everything else stays readable. Links are ignored.
    /// Enforced with Landlock, so it needs ABI 3+ (Linux 6.2) and is
    /// refused without it. Machines (`tessera node`) use this.
    pub writable: Option<Vec<PathBuf>>,
    /// Whose commands these are (a chat and the subagents it started).
    /// Where commands share a process namespace (no Landlock ABI 2), each
    /// group gets its own: one chat's commands never see another's
    /// processes, their environment (secrets) or open pipes.
    pub group: String,
}

/// Startup locations are read-only even before they exist. Their nearest
/// existing parents stay read-only, with safe existing siblings opened back
/// up. Nothing creates a new startup name or swaps a symlink into its place.
/// Folder rules still narrow writes. A bounded scan leaves excess entries
/// read-only rather than silently dropping protection.
pub(crate) fn protect_startup(
    folders: Vec<(PathBuf, bool)>,
    startup: Vec<PathBuf>,
) -> Vec<(PathBuf, bool)> {
    let folders: Vec<_> = folders
        .into_iter()
        .map(|(p, w)| (crate::tools::canonical(&p), w))
        .collect();
    let protected: Vec<_> = startup
        .into_iter()
        .flat_map(|p| [crate::tools::canonical(&p), p])
        .collect();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if protected
            .iter()
            .any(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.nlink() > 1))
        {
            // A read-only path mount cannot stop an existing hard-link alias
            // elsewhere. Fail closed on all writes instead of guessing where
            // those aliases live. Explicit Off never builds these rules.
            return vec![(PathBuf::from("/"), false)];
        }
    }
    let mut roots: Vec<PathBuf> = protected
        .iter()
        .filter_map(|p| {
            p.parent()?
                .ancestors()
                .find(|p| p.is_dir())
                .map(Path::to_path_buf)
        })
        .collect();
    roots.sort_by_key(|p| p.components().count());
    roots.dedup();
    let mut outer = Vec::new();
    for p in roots {
        if !outer.iter().any(|q: &PathBuf| p.starts_with(q)) {
            outer.push(p);
        }
    }
    fn open(
        p: &Path,
        protected: &[PathBuf],
        rules: &[(PathBuf, bool)],
        out: &mut Vec<(PathBuf, bool)>,
        budget: &mut usize,
    ) {
        if *budget == 0 || protected.iter().any(|k| p.starts_with(k)) {
            return;
        }
        *budget -= 1;
        if !protected.iter().any(|k| k.starts_with(p)) {
            let write = rules
                .iter()
                .filter(|(q, _)| p.starts_with(q))
                .max_by_key(|(q, _)| q.components().count())
                .is_none_or(|(_, w)| *w);
            if write {
                out.push((p.to_path_buf(), true));
            }
        } else if let Ok(entries) = std::fs::read_dir(p) {
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| !t.is_symlink()) {
                    open(&e.path(), protected, rules, out, budget);
                }
                if *budget == 0 {
                    break;
                }
            }
        }
    }
    let mut out: Vec<_> = outer.iter().map(|p| (p.clone(), false)).collect();
    let mut budget = 512;
    for root in &outer {
        open(root, &protected, &folders, &mut out, &mut budget);
    }
    out.extend(folders.into_iter().filter(|(p, w)| {
        !*w || !protected
            .iter()
            .any(|k| p.starts_with(k) || k.starts_with(p))
    }));
    out
}

/// Where the user's session keeps sockets that make a service manager,
/// multiplexer, editor or container engine run a program outside the
/// sandbox (`systemd-run --user cat ~/.local/share/tessera/auth.json`).
pub fn session_hides() -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let mut v: Vec<PathBuf> = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        v.push(PathBuf::from(format!("/run/user/{uid}")));
        let tmp = std::env::var_os("TMUX_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        v.push(tmp.join(format!("tmux-{uid}")));
        v.push(PathBuf::from(format!("/tmp/tmux-{uid}")));
        v.push(PathBuf::from(format!("/tmp/zellij-{uid}")));
        v.push(PathBuf::from(format!("/tmp/emacs{uid}")));
        v.push(PathBuf::from("/tmp/.X11-unix"));
        if let Some(user) = std::env::var_os("USER") {
            let user = user.to_string_lossy();
            v.push(PathBuf::from(format!("/run/screen/S-{user}")));
            v.push(PathBuf::from(format!("/var/run/screen/S-{user}")));
        }
        v.extend(dirs::home_dir().map(|h| h.join(".screen")));
        // Without Landlock ABI 6 (Linux 6.12) a command can still reach the
        // X server's abstract socket; without its login cookie the server
        // turns it away.
        if support().landlock < 6 {
            v.extend(dirs::home_dir().map(|h| h.join(".Xauthority")));
            v.extend(
                std::env::var_os("XAUTHORITY")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute()),
            );
        }
        v
    }
    #[cfg(not(unix))]
    Vec::new()
}

/// The SSH agent signs but can't run anything, and `git` over SSH needs it.
pub fn session_keeps() -> Vec<PathBuf> {
    std::env::var_os("SSH_AUTH_SOCK")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .into_iter()
        .collect()
}

/// Whether the kernel supports the sandbox, probed once.
#[derive(Debug, Clone)]
pub struct Support {
    pub available: bool,
    /// Landlock ABI version (0 = unavailable).
    pub landlock: i32,
    /// Commands can share a private process namespace with its own `/proc`
    /// (how other processes stay out of reach without Landlock ABI 2).
    pub private_pids: bool,
    /// Why it's unavailable, for diagnostics.
    pub reason: Option<String>,
}

impl Support {
    /// Namespaces plus Landlock or a private process view: otherwise a
    /// command can still read hidden files through `/proc/<pid>/root` of
    /// the user's other processes.
    pub fn complete(&self) -> bool {
        self.available && (self.landlock >= 2 || self.private_pids)
    }

    /// Commands go through the shared process namespace.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn shared(&self) -> bool {
        self.available && self.landlock < 2 && self.private_pids
    }
}

/// Tests can pretend the kernel's Landlock is older than it is
/// (`TESSERA_TEST_LANDLOCK_ABI=1`), to exercise what such servers do.
const LANDLOCK_CAP_VAR: &str = "TESSERA_TEST_LANDLOCK_ABI";

pub fn support() -> &'static Support {
    static S: std::sync::OnceLock<Support> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let cap = std::env::var(LANDLOCK_CAP_VAR)
            .ok()
            .and_then(|v| v.trim().parse().ok());
        imp::probe(cap)
    })
}

/// Which sandbox a check run must exercise (`direct` or `shared`), for
/// [`test_mode`].
const TEST_SANDBOX_VAR: &str = "TESSERA_TEST_SANDBOX";

/// For tests that need the command sandbox: the mode it runs in here
/// (`direct`: own namespaces per command, `shared`: a chat's shared process
/// namespace, Landlock below ABI 2), or `None` when it can't run, which the
/// test then skips with a line on stderr. A check run that must exercise a
/// mode sets `TESSERA_TEST_SANDBOX=direct|shared`: then any other outcome
/// fails the test instead of skipping it (docs/TESTING.md).
#[doc(hidden)]
pub fn test_mode(test: &str) -> Option<&'static str> {
    let s = support();
    let mode = s
        .available
        .then(|| if s.shared() { "shared" } else { "direct" });
    let want = std::env::var(TEST_SANDBOX_VAR)
        .ok()
        .filter(|v| !v.trim().is_empty());
    if let Some(want) = want
        && Some(want.trim()) != mode
    {
        panic!(
            "{test}: this check run needs the {} command sandbox, but here it is {} ({})",
            want.trim(),
            mode.unwrap_or("unavailable"),
            describe()
        );
    }
    match mode {
        Some(m) => eprintln!("{test}: command sandbox {m} ({})", describe()),
        None => eprintln!("SKIP {test}: the command sandbox is {}", describe()),
    }
    mode
}

/// One line for banners and diagnostics.
pub fn describe() -> String {
    let s = support();
    if s.available {
        if s.landlock >= 2 {
            "on (namespaces + Landlock)".into()
        } else if s.private_pids {
            "on (namespaces + a private process view)".into()
        } else {
            "partial (namespaces only: no Landlock on this kernel, so /proc of other processes is reachable)".into()
        }
    } else {
        format!(
            "unavailable ({})",
            s.reason.as_deref().unwrap_or("unsupported platform")
        )
    }
}

/// What the user can do about a missing or partial sandbox, or a server
/// running as root (commands in backticks); `None` when all is well.
pub fn hint() -> Option<String> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return Some("Tessera runs as root, so the agent's commands can change system files. Run it as a normal user: `sudo tessera service install --user <name>` (or `tessera service install` as that user).".into());
    }
    let s = support();
    if s.complete() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        Some(imp::hint(s))
    }
    #[cfg(windows)]
    {
        Some("Tessera can't sandbox the agent's commands on Windows yet. Every command asks first until you explicitly choose Off in Settings → Privacy & safety → What commands can reach. Without the safe box, commands have your account's access, including Tessera's keys and chats. Run the server on Linux for the sandbox.".into())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Some("The command safe box exists only on Linux. Every command asks first until you explicitly choose Off in Settings → Privacy & safety → What commands can reach. Without it, commands have your account's access, including Tessera's keys and chats.".into())
    }
}

/// The sandbox for a command started in `cwd`, given the host's setting
/// (`None` = don't sandbox; `true` = strict), or why it must not run.
pub fn prepare(sandbox: Option<&(Spec, bool)>, cwd: &Path) -> Result<Option<Prepared>, String> {
    let Some((spec, strict)) = sandbox else {
        return Ok(None);
    };
    let s = support();
    if spec.writable.is_some() && !confines_writes() {
        return Err(format!(
            "Refused: commands here may only change the folders allowed for them, which needs Landlock ABI 3 (Linux 6.2 or newer), but the sandbox is {}.",
            describe()
        ));
    }
    if s.complete() || (s.available && !strict) {
        Prepared::new(spec, cwd)
            .map(Some)
            .map_err(|e| format!("Could not prepare the command sandbox: {e}"))
    } else if *strict {
        Err(format!(
            "Refused: commands must run sandboxed (security.sandbox = \"strict\"), but the sandbox is {}.",
            describe()
        ))
    } else {
        Err(format!(
            "The command safe box is unavailable ({}). Commands need exact approval until you explicitly choose Off in What commands can reach.",
            describe()
        ))
    }
}

/// Commands can be kept from changing files outside chosen folders
/// ([`Spec::writable`]): the full sandbox with Landlock ABI 3 or newer (including truncate).
pub fn confines_writes() -> bool {
    let s = support();
    s.available && s.landlock >= 3
}

/// The process running a sandboxed program started as `pid`: the child of
/// the native shared launcher (or old nsenter), else `pid` itself.
pub fn program_pid(pid: u32) -> u32 {
    let proc_ = |f: &str| std::fs::read_to_string(format!("/proc/{pid}/{f}")).unwrap_or_default();
    #[cfg(target_os = "linux")]
    let native = metadata::launcher(pid);
    #[cfg(not(target_os = "linux"))]
    let native = false;
    if proc_("comm").trim() != "nsenter" && !native {
        return pid;
    }
    proc_(&format!("task/{pid}/children"))
        .split_whitespace()
        .filter_map(|c| c.parse().ok())
        .find(|child| !supervisor_auxiliary(*child))
        .unwrap_or(pid)
}

#[cfg(target_os = "linux")]
mod environment;
#[cfg(target_os = "linux")]
mod metadata;
/// Internal anchors, identified only by a private server-issued registration
/// and retained kernel pidfds. A requested program's comm is never authority.
pub(crate) fn supervisor_auxiliary(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        metadata::auxiliary(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

#[cfg(target_os = "linux")]
mod launcher;
#[cfg(target_os = "linux")]
mod supervision;
#[cfg(target_os = "linux")]
pub use launcher::entry as directory_launcher_entry;

/// Production re-enters the hosting executable. Test harnesses use a tiny
/// companion which calls the identical entry point (no libtest parsing).
#[cfg(target_os = "linux")]
fn directory_launcher() -> PathBuf {
    let exe = std::env::current_exe().expect("the running executable has a path");
    #[cfg(debug_assertions)]
    if exe.parent().is_some_and(|p| p.ends_with("deps")) {
        if let Some(cli) = std::env::var_os("TESSERA_TEST_LAUNCHER_EXE") {
            return PathBuf::from(cli);
        }
        return exe
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/sandbox_launcher");
    }
    exe
}
/// How a launch whose working folder is held open enters it
/// ([`Prepared::directory_entry`]).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryEntry {
    /// Open the folder's path again after the mounts, check it is the held
    /// folder, and enter that.
    Reopen,
    /// Shared mode passes only values to the native launcher. Close the host
    /// descriptor before nsenter starts; only the matching checked launch
    /// may use this path (-1 means no checked argv was prepared).
    Values(i32),
}

/// The exit status of a command refused because its working folder is not
/// the one held open (the shared namespace's check, [`Prepared::directory_argv`]).
pub const FOLDER_CHANGED_STATUS: i32 = 126;

/// Everything the child process needs, prepared before it is forked: after
/// `fork` in a multithreaded program only async-signal-safe calls are
/// allowed, so the child itself does nothing but system calls.
///
/// Start the command as [`argv`](Self::argv) says and call
/// [`enter`](Self::enter) in its `pre_exec`; a command that skipped `argv`
/// where it matters fails to start rather than run unconfined.
pub struct Prepared {
    #[cfg(target_os = "linux")]
    inner: imp::Mode,
    wrapped: std::sync::atomic::AtomicBool,
    #[cfg(target_os = "linux")]
    trusted_env: std::sync::atomic::AtomicBool,
    /// Start metadata only: host fd is closed before nsenter, never handed
    /// to the launcher; -1 means no checked argv has been prepared.
    #[cfg(target_os = "linux")]
    pinned: std::sync::atomic::AtomicI32,
}

impl Prepared {
    pub fn new(spec: &Spec, cwd: &Path) -> std::io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let s = support();
            let inner = if s.shared() && spec.writable.is_none() {
                imp::Mode::Joined(imp::join_args(spec, cwd)?)
            } else {
                imp::Mode::Direct(Box::new(imp::Plan::new(spec, cwd, s.landlock)?))
            };
            Ok(Self {
                inner,
                wrapped: Default::default(),
                trusted_env: Default::default(),
                pinned: std::sync::atomic::AtomicI32::new(-1),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (spec, cwd);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "sandboxing is only implemented on Linux",
            ))
        }
    }

    /// The program to start for `program args`: the same, or `nsenter`
    /// joining the shared process namespace first.
    pub fn argv(&self, program: OsString, args: Vec<OsString>) -> (OsString, Vec<OsString>) {
        self.wrapped
            .store(true, std::sync::atomic::Ordering::Relaxed);
        #[cfg(target_os = "linux")]
        if let imp::Mode::Joined(joined) = &self.inner {
            return joined.argv(program, args, None);
        }
        (program, args)
    }

    /// A held folder checked by identity inside the namespace. Only device
    /// and inode values go to the native launcher; no host directory fd
    /// crosses exec or becomes visible to other sandboxed commands.
    #[cfg(unix)]
    pub fn directory_argv(
        &self,
        program: OsString,
        args: Vec<OsString>,
        pinned: i32,
        identity: (u64, u64),
    ) -> (OsString, Vec<OsString>) {
        #[cfg(target_os = "linux")]
        if let imp::Mode::Joined(joined) = &self.inner {
            self.wrapped
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.pinned
                .store(pinned, std::sync::atomic::Ordering::Relaxed);
            return joined.argv(program, args, Some(identity));
        }
        let _ = (pinned, identity);
        self.argv(program, args)
    }

    /// Install after all requested-program env additions, before fork/exec.
    /// The shared trusted loaders get fixed settings only; program values
    /// cross in a sealed memfd, never argv. Direct mode needs no wrapper.
    pub fn protect_environment(&self, cmd: &mut std::process::Command) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        if matches!(self.inner, imp::Mode::Joined(_)) {
            environment::protect(cmd)?;
            self.trusted_env
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        let _ = cmd;
        Ok(())
    }
    /// Cleanup outside a joined PID namespace can only see/retain visible edges.
    pub(crate) fn joins_pid_namespace(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            matches!(self.inner, imp::Mode::Joined(_))
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    /// Direct mode checks after mounting; shared mode closes the host fd and
    /// leaves the identity comparison to the native launcher inside.
    #[cfg(unix)]
    pub fn directory_entry(&self) -> DirectoryEntry {
        #[cfg(target_os = "linux")]
        {
            match self.inner {
                imp::Mode::Direct(_) => DirectoryEntry::Reopen,
                imp::Mode::Joined(_) => {
                    DirectoryEntry::Values(self.pinned.load(std::sync::atomic::Ordering::Relaxed))
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            DirectoryEntry::Reopen
        }
    }

    /// Enter the sandbox. Call only in the child, between fork and exec.
    ///
    /// # Safety
    /// Must run in a freshly forked, single-threaded child.
    pub unsafe fn enter(&self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            match &self.inner {
                imp::Mode::Direct(plan) => unsafe { plan.enter() },
                imp::Mode::Joined(_)
                    if self.wrapped.load(std::sync::atomic::Ordering::Relaxed)
                        && self.trusted_env.load(std::sync::atomic::Ordering::Relaxed) =>
                {
                    Ok(())
                }
                imp::Mode::Joined(_) => Err(std::io::Error::other(
                    "the sandbox needs Prepared::argv and a protected launcher environment",
                )),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::io::{Error, Result};
    use std::os::unix::ffi::OsStrExt;

    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;
    const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    /// Removing and making entries of every kind (directories, files,
    /// symlinks, sockets, pipes, devices).
    const LANDLOCK_ACCESS_FS_CHANGE: u64 = 0b1_1111_1111 << 4;
    const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
    const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
    const LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1;
    const MOUNT_ATTR_RDONLY: u64 = 1;

    /// The kernel reads only as many fields as the size we pass, so older
    /// ABIs get the first one alone.
    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    #[repr(C)]
    struct MountAttr {
        attr_set: u64,
        attr_clr: u64,
        propagation: u64,
        userns_fd: u64,
    }

    fn cstr(p: &Path) -> Result<CString> {
        CString::new(p.as_os_str().as_bytes()).map_err(|_| Error::other("path contains a NUL byte"))
    }

    pub enum Mode {
        Direct(Box<Plan>),
        Joined(Joined),
    }

    /// A launch into a shared namespace: nsenter then a native launcher
    /// which enters the working folder inside the namespace.
    pub struct Joined {
        pub(super) nsenter: Vec<OsString>,
        pub(super) cwd: OsString,
    }

    impl Joined {
        /// With a held identity use the native entry point. Ordinary
        /// unbound launches retain their existing entry-by-name behaviour.
        pub(super) fn argv(
            &self,
            program: OsString,
            args: Vec<OsString>,
            held: Option<(u64, u64)>,
        ) -> (OsString, Vec<OsString>) {
            let mut all = self.nsenter[1..].to_vec();
            let (device, inode) = held
                .map(|(d, i)| (d.to_string(), i.to_string()))
                .unwrap_or_else(|| ("-".into(), "-".into()));
            all.extend([
                directory_launcher().into_os_string(),
                launcher::FLAG.into(),
                self.cwd.clone(),
                device.into(),
                inode.into(),
                "--".into(),
            ]);
            all.push(program);
            all.extend(args);
            (self.nsenter[0].clone(), all)
        }
    }

    struct Keep {
        source: CString,
        /// Directories to create inside the tmpfs, outermost first; for a
        /// directory the last one is the mount point.
        mkdirs: Vec<CString>,
        mountpoint: CString,
        /// A single file (socket): the mount point is an empty file.
        file: bool,
        /// Flags for the read-only remount (must repeat the locked flags of
        /// the underlying mount).
        ro_flags: libc::c_ulong,
    }

    struct Hide {
        target: CString,
        keeps: Vec<Keep>,
    }

    /// Single files that read as empty (`/dev/null` bound over them).
    fn hidden_files(spec: &Spec, dirs: &[PathBuf], keeps: &[PathBuf]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = spec
            .hide
            .iter()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .filter(|p| !dirs.iter().any(|d| p.starts_with(d)) && !keeps.contains(p))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    struct Folder {
        path: CString,
        write: bool,
        /// A single file (read-only) rather than a directory.
        file: bool,
        ro_flags: libc::c_ulong,
    }

    /// The mounts every sandboxed process sees, prepared up front.
    struct Mounts {
        /// Directories holding something hidden or read-only, each mounted
        /// on itself so it can't be moved or removed (shortest first).
        pins: Vec<CString>,
        /// Shortest path first, so nested rules land on top.
        folders: Vec<Folder>,
        hides: Vec<Hide>,
        files: Vec<CString>,
        null: CString,
        tmpfs: CString,
        tmpfs_opts: CString,
        root: CString,
        proc_: CString,
    }

    pub struct Plan {
        uid_map: Vec<u8>,
        gid_map: Vec<u8>,
        mounts: Mounts,
        cwd: CString,
        landlock_abi: i32,
        /// [`Spec::writable`]: the only directories (with `/dev/shm`) where
        /// files may change, and single files (`false`) that may; `None` =
        /// no such limit.
        confine: Option<Vec<(CString, bool)>>,
        dev: CString,
        setgroups: CString,
        uid_map_file: CString,
        gid_map_file: CString,
    }

    fn remount_flags(path: &Path) -> libc::c_ulong {
        let mut flags = (libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY) as libc::c_ulong;
        let Ok(c) = cstr(path) else { return flags };
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0 {
            let f = st.f_flag;
            for (st_flag, ms_flag) in [
                (libc::ST_NOSUID, libc::MS_NOSUID),
                (libc::ST_NODEV, libc::MS_NODEV),
                (libc::ST_NOEXEC, libc::MS_NOEXEC),
                (libc::ST_NOATIME, libc::MS_NOATIME),
                (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
                (libc::ST_RELATIME, libc::MS_RELATIME),
            ] {
                if f & st_flag != 0 {
                    flags |= ms_flag as libc::c_ulong;
                }
            }
        }
        flags
    }

    /// The directories above `targets` that a command could move away
    /// (their parent is writable): `mv ~/.config ~/.c` would take a hidden
    /// or read-only folder inside along, out of its place, and leave room
    /// for another one there (a made-up `~/.config/tessera`, a changed
    /// service file) that runs or is read outside the sandbox later. A
    /// directory something is mounted on can't be moved or removed, so each
    /// of these is mounted on itself. Shortest first.
    pub(super) fn pins(targets: &[PathBuf]) -> Vec<PathBuf> {
        let writable =
            |d: &Path| cstr(d).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0);
        let mut out: Vec<PathBuf> = Vec::new();
        for t in targets {
            let mut at = t.parent();
            while let Some(dir) = at {
                let Some(parent) = dir.parent() else { break };
                if writable(parent) && !out.iter().any(|o| o == dir) {
                    out.push(dir.to_path_buf());
                }
                at = Some(parent);
            }
        }
        out.sort_by_key(|p| p.components().count());
        out
    }

    /// Outermost existing directories only (hiding `/a` already hides `/a/b`).
    fn normalize_hides(spec: &Spec) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = spec
            .hide
            .iter()
            .filter(|p| p.is_dir())
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        dirs.sort();
        dirs.dedup();
        let mut out: Vec<PathBuf> = Vec::new();
        for d in dirs {
            if !out.iter().any(|o| d.starts_with(o)) {
                out.push(d);
            }
        }
        out
    }

    impl Mounts {
        fn new(spec: &Spec) -> Result<Self> {
            let keeps: Vec<PathBuf> = spec
                .keep
                .iter()
                .filter(|p| p.exists())
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .collect();
            let hidden = normalize_hides(spec);
            let mut hides = Vec::new();
            for target in &hidden {
                let mut hide = Hide {
                    target: cstr(target)?,
                    keeps: Vec::new(),
                };
                for k in keeps
                    .iter()
                    .filter(|k| k.starts_with(target) && *k != target)
                {
                    let file = !k.is_dir();
                    let rel = k.strip_prefix(target).unwrap_or(k);
                    let mut mkdirs = Vec::new();
                    let mut at = target.clone();
                    let n = rel.components().count();
                    for comp in rel.components().take(if file { n - 1 } else { n }) {
                        at.push(comp);
                        mkdirs.push(cstr(&at)?);
                    }
                    hide.keeps.push(Keep {
                        source: cstr(k)?,
                        mountpoint: cstr(k)?,
                        mkdirs,
                        file,
                        ro_flags: remount_flags(k),
                    });
                }
                hides.push(hide);
            }
            // Folder rules: only directories (and read-only single files)
            // outside the hidden ones, and only writable ones that sit
            // inside a read-only one.
            let mut rules: Vec<(PathBuf, bool)> = spec
                .folders
                .iter()
                .filter(|(p, _)| p.is_dir() || p.is_file())
                .map(|(p, w)| (p.canonicalize().unwrap_or_else(|_| p.clone()), *w))
                .filter(|(p, _)| !hidden.iter().any(|h| p.starts_with(h)))
                .collect();
            rules.sort_by_key(|(p, _)| p.components().count());
            let mut folders = Vec::new();
            let mut targets = hidden.clone();
            for (i, (p, write)) in rules.iter().enumerate() {
                let inside_ro = rules[..i].iter().any(|(q, w)| !*w && p.starts_with(q));
                if *write && !inside_ro {
                    continue;
                }
                folders.push(Folder {
                    path: cstr(p)?,
                    write: *write,
                    file: !p.is_dir(),
                    ro_flags: remount_flags(p),
                });
                targets.push(p.clone());
            }
            let files = hidden_files(spec, &hidden, &keeps);
            targets.extend(files.iter().cloned());
            let files = files.iter().map(|f| cstr(f)).collect::<Result<Vec<_>>>()?;
            Ok(Self {
                pins: pins(&targets)
                    .iter()
                    .map(|p| cstr(p))
                    .collect::<Result<_>>()?,
                folders,
                hides,
                files,
                null: CString::new("/dev/null").unwrap(),
                tmpfs: CString::new("tmpfs").unwrap(),
                tmpfs_opts: CString::new("size=64k,nr_inodes=256,mode=0755").unwrap(),
                root: CString::new("/").unwrap(),
                proc_: CString::new("proc").unwrap(),
            })
        }

        /// Set up the view, in a mount namespace we own.
        unsafe fn apply(&self) -> Result<()> {
            unsafe {
                // Nothing we do here may propagate back to the real system.
                check(libc::mount(
                    std::ptr::null(),
                    self.root.as_ptr(),
                    std::ptr::null(),
                    (libc::MS_REC | libc::MS_PRIVATE) as libc::c_ulong,
                    std::ptr::null(),
                ))?;
                // First the directories holding what follows, each on
                // itself (with what is mounted inside it), so none can be
                // moved away with what it holds.
                for p in &self.pins {
                    check(libc::mount(
                        p.as_ptr(),
                        p.as_ptr(),
                        std::ptr::null(),
                        (libc::MS_BIND | libc::MS_REC) as libc::c_ulong,
                        std::ptr::null(),
                    ))?;
                }
                // Open what we mount again before anything covers it (in the
                // new namespace: bind mounts can't cross namespaces).
                let mut fds = [-1i32; 1024];
                let mut n = 0;
                let keeps = self.hides.iter().flat_map(|h| &h.keeps);
                for (source, dir) in self
                    .folders
                    .iter()
                    .map(|f| (&f.path, !f.file))
                    .chain(keeps.map(|k| (&k.source, !k.file)))
                {
                    if n == fds.len() {
                        return Err(Error::other("too many sandbox mounts"));
                    }
                    let dir = if dir { libc::O_DIRECTORY } else { 0 };
                    fds[n] = check(libc::open(
                        source.as_ptr(),
                        libc::O_PATH | dir | libc::O_CLOEXEC,
                    ))?;
                    n += 1;
                }
                let mut i = 0;
                // Folder rules first: the hides must end up on top of them.
                for f in &self.folders {
                    if i == n {
                        break;
                    }
                    bind_fd(fds[i], &f.path)?;
                    libc::close(fds[i]);
                    i += 1;
                    if !f.write {
                        read_only(&f.path, f.ro_flags)?;
                    }
                }
                for h in &self.hides {
                    let flags =
                        (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as libc::c_ulong;
                    check(libc::mount(
                        self.tmpfs.as_ptr(),
                        h.target.as_ptr(),
                        self.tmpfs.as_ptr(),
                        flags,
                        self.tmpfs_opts.as_ptr().cast(),
                    ))?;
                    for k in &h.keeps {
                        if i == n {
                            break;
                        }
                        for d in &k.mkdirs {
                            let r = libc::mkdir(d.as_ptr(), 0o755);
                            if r != 0 && *libc::__errno_location() != libc::EEXIST {
                                return Err(Error::last_os_error());
                            }
                        }
                        if k.file {
                            libc::close(check(libc::open(
                                k.mountpoint.as_ptr(),
                                libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                                0o600,
                            ))?);
                        }
                        bind_fd(fds[i], &k.mountpoint)?;
                        // Best effort: if the underlying mount has flags we
                        // can't repeat, the kept directory stays writable.
                        libc::mount(
                            std::ptr::null(),
                            k.mountpoint.as_ptr(),
                            std::ptr::null(),
                            k.ro_flags,
                            std::ptr::null(),
                        );
                        libc::close(fds[i]);
                        i += 1;
                    }
                    let ro = (libc::MS_REMOUNT
                        | libc::MS_RDONLY
                        | libc::MS_NOSUID
                        | libc::MS_NODEV
                        | libc::MS_NOEXEC) as libc::c_ulong;
                    check(libc::mount(
                        std::ptr::null(),
                        h.target.as_ptr(),
                        std::ptr::null(),
                        ro,
                        std::ptr::null(),
                    ))?;
                }
                for f in &self.files {
                    check(libc::mount(
                        self.null.as_ptr(),
                        f.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND as libc::c_ulong,
                        std::ptr::null(),
                    ))?;
                }
                Ok(())
            }
        }
    }

    /// Bind what `fd` (an `O_PATH` descriptor) refers to onto `target`.
    unsafe fn bind_fd(fd: i32, target: &CString) -> Result<()> {
        let mut src = [0u8; 32];
        fd_path(fd, &mut src);
        unsafe {
            check(libc::mount(
                src.as_ptr().cast(),
                target.as_ptr(),
                std::ptr::null(),
                (libc::MS_BIND | libc::MS_REC) as libc::c_ulong,
                std::ptr::null(),
            ))?;
        }
        Ok(())
    }

    /// Make a mount and everything under it read-only (Linux 5.12+), or at
    /// least the mount itself.
    unsafe fn read_only(path: &CString, flags: libc::c_ulong) -> Result<()> {
        let attr = MountAttr {
            attr_set: MOUNT_ATTR_RDONLY,
            attr_clr: 0,
            propagation: 0,
            userns_fd: 0,
        };
        let r = unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                libc::AT_FDCWD,
                path.as_ptr(),
                libc::AT_RECURSIVE as libc::c_uint,
                &attr as *const MountAttr,
                std::mem::size_of::<MountAttr>(),
            )
        };
        if r == 0 {
            return Ok(());
        }
        unsafe {
            check(libc::mount(
                std::ptr::null(),
                path.as_ptr(),
                std::ptr::null(),
                flags,
                std::ptr::null(),
            ))?;
        }
        Ok(())
    }

    fn id_maps() -> (Vec<u8>, Vec<u8>) {
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        (
            format!("{uid} {uid} 1\n").into_bytes(),
            format!("{gid} {gid} 1\n").into_bytes(),
        )
    }

    impl Plan {
        pub fn new(spec: &Spec, cwd: &Path, landlock_abi: i32) -> Result<Self> {
            let (uid_map, gid_map) = id_maps();
            let confine = match &spec.writable {
                None => None,
                Some(dirs) => {
                    let mut v = Vec::new();
                    for d in dirs
                        .iter()
                        .map(|p| p.as_path())
                        .chain([Path::new("/dev/shm")])
                    {
                        if d.is_dir() {
                            let real = d.canonicalize().unwrap_or_else(|_| d.to_path_buf());
                            v.push((cstr(&real)?, true));
                        } else if std::fs::symlink_metadata(d).is_ok_and(|m| m.is_file()) {
                            // A single file, never through a link.
                            v.push((cstr(d)?, false));
                        }
                    }
                    Some(v)
                }
            };
            Ok(Self {
                uid_map,
                gid_map,
                mounts: Mounts::new(spec)?,
                cwd: cstr(cwd)?,
                landlock_abi,
                confine,
                dev: CString::new("/dev").unwrap(),
                setgroups: CString::new("/proc/self/setgroups").unwrap(),
                uid_map_file: CString::new("/proc/self/uid_map").unwrap(),
                gid_map_file: CString::new("/proc/self/gid_map").unwrap(),
            })
        }

        /// New user and mount namespaces (plus `extra` flags) with our own
        /// ids mapped and the view set up.
        unsafe fn namespaces(&self, extra: libc::c_int) -> Result<()> {
            unsafe {
                // A non-dumpable parent (see `harden_process`) leaves our /proc
                // files owned by root, and we need to write our uid map.
                libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0);
                check(libc::unshare(
                    libc::CLONE_NEWUSER | libc::CLONE_NEWNS | extra,
                ))?;
                write_file(&self.setgroups, b"deny")?;
                write_file(&self.uid_map_file, &self.uid_map)?;
                write_file(&self.gid_map_file, &self.gid_map)?;
                self.mounts.apply()
            }
        }

        pub unsafe fn enter(&self) -> Result<()> {
            unsafe {
                self.namespaces(0)?;
                // The working directory was entered before the mounts; enter it
                // again so a cwd inside a hidden directory sees the empty view.
                if libc::chdir(self.cwd.as_ptr()) != 0 {
                    libc::chdir(self.mounts.root.as_ptr());
                }
                if self.confine.is_some() && self.landlock_abi < 3 {
                    return Err(Error::other("confining writes needs Landlock ABI 3"));
                }
                if self.landlock_abi >= 2 {
                    let truncate = if self.landlock_abi >= 3 {
                        LANDLOCK_ACCESS_FS_TRUNCATE
                    } else {
                        0
                    };
                    let writes = LANDLOCK_ACCESS_FS_WRITE_FILE
                        | LANDLOCK_ACCESS_FS_CHANGE
                        | LANDLOCK_ACCESS_FS_REFER
                        | truncate;
                    let handled = if self.confine.is_some() {
                        writes
                    } else {
                        LANDLOCK_ACCESS_FS_REFER
                    };
                    let attr = RulesetAttr {
                        handled_access_fs: handled,
                        handled_access_net: 0,
                        // Signals stay unscoped: each command gets its own
                        // domain, and `kill` of an earlier command's server
                        // must keep working.
                        scoped: LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET,
                    };
                    let size = if self.landlock_abi >= 6 {
                        std::mem::size_of::<RulesetAttr>()
                    } else {
                        std::mem::size_of::<u64>()
                    };
                    let ruleset = check(libc::syscall(
                        libc::SYS_landlock_create_ruleset,
                        &attr as *const RulesetAttr,
                        size,
                        0u32,
                    ) as i32)?;
                    match &self.confine {
                        // Renames and links stay allowed everywhere; the
                        // domain exists for its process isolation and mount
                        // lock.
                        None => {
                            allow_beneath(ruleset, &self.mounts.root, LANDLOCK_ACCESS_FS_REFER)?
                        }
                        // Files change only beneath the chosen directories
                        // (links and moves too, so nothing from elsewhere
                        // can be linked in); devices can still be written.
                        Some(dirs) => {
                            for (d, dir) in dirs {
                                // A path removed since it was checked
                                // simply gets no access.
                                let _ = if *dir {
                                    allow_beneath(ruleset, d, writes)
                                } else {
                                    allow_file(ruleset, d, LANDLOCK_ACCESS_FS_WRITE_FILE | truncate)
                                };
                            }
                            let _ = allow_beneath(
                                ruleset,
                                &self.dev,
                                LANDLOCK_ACCESS_FS_WRITE_FILE | truncate,
                            );
                        }
                    }
                    check(libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) as i32)?;
                    libc::close(ruleset);
                }
                Ok(())
            }
        }

        /// The shared namespace's two processes: this one (outside it) and
        /// its child, PID 1 inside, with a fresh `/proc` for everyone who
        /// joins. Both then run [`INIT_SCRIPT`].
        unsafe fn enter_init(&self) -> Result<()> {
            unsafe {
                self.namespaces(libc::CLONE_NEWPID)?;
                match libc::fork() {
                    -1 => Err(Error::last_os_error()),
                    0 => {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                        let flags =
                            (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as libc::c_ulong;
                        let p = &self.mounts.proc_;
                        // Never report an error from here: our parent is
                        // already running and the spawner would wait for it.
                        // Without PID 1 the namespace is gone and the probe
                        // (or the next command) notices.
                        if libc::mount(
                            p.as_ptr(),
                            c"/proc".as_ptr(),
                            p.as_ptr(),
                            flags,
                            std::ptr::null(),
                        ) != 0
                        {
                            libc::_exit(1);
                        }
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
        }
    }

    /// PID 1 reaps orphans (a shell waiting on `sleep` reaps any child);
    /// the outer process lives until the server closes its stdin (or dies),
    /// and PID 1 and everything in the namespace go with it.
    const INIT_SCRIPT: &str = "if [ $$ = 1 ]; then while :; do sleep 3600; done; else read _; fi";

    struct Init {
        spec: Spec,
        child: std::process::Child,
    }

    impl Init {
        fn start(spec: &Spec) -> Result<Self> {
            use std::os::unix::process::CommandExt;
            // The launcher is trusted code, not another writable command
            // artifact. Bind its executable read-only and pin its parents in
            // this view even when the surrounding working folder is writable.
            let mut entry_spec = spec.clone();
            entry_spec.folders.push((directory_launcher(), false));
            let plan = Plan::new(&entry_spec, Path::new("/"), 0)?;
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.arg("-c")
                .arg(INIT_SCRIPT)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .current_dir("/")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            unsafe {
                cmd.pre_exec(move || {
                    libc::setsid();
                    plan.enter_init()
                })
            };
            // Returns once both have started (or PID 1 gave up).
            let mut init = Self {
                spec: spec.clone(),
                child: cmd.spawn()?,
            };
            if !init.alive() {
                init.stop();
                return Err(Error::other("the process namespace did not start"));
            }
            Ok(init)
        }

        /// The outer process runs and PID 1 is still there.
        fn alive(&mut self) -> bool {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return false;
            }
            let pid = self.child.id();
            match std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
                Ok(children) => !children.trim().is_empty(),
                // Kernels without the file: trust the outer process.
                Err(_) => true,
            }
        }

        /// How many processes run in the namespace (PID 1 and its `sleep`
        /// when nothing else does); many when that can't be told.
        fn running(&self) -> usize {
            let outer = self.child.id();
            let pid1 = read(&format!("/proc/{outer}/task/{outer}/children"));
            let Some(ns) = pid1
                .split_whitespace()
                .next()
                .and_then(|p| std::fs::read_link(format!("/proc/{p}/ns/pid")).ok())
            else {
                return usize::MAX;
            };
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return usize::MAX;
            };
            entries
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                })
                .filter(|e| std::fs::read_link(e.path().join("ns/pid")).is_ok_and(|l| l == ns))
                .filter(|e| {
                    !e.file_name()
                        .to_str()
                        .and_then(|s| s.parse().ok())
                        .is_some_and(supervisor_auxiliary)
                })
                .count()
        }

        /// `nsenter` up to the program. The working directory is entered
        /// inside by the native launcher ([`Joined::argv`]), which remains
        /// outside the PID namespace until entry is complete and then spawns
        /// the program. `--wd` would open cwd before joining and bypass mounts.
        fn args(&self, nsenter: &Path, cwd: &Path) -> Joined {
            let ns = |name: &str| format!("/proc/{}/ns/{name}", self.child.id());
            Joined {
                nsenter: vec![
                    nsenter.as_os_str().to_owned(),
                    format!("--user={}", ns("user")).into(),
                    format!("--mount={}", ns("mnt")).into(),
                    format!("--pid={}", ns("pid_for_children")).into(),
                    "--preserve-credentials".into(),
                    "--no-fork".into(),
                    "--".into(),
                ],
                cwd: cwd.as_os_str().to_owned(),
            }
        }

        fn stop(mut self) {
            drop(self.child.stdin.take());
            for _ in 0..40 {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct Shared {
        /// Each group's namespace, and when a command last joined it.
        groups: std::collections::HashMap<String, (Init, std::time::Instant)>,
        /// Earlier namespaces (the settings changed) whose commands and
        /// background servers may still run; they end with the server.
        retired: Vec<Init>,
        swept: Option<std::time::Instant>,
    }

    /// A group's namespace with nothing running in it is stopped once no
    /// command has joined it for this long (there's nothing left to see).
    const IDLE_GROUP: std::time::Duration = std::time::Duration::from_secs(600);

    impl Shared {
        /// Stop other groups' namespaces that sat idle (at most once a
        /// minute: it looks through `/proc`).
        fn sweep(&mut self, keep: &str) {
            let now = std::time::Instant::now();
            if self
                .swept
                .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(60))
            {
                return;
            }
            self.swept = Some(now);
            let stale: Vec<String> = self
                .groups
                .iter()
                .filter(|(g, (init, used))| {
                    g.as_str() != keep
                        && now.duration_since(*used) > IDLE_GROUP
                        && init.running() <= 2
                })
                .map(|(g, _)| g.clone())
                .collect();
            for g in stale {
                if let Some((init, _)) = self.groups.remove(&g) {
                    init.stop();
                }
            }
        }
    }

    fn shared() -> &'static parking_lot::Mutex<Shared> {
        static S: std::sync::OnceLock<parking_lot::Mutex<Shared>> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            parking_lot::Mutex::new(Shared {
                groups: Default::default(),
                retired: Vec::new(),
                swept: None,
            })
        })
    }

    fn find_nsenter() -> Option<PathBuf> {
        [
            "/usr/bin/nsenter",
            "/bin/nsenter",
            "/usr/sbin/nsenter",
            "/sbin/nsenter",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
    }

    /// `nsenter` arguments that run a program in the shared namespace of
    /// `spec.group`, starting it (again) when needed.
    pub fn join_args(spec: &Spec, cwd: &Path) -> Result<Joined> {
        let nsenter = find_nsenter().ok_or_else(|| Error::other("nsenter is missing"))?;
        let mut s = shared().lock();
        let s = &mut *s;
        let fresh = match s.groups.get_mut(&spec.group) {
            Some((i, _)) => i.spec != *spec || !i.alive(),
            None => true,
        };
        if fresh {
            s.retired
                .extend(s.groups.remove(&spec.group).map(|(i, _)| i));
            for mut old in std::mem::take(&mut s.retired) {
                if old.alive() {
                    s.retired.push(old);
                } else {
                    old.stop();
                }
            }
            s.groups.insert(
                spec.group.clone(),
                (Init::start(spec)?, std::time::Instant::now()),
            );
        }
        s.sweep(&spec.group);
        let (init, used) = s.groups.get_mut(&spec.group).expect("started");
        *used = std::time::Instant::now();
        Ok(init.args(&nsenter, cwd))
    }

    /// Grant `access` beneath the directory `dir` in a Landlock ruleset.
    unsafe fn allow_beneath(ruleset: i32, dir: &CString, access: u64) -> Result<()> {
        unsafe {
            let fd = check(libc::open(
                dir.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            ))?;
            let rule = PathBeneathAttr {
                allowed_access: access,
                parent_fd: fd,
            };
            let r = libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset,
                LANDLOCK_RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0u32,
            ) as i32;
            libc::close(fd);
            check(r).map(|_| ())
        }
    }

    /// Allow `access` (file rights only) on the single file `path`.
    unsafe fn allow_file(ruleset: i32, path: &CString, access: u64) -> Result<()> {
        unsafe {
            let fd = check(libc::open(
                path.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            ))?;
            let rule = PathBeneathAttr {
                allowed_access: access,
                parent_fd: fd,
            };
            let r = libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset,
                LANDLOCK_RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0u32,
            ) as i32;
            libc::close(fd);
            check(r).map(|_| ())
        }
    }

    fn check(r: i32) -> Result<i32> {
        if r < 0 {
            Err(Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    unsafe fn write_file(path: &CString, data: &[u8]) -> Result<()> {
        unsafe {
            let fd = check(libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC))?;
            let w = libc::write(fd, data.as_ptr().cast(), data.len());
            libc::close(fd);
            if w != data.len() as isize {
                return Err(Error::last_os_error());
            }
            Ok(())
        }
    }

    /// `/proc/self/fd/<fd>` into a NUL-terminated stack buffer (no allocation).
    fn fd_path(fd: i32, out: &mut [u8; 32]) {
        let prefix = b"/proc/self/fd/";
        out[..prefix.len()].copy_from_slice(prefix);
        let mut digits = [0u8; 10];
        let mut n = fd.max(0) as u32;
        let mut len = 0;
        loop {
            digits[len] = b'0' + (n % 10) as u8;
            len += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for j in 0..len {
            out[prefix.len() + j] = digits[len - 1 - j];
        }
        out[prefix.len() + len] = 0;
    }

    fn probe_dir() -> Result<(PathBuf, Spec)> {
        let tmp = std::env::temp_dir().join(format!(
            "tessera-sandbox-probe-{}-{}",
            std::process::id(),
            crate::util::random_token(6)
        ));
        let keep = tmp.join("keep");
        std::fs::create_dir_all(&keep)?;
        std::fs::write(tmp.join("secret"), b"x")?;
        let spec = Spec {
            hide: vec![tmp.clone()],
            keep: vec![keep],
            folders: Vec::new(),
            writable: None,
            group: String::new(),
        };
        Ok((tmp, spec))
    }

    /// Inside: the secret must be gone and the kept directory present.
    fn probe_script(tmp: &Path) -> String {
        format!(
            "test ! -e '{}' && test -d '{}'",
            tmp.join("secret").display(),
            tmp.join("keep").display()
        )
    }

    pub fn probe(landlock_cap: Option<i32>) -> Support {
        let landlock = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<u8>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        } as i32;
        let landlock = landlock.max(0).min(landlock_cap.unwrap_or(i32::MAX));
        let fail = |reason: String| Support {
            available: false,
            landlock,
            private_pids: false,
            reason: Some(reason),
        };
        let Ok((tmp, spec)) = probe_dir() else {
            return fail("could not create a probe directory".into());
        };
        let result = (|| -> std::result::Result<(), String> {
            use std::os::unix::process::CommandExt;
            let plan = Plan::new(&spec, Path::new("/"), landlock).map_err(|e| e.to_string())?;
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.arg("-c").arg(probe_script(&tmp));
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            unsafe { cmd.pre_exec(move || plan.enter()) };
            match cmd.status() {
                Ok(s) if s.success() => Ok(()),
                Ok(s) => Err(format!("probe exited with {s}")),
                Err(e) => Err(e.to_string()),
            }
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        let private_pids = result.is_ok() && landlock < 2 && shared_works().is_ok();
        match result {
            Ok(()) => Support {
                available: true,
                landlock,
                private_pids,
                reason: None,
            },
            Err(e) => fail(format!("user namespaces are not available: {e}")),
        }
    }

    /// A command joined to a shared namespace sees the view and only its
    /// own processes.
    pub(super) fn shared_works() -> std::result::Result<(), String> {
        let (tmp, spec) = probe_dir().map_err(|e| e.to_string())?;
        let r = probe_shared(&tmp, &spec);
        let _ = std::fs::remove_dir_all(&tmp);
        r
    }

    fn probe_shared(tmp: &Path, spec: &Spec) -> std::result::Result<(), String> {
        let nsenter = find_nsenter().ok_or("nsenter is missing")?;
        let init = Init::start(spec).map_err(|e| e.to_string())?;
        let (program, args) = init.args(&nsenter, Path::new("/")).argv(
            "/bin/sh".into(),
            vec![
                "-c".into(),
                format!(
                    "{} && test \"$(cat /proc/1/comm)\" = sh && echo $$",
                    probe_script(tmp)
                )
                .into(),
            ],
            None,
        );
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        environment::protect(&mut cmd).map_err(|e| e.to_string())?;
        let out = cmd.output();
        init.stop();
        let out = out.map_err(|e| e.to_string())?;
        let pid: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(|_| format!("probe exited with {}", out.status))?;
        // A fresh namespace numbers from 1; the host's are far beyond.
        if !out.status.success() || pid > 50 {
            return Err(format!("no private process view (pid {pid})"));
        }
        Ok(())
    }

    fn read(path: &str) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// `VERSION_ID` of an Ubuntu release, if this is one.
    fn ubuntu() -> Option<String> {
        let text = read("/etc/os-release");
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .map(|v| v.trim_matches('"').to_string())
        };
        (field("ID").as_deref() == Some("ubuntu"))
            .then(|| field("VERSION_ID"))
            .flatten()
    }

    pub fn hint(s: &Support) -> String {
        let ubuntu = ubuntu();
        if !s.available {
            if read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") == "1" {
                let exe = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "/usr/local/bin/tessera".into());
                return format!(
                    "This system's AppArmor blocks the user namespaces the sandbox needs. Allow them for Tessera, then restart it: `printf 'abi <abi/4.0>,\\ninclude <tunables/global>\\nprofile tessera {exe} flags=(unconfined) {{\\n  userns,\\n}}\\n' | sudo tee /etc/apparmor.d/tessera && sudo systemctl reload apparmor`"
                );
            }
            if read("/proc/sys/kernel/unprivileged_userns_clone") == "0" {
                return "Unprivileged user namespaces are turned off. Turn them on, then restart Tessera: `echo kernel.unprivileged_userns_clone=1 | sudo tee /etc/sysctl.d/90-tessera.conf && sudo sysctl --system`".into();
            }
            if read("/proc/sys/user/max_user_namespaces") == "0" {
                return "User namespaces are limited to 0. Allow them, then restart Tessera: `echo user.max_user_namespaces=15000 | sudo tee /etc/sysctl.d/90-tessera.conf && sudo sysctl --system`".into();
            }
            return "The sandbox needs unprivileged user namespaces, which this system (or container) doesn't allow. Run Tessera directly on a Linux host with a recent kernel.".into();
        }
        // Namespaces work, but neither Landlock ABI 2 nor a private
        // process view does.
        if find_nsenter().is_none() {
            return "Install util-linux (it provides `nsenter`), then restart Tessera; or use a kernel with Landlock (Linux 5.19+).".into();
        }
        match ubuntu.as_deref() {
            Some("22.04") => "Install Ubuntu's newer kernel, which has Landlock, then reboot: `sudo apt install linux-generic-hwe-22.04`".into(),
            Some("20.04") => "Ubuntu 20.04's kernels lack the needed Landlock version: upgrade to Ubuntu 22.04 or later and install `linux-generic-hwe-22.04`.".into(),
            _ => "Use a Linux 5.19 or newer kernel with Landlock enabled (or allow mounting a private /proc, which this system refused), then restart Tessera.".into(),
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Support;
    pub fn probe(_cap: Option<i32>) -> Support {
        Support {
            available: false,
            landlock: 0,
            private_pids: false,
            reason: Some(if cfg!(windows) {
                "Tessera has no command sandbox on Windows yet".into()
            } else {
                "only implemented on Linux".into()
            }),
        }
    }
}

/// Force the real shared lane without changing the process-global support mode.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn shared_for_tests(spec: &Spec, cwd: &Path) -> Prepared {
    Prepared {
        inner: imp::Mode::Joined(
            imp::join_args(spec, cwd).expect("real shared namespace required"),
        ),
        wrapped: Default::default(),
        trusted_env: Default::default(),
        pinned: std::sync::atomic::AtomicI32::new(-1),
    }
}
/// A shared-namespace launch for tests, without starting a namespace.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn joined_for_tests(cwd: &str) -> Prepared {
    Prepared {
        inner: imp::Mode::Joined(imp::Joined {
            nsenter: vec![
                "/usr/bin/nsenter".into(),
                "--preserve-credentials".into(),
                "--".into(),
            ],
            cwd: cwd.into(),
        }),
        wrapped: Default::default(),
        trusted_env: Default::default(),
        pinned: std::sync::atomic::AtomicI32::new(-1),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn run(spec: &Spec, cwd: &Path, script: &str) -> (bool, String) {
        let plan = Prepared::new(spec, cwd).unwrap();
        let (program, args) = plan.argv("/bin/sh".into(), vec!["-c".into(), script.into()]);
        let mut cmd = std::process::Command::new(program);
        cmd.args(args).current_dir(cwd);
        plan.protect_environment(&mut cmd).unwrap();
        unsafe { cmd.pre_exec(move || plan.enter()) };
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    #[test]
    fn hides_secrets_keeps_skills_and_blocks_proc_tricks() {
        if test_mode("hides_secrets_keeps_skills_and_blocks_proc_tricks").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let skills = home.join("skills/demo");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(home.join("auth.json"), "sk-very-secret").unwrap();
        std::fs::write(skills.join("SKILL.md"), "skill body").unwrap();
        let spec = Spec {
            hide: vec![home.clone()],
            keep: vec![home.join("skills")],
            folders: Vec::new(),
            writable: None,
            group: String::new(),
        };

        // Normal work is unaffected.
        let (ok, out) = run(
            &spec,
            &work,
            "echo hi > a.txt && mkdir -p d && mv a.txt d/ && cat d/a.txt && whoami >/dev/null",
        );
        assert!(ok, "{out}");
        assert!(out.contains("hi"));

        // The secret is gone, however it is addressed, and can't be recreated.
        let secret = home.join("auth.json");
        let (_, out) = run(
            &spec,
            &work,
            &format!(
                "cat '{}'; cat \"$(dirname '{}')/auth.json\"; ls -a '{}'",
                secret.display(),
                secret.display(),
                home.display()
            ),
        );
        assert!(!out.contains("sk-very-secret"), "{out}");
        let (ok, _) = run(&spec, &work, &format!("echo x > '{}/new'", home.display()));
        assert!(!ok, "hidden directories are read-only");

        // Skills are readable but not writable.
        let (ok, out) = run(
            &spec,
            &work,
            &format!("cat '{}/SKILL.md'", skills.display()),
        );
        assert!(ok && out.contains("skill body"), "{out}");
        let (ok, _) = run(
            &spec,
            &work,
            &format!("echo x >> '{}/SKILL.md'", skills.display()),
        );
        assert!(
            !ok || std::fs::read_to_string(skills.join("SKILL.md")).unwrap() == "skill body",
            "skills are read-only"
        );

        // No way around it through processes outside the sandbox.
        if support().landlock >= 2 {
            let parent = std::process::id();
            let (_, out) = run(
                &spec,
                &work,
                &format!(
                    "cat /proc/{parent}/root{} 2>&1; tr '\\0' '\\n' < /proc/{parent}/environ 2>&1 | head -1",
                    secret.display()
                ),
            );
            assert!(!out.contains("sk-very-secret"), "{out}");
            assert!(
                out.contains("ermission denied") || out.contains("not permitted"),
                "{out}"
            );
            let (ok, _) = run(&spec, &work, &format!("umount '{}'", home.display()));
            assert!(!ok, "the masks can't be removed");
        }

        // A working directory inside a hidden tree sees the empty view.
        let (_, out) = run(&spec, &home, "cat auth.json 2>&1; pwd");
        assert!(!out.contains("sk-very-secret"), "{out}");

        // And none of this leaked onto the real system.
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "sk-very-secret");
        assert!(work.join("d/a.txt").exists());
    }

    /// Another program's credentials in a single file read as empty, and
    /// stay as they are outside.
    #[test]
    fn hidden_single_files_read_as_empty() {
        if test_mode("hidden_single_files_read_as_empty").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let netrc = home.join(".netrc");
        std::fs::write(&netrc, "machine example.com password sk-netrc").unwrap();
        std::fs::write(home.join("notes.txt"), "plain notes").unwrap();
        let spec = Spec {
            hide: vec![netrc.clone()],
            ..Default::default()
        };
        let (ok, out) = run(
            &spec,
            &home,
            "cat .netrc; echo; cat notes.txt; echo x > .netrc; true",
        );
        assert!(ok, "{out}");
        assert!(
            !out.contains("sk-netrc") && out.contains("plain notes"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(&netrc).unwrap(),
            "machine example.com password sk-netrc"
        );
    }

    #[test]
    fn read_only_folders_with_writable_ones_inside() {
        if test_mode("read_only_folders_with_writable_ones_inside").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        let drafts = docs.join("drafts");
        let hidden = docs.join("private");
        std::fs::create_dir_all(&drafts).unwrap();
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(docs.join("tax.txt"), "keep me").unwrap();
        std::fs::write(hidden.join("key"), "sk-hidden").unwrap();
        let spec = Spec {
            hide: vec![hidden.clone()],
            keep: Vec::new(),
            folders: vec![(docs.clone(), false), (drafts.clone(), true)],
            writable: None,
            group: String::new(),
        };
        let (ok, out) = run(
            &spec,
            dir.path(),
            &format!(
                "echo new > '{}/tax.txt' 2>&1 || echo refused; echo x > '{}/a.txt' && echo drafted; cat '{}/key' 2>&1; true",
                docs.display(),
                drafts.display(),
                hidden.display()
            ),
        );
        assert!(ok, "{out}");
        assert!(out.contains("refused") && out.contains("drafted"), "{out}");
        // A read-only folder still hides what is hidden inside it.
        assert!(!out.contains("sk-hidden"), "{out}");
        assert_eq!(
            std::fs::read_to_string(docs.join("tax.txt")).unwrap(),
            "keep me"
        );
        assert!(drafts.join("a.txt").exists());
    }

    /// A single file made read-only (Tessera's own program): it can't be
    /// changed, replaced, moved or deleted, while the folder it is in stays
    /// writable.
    #[test]
    fn a_read_only_single_file_stays_as_it_is() {
        if test_mode("a_read_only_single_file_stays_as_it_is").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join("tessera");
        std::fs::write(&program, "the real program").unwrap();
        let spec = Spec {
            folders: vec![(program.clone(), false)],
            ..Default::default()
        };
        let p = program.display();
        let (ok, out) = run(
            &spec,
            &bin,
            &format!(
                "cat '{p}'; echo; echo other > other && echo made-other; \
                 echo x > '{p}' 2>/dev/null && echo WROTE || echo refused-write; \
                 echo x >> '{p}' 2>/dev/null && echo APPENDED || echo refused-append; \
                 mv other '{p}' 2>/dev/null && echo REPLACED || echo refused-replace; \
                 rm -f '{p}' 2>/dev/null && test ! -e '{p}' && echo REMOVED || echo refused-remove; \
                 mv '{p}' moved 2>/dev/null && echo MOVED || echo refused-move"
            ),
        );
        assert!(ok, "{out}");
        for want in [
            "the real program",
            "made-other",
            "refused-write",
            "refused-append",
            "refused-replace",
            "refused-remove",
            "refused-move",
        ] {
            assert!(out.contains(want), "{want}: {out}");
        }
        assert_eq!(
            std::fs::read_to_string(&program).unwrap(),
            "the real program"
        );
        assert!(bin.join("other").exists() && !bin.join("moved").exists());
    }

    /// What is hidden or read-only can't be moved out of its place with
    /// the folder holding it (`mv ~/.config ~/.c`), leaving room for a
    /// made-up one there; the rest of those folders works as usual.
    #[test]
    fn hidden_and_read_only_things_stay_in_their_place() {
        if test_mode("hidden_and_read_only_things_stay_in_their_place").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let config = base.join("home/.config");
        let data = config.join("tessera");
        let bin = base.join("home/.local/bin");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(data.join("config.toml"), "real").unwrap();
        std::fs::write(bin.join("tessera"), "the real program").unwrap();
        let spec = Spec {
            hide: vec![data.clone()],
            folders: vec![(bin.join("tessera"), false)],
            ..Default::default()
        };
        let home = base.join("home");
        let (ok, out) = run(
            &spec,
            &home,
            "for d in .config .local .local/bin; do mv \"$d\" \"$d.x\" 2>/dev/null && echo MOVED-$d || echo kept-$d; done; \
             mv .config/tessera .config/t 2>/dev/null && echo MOVED-data || echo kept-data; \
             mkdir .config/other && mv .config/other .config/other2 && echo x > .local/bin/tool && echo works",
        );
        assert!(ok, "{out}");
        for want in [
            "kept-.config",
            "kept-.local",
            "kept-.local/bin",
            "kept-data",
            "works",
        ] {
            assert!(out.contains(want), "{want}: {out}");
        }
        assert!(!out.contains("MOVED"), "{out}");
        assert_eq!(
            std::fs::read_to_string(data.join("config.toml")).unwrap(),
            "real"
        );
        assert!(config.join("other2").is_dir() && bin.join("tool").is_file());
        // Only folders whose own folder can be changed are held.
        let held = imp::pins(std::slice::from_ref(&data));
        assert!(held.contains(&config) && held.contains(&home), "{held:?}");
        assert!(!held.contains(&PathBuf::from("/")), "{held:?}");
    }

    #[test]
    fn session_control_sockets_are_hidden_but_the_ssh_agent_stays() {
        if test_mode("session_control_sockets_are_hidden_but_the_ssh_agent_stays").is_none() {
            return;
        }
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(run_dir.join("systemd")).unwrap();
        let _bus = UnixListener::bind(run_dir.join("bus")).unwrap();
        let _private = UnixListener::bind(run_dir.join("systemd/private")).unwrap();
        let _agent = UnixListener::bind(run_dir.join("ssh-agent.socket")).unwrap();
        let spec = Spec {
            hide: vec![run_dir.clone()],
            keep: vec![run_dir.join("ssh-agent.socket")],
            folders: Vec::new(),
            writable: None,
            group: String::new(),
        };
        let r = run_dir.display();
        let (ok, out) = run(
            &spec,
            dir.path(),
            &format!(
                "test -S '{r}/bus' && echo bus; test -e '{r}/systemd/private' && echo systemd; test -S '{r}/ssh-agent.socket' && echo agent"
            ),
        );
        assert!(ok, "{out}");
        assert_eq!(out.trim(), "agent", "{out}");
        // The defaults cover the real session's sockets.
        let hides = session_hides();
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            assert!(hides.contains(&PathBuf::from(rt)));
        }
        assert!(hides.iter().any(|p| p.ends_with(".X11-unix")));
    }

    #[test]
    fn startup_slots_stay_read_only_through_quotes_links_and_scripts() {
        if test_mode("startup_slots_stay_read_only_through_quotes_links_and_scripts").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let work = home.join("work");
        let other = dir.path().join("other");
        for d in [&work, &other, &home.join(".config"), &home.join(".ssh")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(other.join("profile"), "original\n").unwrap();
        std::os::unix::fs::symlink(other.join("profile"), home.join(".bashrc")).unwrap();
        std::os::unix::fs::symlink(home.join(".bashrc"), work.join("profile-link")).unwrap();
        std::fs::write(home.join("ordinary.txt"), "original\n").unwrap();
        std::fs::write(
            work.join("fixture.sh"),
            "printf fixture >> \"${HOME}/.profile\"\n",
        )
        .unwrap();
        let spec = Spec {
            folders: protect_startup(
                vec![(home.clone(), true)],
                crate::node::startup_paths(Some(&home)),
            ),
            ..Default::default()
        };
        let commands = [
            r#"printf fixture >> "$HOME/.bashrc""#,
            r#"printf fixture >> "${HOME}/.profile""#,
            "printf fixture >> ~/.bash_profile",
            "printf fixture >> profile-link",
            "sh fixture.sh",
            "mkdir -p \"$HOME/.config/fish\"",
            "mkdir -p \"$HOME/.config/autostart\"",
            "mkdir -p \"$HOME/.config/systemd/user\"",
            "printf fixture > \"$HOME/.ssh/rc\"",
            "printf fixture > \"$HOME/.pam_environment\"",
            "printf fixture > \"$HOME/.xprofile\"",
            "rm \"$HOME/.bashrc\" && printf fixture > \"$HOME/.bashrc\"",
        ];
        for command in commands {
            let (ok, out) = run(
                &spec,
                &work,
                &format!("HOME='{}'; export HOME; {command}", home.display()),
            );
            assert!(!ok, "{command}: {out}");
        }
        let (ok, out) = run(
            &spec,
            &work,
            &format!(
                "printf updated > '{}'; printf project > new.txt",
                home.join("ordinary.txt").display()
            ),
        );
        assert!(ok, "safe siblings stay writable: {out}");
        assert_eq!(
            std::fs::read_to_string(other.join("profile")).unwrap(),
            "original\n"
        );
        assert!(!home.join(".profile").exists() && !home.join(".config/fish").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("ordinary.txt")).unwrap(),
            "updated"
        );
        assert!(work.join("new.txt").is_file());
        let alias = other.join("preexisting-alias");
        std::fs::hard_link(other.join("profile"), &alias).unwrap();
        let spec = Spec {
            folders: protect_startup(
                vec![(home.clone(), true)],
                crate::node::startup_paths(Some(&home)),
            ),
            ..Default::default()
        };
        let (ok, out) = run(
            &spec,
            &work,
            &format!("printf fixture > '{}'", alias.display()),
        );
        assert!(!ok, "pre-existing hard links fail closed: {out}");
        assert_eq!(std::fs::read_to_string(alias).unwrap(), "original\n");
    }

    #[test]
    fn truncate_outside_scratch_is_refused() {
        if !confines_writes() {
            eprintln!(
                "SKIP truncate_outside_scratch_is_refused: needs Landlock ABI 3 and namespaces ({})",
                describe()
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let file = dir.path().join("keep.txt");
        std::fs::write(&file, "unchanged").unwrap();
        let spec = Spec {
            writable: Some(vec![scratch.clone()]),
            ..Default::default()
        };
        // Python's os.truncate calls truncate(2) directly, without opening
        // the file for write first. O_TRUNC alone would miss the ABI-2 gap.
        let (ok, out) = run(
            &spec,
            &scratch,
            &format!(
                "python3 -c 'import os, errno\ntry:\n os.truncate(\"{}\", 0)\nexcept OSError as e:\n assert e.errno in (errno.EPERM, errno.EACCES, errno.EROFS), e\n print(\"TRUNCATE_BLOCKED\")\nelse:\n raise AssertionError(\"truncate escaped scratch\")'",
                file.display()
            ),
        );
        assert!(ok && out.contains("TRUNCATE_BLOCKED"), "{out}");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "unchanged");
    }

    /// Machines: commands change files only in the chosen folders.
    #[test]
    fn writes_can_be_confined_to_chosen_folders() {
        if !confines_writes() {
            eprintln!(
                "SKIP write confinement: needs Landlock ABI 3 (sandbox: {})",
                describe()
            );
            assert!(
                prepare(
                    Some(&(
                        Spec {
                            writable: Some(Vec::new()),
                            ..Default::default()
                        },
                        false
                    )),
                    Path::new("/")
                )
                .is_err(),
                "refused, never run unconfined"
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let tmp = dir.path().join("tmp");
        let other = dir.path().join("other");
        for d in [&work, &tmp, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(other.join("keep.txt"), "original").unwrap();
        let spec = Spec {
            writable: Some(vec![work.clone(), tmp.clone()]),
            ..Default::default()
        };
        let o = other.display();
        let (ok, out) = run(
            &spec,
            &work,
            &format!(
                "set -e; echo a > a.txt; mkdir -p d/e; mv a.txt d/e/; echo t > '{t}/x'; \
                 echo quiet > /dev/null; echo shown > /dev/stdout; cat '{o}/keep.txt'; \
                 echo changed > '{o}/keep.txt' 2>/dev/null && echo WROTE || echo refused-write; \
                 touch '{o}/new' 2>/dev/null && echo MADE || echo refused-create; \
                 ln '{o}/keep.txt' linked 2>/dev/null && echo LINKED || echo refused-link; \
                 mv '{o}/keep.txt' moved 2>/dev/null && echo MOVED || echo refused-move; \
                 rm -f '{o}/keep.txt' 2>/dev/null || true; test -e '{o}/keep.txt' && echo still-there",
                t = tmp.display()
            ),
        );
        assert!(ok, "{out}");
        for want in [
            "shown",
            "original",
            "refused-write",
            "refused-create",
            "refused-link",
            "refused-move",
            "still-there",
        ] {
            assert!(out.contains(want), "{want}: {out}");
        }
        assert!(work.join("d/e/a.txt").exists() && tmp.join("x").exists());
        assert_eq!(
            std::fs::read_to_string(other.join("keep.txt")).unwrap(),
            "original"
        );
        assert!(!other.join("new").exists());
    }

    /// The shared process namespace older kernels use, whatever this one is.
    #[test]
    fn a_shared_process_namespace_hides_other_processes() {
        if test_mode("a_shared_process_namespace_hides_other_processes").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let work = dir.path().join("work");
        std::fs::create_dir_all(home.join("skills")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(home.join("auth.json"), "sk-very-secret").unwrap();
        // Its own group: other tests' commands in the default one (with
        // other specs) start that one afresh.
        let spec = Spec {
            hide: vec![home.clone()],
            keep: vec![home.join("skills")],
            folders: Vec::new(),
            writable: None,
            group: "process view test".into(),
        };
        if let Err(e) = imp::shared_works() {
            eprintln!("no private process view here: {e}");
            return;
        }
        let run_in = |spec: &Spec, script: &str| {
            let (program, args) = imp::join_args(spec, &work).unwrap().argv(
                "/bin/sh".into(),
                vec!["-c".into(), script.into()],
                None,
            );
            let mut cmd = std::process::Command::new(program);
            cmd.args(args);
            environment::protect(&mut cmd).unwrap();
            let out = cmd.output().unwrap();
            (
                out.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        };
        let run = |script: &str| run_in(&spec, script);
        // Ordinary work, including moving files between directories (what
        // Landlock ABI 1 would forbid).
        let (ok, out) =
            run("mkdir -p a b && echo hi > a/f && mv a/f b/f && ln b/f c && cat c && pwd");
        assert!(ok, "{out}");
        assert!(
            out.contains("hi") && out.contains(&work.display().to_string()),
            "{out}"
        );
        // This test process (like the Tessera server) is out of sight.
        let me = std::process::id();
        let secret = home.join("auth.json");
        let (_, out) = run(&format!(
            "cat /proc/{me}/root{} 2>&1; ls /proc | grep -c '^[0-9]'",
            secret.display()
        ));
        assert!(!out.contains("sk-very-secret"), "{out}");
        let visible: usize = out.lines().last().unwrap().trim().parse().unwrap();
        assert!(visible < 10, "only the namespace's processes: {out}");
        // A server one command starts is visible to, and stoppable by, the next.
        let (ok, out) = run("sleep 300 >/dev/null 2>&1 & echo $! > pid; echo started");
        assert!(ok, "{out}");
        let (ok, out) = run("kill $(cat pid) && echo stopped");
        assert!(ok && out.contains("stopped"), "{out}");
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "sk-very-secret");
        // Another chat's commands (another group) see none of this one's
        // processes, so neither their environment nor their pipes.
        let (ok, out) = run("sleep 301 >/dev/null 2>&1 & echo $! > pid; echo started");
        assert!(ok, "{out}");
        let sleeps = "for p in /proc/[0-9]*; do tr '\\0' ' ' < $p/cmdline 2>/dev/null; echo; done | grep -c '^sleep 30[1]'; true";
        let other = Spec {
            group: "another chat".into(),
            ..spec.clone()
        };
        let (_, out) = run_in(&other, sleeps);
        assert_eq!(
            out.trim(),
            "0",
            "another group's processes are out of sight"
        );
        let (_, out) = run(sleeps);
        assert_eq!(out.trim(), "1", "its own group's are in sight");
        let (ok, out) = run("kill $(cat pid) && echo stopped");
        assert!(ok && out.contains("stopped"), "{out}");
    }

    fn joined(cwd: &str) -> Prepared {
        joined_for_tests(cwd)
    }

    #[test]
    fn a_shared_namespace_command_must_be_wrapped() {
        let p = joined("/work");
        assert!(unsafe { p.enter() }.is_err(), "fails closed");
        let (program, args) = p.argv("bash".into(), vec!["-c".into(), "true".into()]);
        assert_eq!(program, "/usr/bin/nsenter");
        assert_eq!(args[..2], ["--preserve-credentials", "--"]);
        assert_eq!(Path::new(&args[2]), directory_launcher());
        assert_eq!(
            args[3..],
            [
                launcher::FLAG,
                "/work",
                "-",
                "-",
                "--",
                "bash",
                "-c",
                "true"
            ]
        );
        assert!(
            unsafe { p.enter() }.is_err(),
            "also fails closed without the clean environment"
        );
        p.protect_environment(&mut std::process::Command::new(program))
            .unwrap();
        assert!(unsafe { p.enter() }.is_ok());
    }

    /// Only numeric identity values cross exec, never nsenter --wd or a
    /// descriptor opened in the host mount view, and no trusted shell.
    #[test]
    fn a_held_folder_is_entered_and_checked_inside_the_shared_namespace() {
        let p = joined("/work dir");
        assert_eq!(p.directory_entry(), DirectoryEntry::Values(-1));
        assert!(unsafe { p.enter() }.is_err(), "fails closed until wrapped");
        let (program, args) = p.directory_argv(
            "/bin/sh".into(),
            vec!["-c".into(), "pwd".into()],
            42,
            (100, 200),
        );
        assert_eq!(program, "/usr/bin/nsenter");
        assert!(!args.iter().any(|a| a.to_string_lossy().starts_with("--wd")));
        assert_eq!(args[..2], ["--preserve-credentials", "--"]);
        assert_eq!(Path::new(&args[2]), directory_launcher());
        assert_eq!(
            args[3..],
            [
                launcher::FLAG,
                "/work dir",
                "100",
                "200",
                "--",
                "/bin/sh",
                "-c",
                "pwd"
            ]
        );
        assert_eq!(p.directory_entry(), DirectoryEntry::Values(42));
        p.protect_environment(&mut std::process::Command::new(program))
            .unwrap();
        assert!(unsafe { p.enter() }.is_ok());
    }

    /// A held folder is handed over only to the check set up for it: a
    /// shared-namespace launch built with plain `argv` fails to start rather
    /// than give the program a folder opened outside the sandbox.
    #[tokio::test]
    async fn a_held_folder_is_never_handed_to_an_unchecked_launch() {
        let dir = tempfile::tempdir().unwrap();
        let directory = crate::tools::file_object::ProcessDirectory::open(dir.path()).unwrap();
        let p = joined(&directory.path().display().to_string());
        let _ = p.argv("/bin/true".into(), Vec::new());
        // Started directly (no nsenter needed): the hook refuses first.
        let mut cmd = tokio::process::Command::new("/bin/true");
        let _held = directory.apply(&mut cmd, Some(p));
        let err = cmd.status().await.unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL), "{err}");
    }

    /// Runs inside the sandbox (re-executed by the test below).
    #[test]
    fn abstract_socket_probe() {
        use std::os::linux::net::SocketAddrExt;
        let Some(name) = std::env::var_os("TESSERA_ABSTRACT_PROBE") else {
            return;
        };
        let addr =
            std::os::unix::net::SocketAddr::from_abstract_name(name.as_encoded_bytes()).unwrap();
        match std::os::unix::net::UnixStream::connect_addr(&addr) {
            Ok(_) => println!("PROBE connected"),
            Err(e) => println!("PROBE refused: {e}"),
        }
    }

    #[test]
    fn abstract_sockets_outside_the_sandbox_are_unreachable() {
        use std::os::linux::net::SocketAddrExt;
        if !support().available || support().landlock < 6 {
            eprintln!("needs Landlock ABI 6 (sandbox: {})", describe());
            return;
        }
        let name = format!("tessera-test-{}", std::process::id());
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind_addr(&addr).unwrap();
        let probe = |sandboxed: bool| {
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "sandbox::tests::abstract_socket_probe",
                "--nocapture",
            ])
            .env("TESSERA_ABSTRACT_PROBE", &name);
            if sandboxed {
                let plan = Prepared::new(&Spec::default(), Path::new("/")).unwrap();
                unsafe { cmd.pre_exec(move || plan.enter()) };
            }
            String::from_utf8_lossy(&cmd.output().unwrap().stdout).to_string()
        };
        assert!(probe(false).contains("PROBE connected"));
        let inside = probe(true);
        assert!(inside.contains("PROBE refused"), "{inside}");
    }
}
