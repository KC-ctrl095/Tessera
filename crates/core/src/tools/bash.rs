//! Shell execution with live output, timeouts that hand long commands to the
//! background instead of killing them, and a `job` tool to manage them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_bool, opt_str, opt_u64};
use crate::auth::SecretPlace;

const JOB_BUFFER: usize = 2 * 1024 * 1024;

struct BgJob {
    id: String,
    /// Session that started it: other sessions (and bots) can't see it.
    session: String,
    command: String,
    started: Instant,
    output: Mutex<String>,
    exit: Mutex<Option<i32>>,
    done: tokio::sync::Notify,
    kill: CancellationToken,
    /// Bytes of output already returned by `job output`.
    read_pos: Mutex<usize>,
    #[cfg(all(test, target_os = "linux"))]
    /// Diagnostic only. Production signalling requires the unreaped Child.
    pid: u32,
    #[cfg(windows)]
    windows_job: Option<Arc<crate::platform::windows::TreeJob>>,
    #[cfg(target_os = "linux")]
    tree: Option<Arc<Mutex<tree::Tree>>>,
    /// What killing it did, once cleanup was attempted.
    stopped: Mutex<Option<Stopped>>,
    joined_namespace: bool,
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<BgJob>>> {
    static J: OnceLock<Mutex<HashMap<String, Arc<BgJob>>>> = OnceLock::new();
    J.get_or_init(Default::default)
}

const PENDING_SESSION_CAP: usize = 8;
const PENDING_GLOBAL_CAP: usize = 32;

fn pending_command_refusal(all: &HashMap<String, Arc<BgJob>>, session: &str) -> Option<String> {
    let mut total = 0;
    let mut own = 0;
    for job in all.values().filter(|job| cleanup_pending(job)) {
        total += 1;
        own += usize::from(job.session == session);
    }
    let count = if own >= PENDING_SESSION_CAP {
        own
    } else if total >= PENDING_GLOBAL_CAP {
        total
    } else {
        return None;
    };
    Some(format!(
        "{count} earlier commands are still being cleaned up; wait or check `job list`."
    ))
}

fn own_job(id: &str, session: &str) -> Option<Arc<BgJob>> {
    jobs()
        .lock()
        .get(id)
        .filter(|job| job.session == session)
        .cloned()
}

/// Keep `job` for the `job` tool, letting go of the oldest finished jobs
/// beyond what the machine keeps (24, fewer on a small machine; each holds
/// up to `JOB_BUFFER` of output). They used to stay until the server stopped.
fn keep_job(job: &Arc<BgJob>) {
    keep_job_locked(&mut jobs().lock(), job);
}

fn keep_job_locked(all: &mut HashMap<String, Arc<BgJob>>, job: &Arc<BgJob>) {
    all.insert(job.id.clone(), job.clone());
    drop_oldest_finished(all, crate::budget::get().kept_jobs);
}

/// Pending publication uses this same registry lock. Never act on the result
/// snapshot's earlier completion check when removing an addressable record.
fn remove_published_locked(all: &mut HashMap<String, Arc<BgJob>>, job: &Arc<BgJob>) -> bool {
    if all
        .get(&job.id)
        .is_some_and(|current| Arc::ptr_eq(current, job))
        && publication_complete(job)
        && !cleanup_pending(job)
    {
        all.remove(&job.id);
        true
    } else {
        false
    }
}

fn remove_published(job: &Arc<BgJob>) {
    remove_published_locked(&mut jobs().lock(), job);
}

fn drop_oldest_finished(all: &mut HashMap<String, Arc<BgJob>>, keep: usize) {
    let mut finished: Vec<(Instant, String)> = all
        .values()
        .filter(|j| publication_complete(j))
        .map(|j| (j.started, j.id.clone()))
        .collect();
    if finished.len() > keep {
        finished.sort();
        let extra = finished.len() - keep;
        for (_, id) in &finished[..extra] {
            all.remove(id);
        }
    }
}

/// Background commands kept now: running, finished, and the bytes of
/// output they hold.
pub fn job_stats() -> (usize, usize, usize) {
    let all = jobs().lock();
    let finished = all.values().filter(|j| j.exit.lock().is_some()).count();
    let bytes = all.values().map(|j| j.output.lock().len()).sum();
    (all.len() - finished, finished, bytes)
}

pub(crate) fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][A-Z0-9]|\x1b[=>]",
        )
        .unwrap()
    });
    let clean = re.replace_all(s, "").replace("\r\n", "\n");
    if !clean.contains('\r') {
        return clean;
    }
    // Carriage returns redraw a line (progress bars): keep the final state.
    clean
        .split('\n')
        .map(|line| line.rsplit('\r').find(|seg| !seg.is_empty()).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Only ordinary process and toolchain locations cross into agent work.
/// SSH_AUTH_SOCK is deliberate: git can use the user's agent, including its
/// signing authority. Nodes retain their stricter socket policy.
pub(crate) fn allowed_env(name: &str) -> bool {
    let name = if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.into()
    };
    matches!(
        name.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "LANG"
            | "LANGUAGE"
            | "TERM"
            | "TZ"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "SSH_AUTH_SOCK"
            | "DISPLAY"
            | "WAYLAND_DISPLAY"
            | "XDG_RUNTIME_DIR"
            | "CARGO_HOME"
            | "RUSTUP_HOME"
            | "RUSTUP_TOOLCHAIN"
            | "VIRTUAL_ENV"
            | "CONDA_PREFIX"
            | "NVM_DIR"
            | "PYENV_ROOT"
            | "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "PATHEXT"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "APPDATA"
            | "LOCALAPPDATA"
            | "PROGRAMFILES"
            | "PROGRAMFILES(X86)"
            | "PROGRAMDATA"
            | "CLAUDE_CODE_GIT_BASH_PATH"
    ) || name.starts_with("LC_")
        || (name.starts_with("XDG_") && (name.ends_with("_DIR") || name.ends_with("_DIRS")))
}

pub(crate) fn agent_environment(
    inherited: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    pass_env: &[String],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    inherited
        .into_iter()
        .filter(|(k, _)| {
            let name = k.to_string_lossy();
            // Tessera's private paths never become inherited process settings.
            !name.to_ascii_uppercase().starts_with("TESSERA_")
                && (allowed_env(&name)
                    || pass_env.iter().any(|n| {
                        if cfg!(windows) {
                            n.eq_ignore_ascii_case(&name)
                        } else {
                            n == &name
                        }
                    }))
        })
        .collect()
}

pub(crate) fn withhold_env(cmd: &mut tokio::process::Command) {
    pass_environment(cmd, &[]);
}

pub(crate) fn pass_environment(cmd: &mut tokio::process::Command, names: &[String]) {
    // Preserve only explicit settings the harness put on this Command, not
    // arbitrary server variables. Callers inject requested vault values last.
    let explicit: Vec<_> = cmd
        .as_std()
        .get_envs()
        .map(|(k, v)| (k.to_os_string(), v.map(std::ffi::OsStr::to_os_string)))
        .collect();
    cmd.env_clear()
        .envs(agent_environment(std::env::vars_os(), names));
    for (k, v) in explicit {
        if let Some(v) = v {
            cmd.env(k, v);
        } else {
            cmd.env_remove(k);
        }
    }
}

/// Agent work runs below the server: 10 steps nicer, the lowest best-effort
/// I/O level, and (Linux) first in line when memory runs out, so builds and
/// tests an agent starts (and what they start in turn) can't starve or take
/// down Tessera itself, and every device connected to it, on a small server.
/// Called in the child before exec (plain system calls only); best effort.
#[cfg(unix)]
pub(crate) fn yield_to_server() {
    // SAFETY: async-signal-safe system calls on the calling process only.
    unsafe {
        let _ = libc::nice(10);
        // Raising one's own OOM score needs no privilege; children inherit it.
        // (On 2026-10-06 a test browser's 4 GB ran a server out of memory.)
        #[cfg(target_os = "linux")]
        {
            let fd = libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY);
            if fd >= 0 {
                let v = b"500";
                let _ = libc::write(fd, v.as_ptr().cast(), v.len());
                libc::close(fd);
            }
        }
        #[cfg(target_os = "linux")]
        {
            const IOPRIO_WHO_PROCESS: libc::c_long = 1;
            const IOPRIO_CLASS_BE: libc::c_long = 2;
            libc::syscall(
                libc::SYS_ioprio_set,
                IOPRIO_WHO_PROCESS,
                0 as libc::c_long,
                (IOPRIO_CLASS_BE << 13) | 7,
            );
        }
    }
}

/// The shell running `command`, in the sandbox when there is one.
pub(crate) fn shell_command(
    command: &str,
    sandbox: Option<&crate::sandbox::Prepared>,
) -> tokio::process::Command {
    shell_command_impl(command, sandbox, None)
}

/// `cwd`: the working folder held open for this launch, which the sandbox
/// then enters in its own view ([`super::file_object::ProcessDirectory::argv`]).
fn shell_command_impl(
    command: &str,
    sandbox: Option<&crate::sandbox::Prepared>,
    cwd: Option<&super::file_object::ProcessDirectory>,
) -> tokio::process::Command {
    // Windows: Git Bash, PowerShell or cmd (see `platform::shell`); there
    // is no sandbox there to wrap it in.
    #[cfg(windows)]
    {
        let _ = (sandbox, cwd);
        let shell = crate::platform::shell();
        let (line, env) = shell.command_line(command);
        let mut c = tokio::process::Command::new(&shell.program);
        c.raw_arg(line).envs(env);
        crate::platform::agent_process(&mut c);
        c
    }
    #[cfg(not(windows))]
    let (shell, flag) = if std::path::Path::new("/bin/bash").exists()
        || std::path::Path::new("/usr/bin/bash").exists()
    {
        ("bash", "-c")
    } else {
        ("sh", "-c")
    };
    #[cfg(not(windows))]
    {
        let args = vec![flag.into(), command.into()];
        let (program, args) = match (cwd, sandbox) {
            (Some(cwd), sandbox) => cwd.argv(sandbox, shell.into(), args),
            (None, Some(s)) => s.argv(shell.into(), args),
            (None, None) => (shell.into(), args),
        };
        let mut c = tokio::process::Command::new(program);
        c.args(args);
        c
    }
}

#[cfg(target_os = "linux")]
const TREE_POLL_STEADY: Duration = Duration::from_millis(500);

#[cfg(target_os = "linux")]
const KEPT_TREE_POLL: Duration = Duration::from_millis(250);

#[cfg(target_os = "linux")]
fn tree_poll_interval(age: Duration) -> Duration {
    if age < Duration::from_secs(2) {
        Duration::from_millis(20)
    } else {
        TREE_POLL_STEADY
    }
}

/// Retain ownership immediately after spawn, before any await can reap it.
/// T9's Claude supervisor supersedes this guard where available.
pub(crate) struct ChildTree {
    #[cfg(target_os = "linux")]
    tree: Option<Arc<Mutex<tree::Tree>>>,
    #[cfg(target_os = "linux")]
    observing: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(windows)]
    windows_job: Option<crate::platform::windows::TreeJob>,
    armed: bool,
}
impl ChildTree {
    pub(crate) fn capture(pid: u32) -> Self {
        #[cfg(target_os = "linux")]
        {
            let tree = tree::external_tree(pid as i32).map(|tree| Arc::new(Mutex::new(tree)));
            let observing = Arc::new(std::sync::atomic::AtomicBool::new(true));
            if let Some(tree) = tree.clone() {
                let active = observing.clone();
                let started = Instant::now();
                std::thread::spawn(move || {
                    while active.load(std::sync::atomic::Ordering::Relaxed) {
                        if !tree.lock().observe() {
                            break;
                        }
                        // Pin fast startup forks, then avoid a permanent hot watcher.
                        std::thread::sleep(tree_poll_interval(started.elapsed()));
                    }
                });
            }
            Self {
                tree,
                observing,
                armed: true,
            }
        }
        #[cfg(windows)]
        {
            Self {
                windows_job: crate::platform::windows::take_job(pid),
                armed: true,
            }
        }
        #[cfg(all(not(target_os = "linux"), not(windows)))]
        {
            let _ = pid;
            Self { armed: true }
        }
    }
    /// Accept completion only after the caller validates the result. Timeout,
    /// I/O failure, cancellation and early return otherwise clean up on Drop.
    pub(crate) fn disarm(mut self) {
        self.armed = false;
        #[cfg(target_os = "linux")]
        self.observing
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn stop(mut self) {
        self.cleanup();
    }
    /// Linux callers keep the actual Child unreaped while the retained tree
    /// is stopped off-worker. The id is only a reap-state gate, not authority.
    #[cfg(target_os = "linux")]
    pub(crate) async fn stop_unreaped(mut self, child: &tokio::process::Child) {
        if child.id().is_none() {
            tracing::warn!("external process cleanup not checked: child was already reaped");
            self.disarm();
            return;
        }
        self.armed = false;
        if tokio::task::spawn_blocking(move || self.cleanup_linux())
            .await
            .is_err()
        {
            tracing::warn!("external process cleanup not checked: cleanup worker failed");
        }
    }

    /// Drop can run after the runtime has gone away. Keep the owning Child
    /// unreaped and perform the bounded tree stop directly in that context.
    #[cfg(target_os = "linux")]
    pub(crate) fn stop_unreaped_inline(mut self, child: &tokio::process::Child) {
        if child.id().is_none() {
            tracing::warn!("external process cleanup not checked: child was already reaped");
            self.disarm();
            return;
        }
        self.armed = false;
        self.cleanup_linux();
    }
    #[cfg(target_os = "linux")]
    fn cleanup_linux(&mut self) {
        self.observing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(tree) = self.tree.take() {
            let stopped = tree.lock().stop(Duration::from_millis(1500));
            if stopped.incomplete || !stopped.left.is_empty() {
                tracing::warn!(
                    "external process cleanup incomplete; some processes could not be checked or stopped"
                );
            }
        } else {
            tracing::warn!("external process cleanup incomplete: no held ownership handle");
        }
    }
    fn cleanup(&mut self) {
        if !std::mem::replace(&mut self.armed, false) {
            return;
        }
        #[cfg(target_os = "linux")]
        {
            self.observing
                .store(false, std::sync::atomic::Ordering::Relaxed);
            if let Some(tree) = self.tree.take() {
                std::thread::spawn(move || {
                    let stopped = tree.lock().stop(Duration::from_millis(1500));
                    if stopped.incomplete || !stopped.left.is_empty() {
                        tracing::warn!(
                            "external process cleanup incomplete; some processes could not be checked or stopped"
                        );
                    }
                });
            } else {
                tracing::warn!("external process cleanup incomplete: no held ownership handle");
            }
        }
        #[cfg(windows)]
        if let Some(job) = self.windows_job.take() {
            std::thread::spawn(move || {
                if !matches!(job.end_checked(Duration::from_secs(3)), Ok(0)) {
                    tracing::warn!(
                        "external process cleanup not checked or incomplete (Windows Job)"
                    );
                }
            });
        } else {
            tracing::warn!("external process cleanup not checked: no owned Windows Job");
        }
        #[cfg(all(not(target_os = "linux"), not(windows)))]
        tracing::warn!("external process cleanup not checked: no retained process ownership");
    }
}
impl Drop for ChildTree {
    fn drop(&mut self) {
        self.cleanup();
    }
}
/// Best-effort compatibility API for callers that still hold an unreaped child.
/// Async callers must instead capture ChildTree immediately after spawn.
#[cfg(unix)]
pub(crate) fn kill_group(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        // Acquire the direct child's handle before the caller can reap it;
        // only the tree scan/grace wait moves off this async worker.
        if let Some(mut tree) = tree::external_tree(pid as i32) {
            std::thread::spawn(move || tree.stop(Duration::from_millis(1500)));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // No Child ownership is available to check whether its leader was
        // reaped. Do not signal this numeric group, now or on a delayed thread.
        let _ = pid;
        tracing::warn!("process cleanup not checked: an unreaped Child is required");
    }
}

/// No spawn-time ownership ticket here: a later numeric registry lookup could
/// take a different command's Job after PID reuse. Callers must retain ChildTree.
#[cfg(windows)]
pub(crate) fn kill_group(pid: u32) {
    if pid == 0 {
        return;
    }
    tracing::warn!("process cleanup not checked: no spawn-time-owned Windows Job handle");
}

/// A private per-launch marker helps find detached descendants. Commands that
/// clear their environment and reparent before observation can escape cleanup.
const JOB_MARKER: &str = "TESSERA_JOB";
const CLEANUP_FALLBACK: &str = "Cleanup used numeric process IDs because Linux process handles (pidfd) are unavailable. This is best-effort, not race-free: a process ID can be reused between the check and the signal.";
const CLEANUP_BEST_EFFORT: &str = "Cleanup is best-effort. A daemon that cleared its environment and detached before observation may escape.";
const CLEANUP_JOINED: &str =
    "Cleanup in a joined PID namespace is best-effort, not namespace-wide.";

/// What stopping a job did: how many processes it signalled, and those
/// still running afterwards (another user's, or stuck in the kernel).
#[derive(Clone, Debug, Default)]
struct Stopped {
    signalled: usize,
    left: Vec<i32>,
    numeric_fallback: bool,
    incomplete: bool,
    joined_namespace: bool,
    not_checked: bool,
    remaining: Option<u32>,
    // Pending is independent of exit: an ended job can still be cleaning up.
    pending: bool,
}

impl Stopped {
    #[cfg(not(target_os = "linux"))]
    fn not_checked() -> Self {
        Self {
            not_checked: true,
            incomplete: true,
            ..Default::default()
        }
    }
}

/// Numeric Unix group signalling requires the actual unreaped Child at the
/// moment of signalling. Never schedule a numeric signal for after its reap.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn signal_unreaped(child: &mut tokio::process::Child) {
    if matches!(child.try_wait(), Ok(None))
        && let Some(pid) = child.id()
        && pid != 0
    {
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
}

/// Observe polite Linux exit without reaping the leader: even its zombie pins
/// the parent edge until held-handle descendant cleanup has finished.
#[cfg(target_os = "linux")]
pub(crate) async fn wait_unreaped(child: &tokio::process::Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    while let Some(pid) = child.id() {
        let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut status,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 || unsafe { status.si_pid() } != 0 || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
/// Stop through retained ownership and markers; escaped daemons may be missed.
async fn stop_job(job: &BgJob, alive: bool) -> Stopped {
    #[cfg(target_os = "linux")]
    {
        let _ = alive;
        let tree = job.tree.clone();
        let marker = format!("{JOB_MARKER}={}", job.id);
        let mut stopped = tokio::task::spawn_blocking(move || match tree {
            Some(tree) => tree.lock().stop(Duration::from_millis(1500)),
            None => {
                let mut stopped = tree::stop_marker(marker);
                stopped.incomplete = true; // missing initial handle, env-cleared children unknown
                if !stopped.left.contains(&-1) {
                    stopped.left.push(-1);
                }
                stopped
            }
        })
        .await
        .unwrap_or_else(|_| Stopped {
            left: vec![-1],
            incomplete: true,
            ..Default::default()
        });
        stopped.joined_namespace = job.joined_namespace;
        stopped
    }
    #[cfg(windows)]
    {
        let _ = alive;
        let owned = job.windows_job.clone();
        tokio::task::spawn_blocking(move || match owned {
            Some(owned) => match owned.end_checked(Duration::from_secs(3)) {
                Ok(remaining) => Stopped {
                    remaining: Some(remaining),
                    incomplete: remaining != 0,
                    ..Default::default()
                },
                Err(_) => Stopped::not_checked(),
            },
            None => Stopped::not_checked(),
        })
        .await
        .unwrap_or_else(|_| Stopped::not_checked())
    }
    #[cfg(all(not(target_os = "linux"), not(windows)))]
    {
        let _ = (job, alive);
        Stopped::not_checked()
    }
}

/// What `job kill` says: honestly, including processes that survived.
fn killed_text(id: &str, ended: bool, s: &Stopped) -> String {
    let mut text = killed_text_impl(id, ended, s);
    text.push(' ');
    text.push_str(if s.joined_namespace {
        CLEANUP_JOINED
    } else {
        CLEANUP_BEST_EFFORT
    });
    if s.numeric_fallback {
        text.push(' ');
        text.push_str(CLEANUP_FALLBACK);
    }
    text
}

fn killed_text_impl(id: &str, ended: bool, s: &Stopped) -> String {
    let what = if ended {
        format!("`{id}` had already ended")
    } else {
        format!("Stopped `{id}`")
    };
    if s.pending {
        return format!("Stopped; cleanup still running (job `{id}`).");
    }
    if s.not_checked {
        return format!(
            "{what}; cleanup not checked. No process ownership was available or its check failed."
        );
    }
    if let Some(remaining) = s.remaining {
        return if remaining == 0 {
            format!("{what}; Windows Job checked, with no processes still running.")
        } else {
            format!(
                "{what}; Windows Job checked, but {remaining} processes remain after the bounded termination wait."
            )
        };
    }
    if s.incomplete {
        return format!(
            "{what}; stopped; some processes could not be checked. Unidentified processes may remain; no clean-tree result is claimed."
        );
    }
    let pids: Vec<_> = s.left.iter().copied().filter(|pid| *pid > 0).collect();
    if !pids.is_empty() {
        let pids = pids
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        return format!(
            "{what}, but known processes remain (pid {pids}). Check with `ps -o pid,stat,user,args -p {pids}`."
        );
    }
    format!(
        "{what}; cleanup checked {} known processes, with none still running.",
        s.signalled
    )
}

/// Shared by ordinary Stop results, job list/output, and explicit job kill.
fn cleanup_state(job: &BgJob) -> Option<String> {
    // Do not hold stopped while taking exit: Stop publication takes them in
    // the opposite direction. Clone the cleanup state first.
    let stopped = job.stopped.lock().clone()?;
    let mut text = killed_text(&job.id, false, &stopped);
    if !stopped.pending && job.exit.lock().is_none() {
        let what = if stopped.incomplete || stopped.not_checked || !stopped.left.is_empty() {
            "Cleanup attempt finished"
        } else {
            "Tree cleaned up"
        };
        text = format!("{what}; output still draining (job `{}`).\n{text}", job.id);
    }
    Some(text)
}

fn cleanup_pending(job: &BgJob) -> bool {
    job.stopped.lock().as_ref().is_some_and(|s| s.pending)
}

fn publication_complete(job: &BgJob) -> bool {
    job.exit.lock().is_some() && !cleanup_pending(job)
}

/// Publish before cancellation/waits, so every observer can address the job
/// even if the cleanup worker or pipe drain outlasts the foreground deadline.
fn publish_stop_locked(all: &mut HashMap<String, Arc<BgJob>>, job: &Arc<BgJob>) -> bool {
    let ended_cleanup = {
        // Registry -> exit -> stopped is shared with removal/retention. A
        // stale waiter cannot hide this pending publication from reads/caps.
        let exit = job.exit.lock();
        let mut state = job.stopped.lock();
        let new = state.is_none();
        if new {
            *state = Some(Stopped {
                pending: true,
                ..Default::default()
            });
        }
        job.kill.cancel();
        new && exit.is_some()
    };
    keep_job_locked(all, job);
    ended_cleanup
}

fn request_stop(job: &Arc<BgJob>) {
    let ended_cleanup = publish_stop_locked(&mut jobs().lock(), job);
    if ended_cleanup {
        let finishing = job.clone();
        tokio::spawn(async move {
            let final_state = stop_job(&finishing, false).await;
            *finishing.stopped.lock() = Some(final_state);
            finishing.done.notify_waiters();
        });
    }
}
/// Retained Linux process ownership (best-effort, no orphan adoption).
#[cfg(target_os = "linux")]
mod tree;

/// Starts the command; output accumulates in `job.output`.
type Progress = Arc<dyn Fn(&str) + Send + Sync>;

/// What kind of command a job runs.
#[derive(Clone, Copy, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Launch {
    /// An ordinary command (`bash`): not in a read-only chat.
    Command,
    /// A helper's restricted runner; only the sandbox confines project
    /// code. `offline`: in a network namespace of its own.
    Reading { offline: bool },
}

#[allow(clippy::too_many_arguments)]
fn spawn_job(
    (session, admission): (&str, &super::admission::ActionAdmission),
    command: &str,
    cwd: super::file_object::ProcessDirectory,
    secrets: Vec<String>,
    env: Vec<(String, String)>,
    sandbox: Option<crate::sandbox::Prepared>,
    progress: Option<Progress>,
    launch: Launch,
) -> Result<Arc<BgJob>, String> {
    if let Some(reason) = pending_command_refusal(&jobs().lock(), session) {
        return Err(reason);
    }
    let joined_namespace = sandbox
        .as_ref()
        .is_some_and(crate::sandbox::Prepared::joins_pid_namespace);
    let mut cmd = shell_command_impl(command, sandbox.as_ref(), Some(&cwd));
    withhold_env(&mut cmd);
    // User secrets are available by name; their values never pass through the model.
    cmd.envs(env);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Merges and rebases take their default message instead of failing
        // ("Terminal is dumb, but EDITOR unset") or waiting for an editor.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("TESSERA", "1")
        .kill_on_drop(false);
    let id = format!("job-{}", &crate::util::new_id()[14..]);
    cmd.env(JOB_MARKER, &id);
    // New session: its own process group (so the whole tree can be killed)
    // and no controlling terminal, so programs like sudo can't prompt over
    // the user's TUI or read its keystrokes.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            yield_to_server();
            Ok(())
        });
    }
    let held = cwd.apply(&mut cmd, sandbox);
    // No network for this command (a read-only helper's, in a sandbox that
    // made it a user namespace of its own, where it may make a network
    // namespace too): only loopback, up, so local tests still work.
    #[cfg(target_os = "linux")]
    if launch == (Launch::Reading { offline: true }) {
        unsafe {
            cmd.pre_exec(|| {
                if libc::unshare(libc::CLONE_NEWNET) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                loopback_up();
                Ok(())
            });
        }
    }
    let start = || crate::platform::spawn(&mut cmd, crate::platform::Start::Agent);
    let mut child = match launch {
        Launch::Command => admission.run(start),
        Launch::Reading { .. } => admission.run_reading(start),
    }
    .map_err(|e| {
        format!(
            "Failed to start shell: {}",
            super::file_object::launch_error(&e)
        )
    })?;
    let held = held.after_spawn();
    #[cfg(any(target_os = "linux", windows))]
    let pid = child.id().unwrap_or(0);
    #[cfg(target_os = "linux")]
    let held_tree = tree::external_tree(pid as i32)
        .map(|tree| Arc::new(Mutex::new(tree.with_marker(format!("{JOB_MARKER}={id}")))));
    #[cfg(windows)]
    let windows_job = crate::platform::windows::take_job(pid).map(Arc::new);
    let job = Arc::new(BgJob {
        id,
        session: session.into(),
        command: command.to_string(),
        started: Instant::now(),
        output: Mutex::new(String::new()),
        exit: Mutex::new(None),
        done: tokio::sync::Notify::new(),
        kill: CancellationToken::new(),
        read_pos: Mutex::new(0),
        #[cfg(all(test, target_os = "linux"))]
        pid,
        #[cfg(windows)]
        windows_job,
        #[cfg(target_os = "linux")]
        tree: held_tree,
        stopped: Mutex::new(None),
        joined_namespace,
    });

    let mut readers = Vec::new();
    let drain = CancellationToken::new();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let job = job.clone();
        let progress = progress.clone();
        let mut redactor = super::redaction::Stream::terminal(secrets.clone());
        let drain = drain.clone();
        readers.push(tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = vec![0u8; 16 * 1024];
            let publish = |text: String| {
                let text = strip_ansi(&text);
                if text.is_empty() {
                    return;
                }
                {
                    let mut out = job.output.lock();
                    out.push_str(&text);
                    if out.len() > JOB_BUFFER {
                        let cut = crate::util::ceil_char(&out, out.len() - JOB_BUFFER / 2);
                        *out = format!("[… earlier output dropped …]\n{}", &out[cut..]);
                        *job.read_pos.lock() = 0;
                    }
                }
                if let Some(p) = &progress {
                    p(&text);
                }
            };
            loop {
                let read = tokio::select! {
                    read = stream.read(&mut buf) => read,
                    _ = drain.cancelled() => break,
                };
                let n = match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                publish(redactor.push(&buf[..n]));
            }
            publish(redactor.finish());
        }));
    }
    #[cfg(target_os = "linux")]
    if let Some(tree) = job.tree.clone() {
        tokio::spawn(async move {
            loop {
                let observing = tree.clone();
                // /proc scans AND mutex waits belong off the Tokio worker,
                // including while Stop owns this mutex through its grace wait.
                let alive = tokio::task::spawn_blocking(move || observing.lock().observe()).await;
                if !matches!(alive, Ok(true)) {
                    break;
                }
                tokio::time::sleep(KEPT_TREE_POLL).await;
            }
        });
    }
    let j = job.clone();
    tokio::spawn(async move {
        let _held = held;
        let wait = async { child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1) };
        let status = tokio::select! {
            s = wait => s,
            _ = j.kill.cancelled() => {
                request_stop(&j);
                #[cfg(all(unix, not(target_os = "linux")))]
                signal_unreaped(&mut child);
                let stopped = stop_job(&j, true).await;
                *j.stopped.lock() = Some(stopped);
                let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                -9
            }
        };
        // Let readers drain (a grandchild may hold the pipe; don't wait forever).
        for mut r in readers {
            if tokio::time::timeout(Duration::from_secs(2), &mut r)
                .await
                .is_err()
            {
                drain.cancel();
                let _ = r.await; // flush the held suffix before final exit publication
            }
        }
        let late_cleanup = {
            let mut exit = j.exit.lock();
            if j.kill.is_cancelled() && cleanup_pending(&j) {
                true
            } else {
                *exit = Some(status);
                false
            }
        };
        if late_cleanup {
            let final_state = stop_job(&j, false).await;
            *j.stopped.lock() = Some(final_state);
            *j.exit.lock() = Some(status);
        }
        j.done.notify_waiters();
    });
    Ok(job)
}

fn format_result(job: &BgJob, ctx: &ToolCtx) -> ToolOutput {
    let out = job.output.lock().clone();
    let code = *job.exit.lock();
    let (body, _) = super::fit_output(
        out.trim_end(),
        ctx,
        &format!("bash: {}", crate::util::one_line(&job.command, 60)),
    );
    let elapsed = job.started.elapsed().as_secs_f32();
    let status = match code {
        Some(0) => format!("[exit 0 · {elapsed:.1}s]"),
        Some(-9) => format!("[killed · {elapsed:.1}s]"),
        Some(c) => format!("[exit {c} · {elapsed:.1}s]"),
        None if cleanup_pending(job) => format!("[stopped; cleanup pending · {elapsed:.1}s]"),
        None if job.kill.is_cancelled() => {
            format!("[stopped; output still draining · {elapsed:.1}s]")
        }
        None => format!("[running · {elapsed:.1}s]"),
    };
    let mut text = if body.is_empty() {
        format!("(no output) {status}")
    } else {
        format!("{body}\n{status}")
    };
    if let Some(cleanup) = cleanup_state(job) {
        text.push('\n');
        text.push_str(&cleanup);
    }
    let stopped = job.stopped.lock().clone();
    ToolOutput {
        is_error: matches!(code, Some(c) if c != 0),
        ..ToolOutput::text(text)
    }
    .with_details(
        json!({ "exit_code": code, "stopped": job.kill.is_cancelled(),
        "job_id": job.kill.is_cancelled().then_some(&job.id),
        "cleanup_pending": stopped.as_ref().is_some_and(|s| s.pending),
        "output_draining": code.is_none() && stopped.as_ref().is_some_and(|s| !s.pending),
        "cleanup_not_checked": stopped.as_ref().is_some_and(|s| s.not_checked),
        "cleanup_remaining": stopped.as_ref().and_then(|s| s.remaining),
        "cleanup_incomplete": stopped.as_ref().map(|s| s.incomplete),
        "cleanup_left": stopped.as_ref().map(|s| &s.left),
        "cleanup_best_effort": stopped.is_some(),
        "cleanup_joined_namespace": job.joined_namespace }),
    )
}

/// Authorize explicit read operands without reading any bytes. This does not
/// grant a shell exemption: the shell would reopen names rather than use these
/// pinned file objects. Unknown/indirect reads require the ordinary question.
fn read_risk(cmd: &str, cwd: &std::path::Path, ctx: &ToolCtx, uncertain_asks: bool) -> Risk {
    let ask = |reason: &str| Risk::Ask {
        reason: reason.into(),
        key: super::approval_key("bash", &json!({"command": cmd, "cwd": cwd}), ctx),
    };
    let credential = crate::claude_code::credential_target("Bash", &json!({"command": cmd}), cwd);
    let mut uncertain = false;
    if let Some(targets) = super::policy::read_targets(cmd) {
        for target in targets {
            let path = cwd.join(crate::util::expand_home(&target));
            match super::path_risk(&path, ctx) {
                block @ Risk::Block { .. } => return block,
                Risk::Ask { .. } => uncertain = true,
                Risk::None => {}
            }
            let Ok(opened) = super::file_object::Opened::open(&path) else {
                uncertain = true;
                continue;
            };
            match super::opened_path_risk(&opened.path, ctx) {
                block @ Risk::Block { .. } => return block,
                Risk::Ask { .. } => uncertain = true,
                Risk::None => {}
            }
            if super::files::authorize_file(&opened.path, ctx, false).is_err() {
                uncertain = true;
            }
        }
    } else {
        uncertain = super::policy::mentions_read_program(cmd);
    }
    if credential {
        ask(
            "This command may read credential files. Approve this exact command, or use the file tools for a guarded, redacted read.",
        )
    } else if uncertain && uncertain_asks {
        ask(
            "The command's read targets cannot all be authorized (missing paths, expansions, recursive or indirect inputs). Approve the exact command, or use the file tools.",
        )
    } else {
        Risk::None
    }
}
pub struct Bash;

impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> String {
        "Run a non-interactive shell command (stdin closed) in the working directory. Long output is truncated, \
         with the full text kept as an artifact. A command still running after `timeout` seconds (default 120, \
         max 3600) continues as a background job you manage with `job`; `background: true` starts servers and \
         watchers that way immediately."
            .to_string()
            + crate::platform::shell_hint()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer"},
                "background": {"type": "boolean"},
                "cwd": {"type": "string"}
            },
            "required": ["command"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    fn summary(&self, args: &Value) -> String {
        crate::util::one_line(
            args.get("command").and_then(Value::as_str).unwrap_or(""),
            120,
        )
    }
    /// The whole command, every line: a risky part on a later line must
    /// be in front of the person approving it.
    fn approval_summary(&self, args: &Value) -> String {
        args.get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_matches(['\n', '\r'])
            .to_string()
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let Some(cmd) = args.get("command").and_then(Value::as_str) else {
            return Risk::None;
        };
        let policy = match command_risk(cmd, args, ctx, true) {
            block @ Risk::Block { .. } => return block,
            other => other,
        };
        // Secrets only reach commands that name them, and by default the
        // first such command in a session is shown to the user.
        let uses = ctx
            .host
            .secret_env(&ctx.session_id, cmd, SecretPlace::Server);
        if let Some(why) =
            super::secrets::elsewhere(&uses, || ctx.host.machines(), SecretPlace::Server)
        {
            return Risk::Block { reason: why };
        }
        let asks: Vec<String> = uses
            .used
            .into_iter()
            .filter(|(_, ask, _)| *ask)
            .map(|(name, _, hosts)| {
                if hosts.is_empty() {
                    format!("${name}")
                } else {
                    format!("${name} (for {})", hosts.join(", "))
                }
            })
            .collect();
        if asks.is_empty() {
            return policy;
        }
        let names: Vec<String> = asks
            .iter()
            .map(|a| {
                a.split(' ')
                    .next()
                    .unwrap_or(a)
                    .trim_start_matches('$')
                    .to_string()
            })
            .collect();
        let secret_reason = format!(
            "This command uses {} from your passwords & keys. The program can send the value anywhere; listed sites are not a network boundary.",
            asks.join(", ")
        );
        let secret_key = names
            .iter()
            .map(|n| format!("secret:{n}"))
            .collect::<Vec<_>>()
            .join("+");
        match policy {
            Risk::Block { .. } => policy,
            Risk::Ask { reason, key } => Risk::Ask {
                reason: format!("{reason} {secret_reason}"),
                key: format!("{key}+{secret_key}"),
            },
            Risk::None => Risk::Ask {
                reason: secret_reason,
                key: secret_key,
            },
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move { run_bash(args, ctx).await })
    }
}

/// What any command risks, before secrets: Claude Code's account files and
/// Tessera's own store are refused, risky commands and credential reads ask.
/// `uncertain_reads`: a read the file tools couldn't check first asks
/// (`bash`); a read-only helper's commands ask only for credential files.
fn command_risk(cmd: &str, args: &Value, ctx: &ToolCtx, uncertain_reads: bool) -> Risk {
    let protected = ctx.host.protected_paths();
    if cmd.contains(".credentials.json")
        || [
            "~/.claude",
            "$HOME/.claude",
            "${HOME}/.claude",
            "$env:USERPROFILE\\.claude",
            "%USERPROFILE%\\.claude",
        ]
        .iter()
        .any(|p| cmd.contains(p))
        || dirs::home_dir().is_some_and(|home| cmd.contains(&format!("{}/.claude", home.display())))
    {
        return Risk::Block {
            reason: "Refused: commands cannot access Claude Code's account files.".into(),
        };
    }
    let cwd = opt_str(args, "cwd")
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.cwd.clone());
    if protected.mentioned_in(cmd) || protected.covers(&super::canonical(&cwd)) {
        return Risk::Block { reason: "Refused: that command touches Tessera's own credential, session and settings store. Use the `settings` tool for its settings.".into() };
    }
    let mut policy = super::policy::assess_command(cmd);
    // Best effort only. The sandbox, not shell spelling, protects deferred
    // execution. Resolve visible operands too so a symlink name cannot hide
    // a startup target from the unconfined approval path.
    if !matches!(policy, Risk::Block { .. })
        && let Some(home) = dirs::home_dir()
    {
        for word in cmd.split(|c: char| c.is_whitespace() || "\"';|&<>=()".contains(c)) {
            let word = word
                .replace("${HOME}", &home.to_string_lossy())
                .replace("$HOME", &home.to_string_lossy());
            if word.is_empty() {
                continue;
            }
            let path = cwd.join(crate::util::expand_home(&word));
            if path.exists() && super::files::startup_path(&path) {
                policy = Risk::Ask {
                    reason: "This command names a startup location, possibly through a link. Approve the exact command. Sandboxed commands still cannot change it; use write or edit for an explicit change.".into(),
                    key: super::approval_key("bash", &json!({"command":cmd, "cwd":cwd}), ctx),
                };
                break;
            }
        }
    }
    // This guard also runs for Claude's bridged bash. The CLI-native
    // credential guard alone cannot protect a call dispatched via MCP.
    match read_risk(cmd, &cwd, ctx, uncertain_reads) {
        block @ Risk::Block { .. } => return block,
        Risk::Ask { reason, key } => {
            if let Risk::Ask {
                reason: ordinary, ..
            } = &policy
            {
                policy = Risk::Ask {
                    reason: format!("{ordinary} {reason}"),
                    key,
                };
            } else if !matches!(policy, Risk::Block { .. }) {
                policy = Risk::Ask { reason, key };
            }
        }
        Risk::None => {}
    }
    if ctx.host.unconfined_commands_ask() && !matches!(policy, Risk::Block { .. }) {
        let extra = if let Risk::Ask { reason, .. } = policy {
            format!(" {reason}")
        } else {
            String::new()
        };
        return Risk::Ask {
            reason: format!(
                "The full safe box isn't available. Approve this exact command, or choose Off in What commands can reach to allow commands without it.{extra}"
            ),
            key: format!("unconfined:{args}"),
        };
    }
    policy
}

async fn run_bash(args: Value, ctx: &ToolCtx) -> ToolOutput {
    if ctx.host.unconfined_commands_ask()
        && let Risk::Ask { key, .. } = Bash.assess(&args, ctx)
        && ctx.cleared.as_deref() != Some(&key)
    {
        return ToolOutput::error(
            "The full safe box isn't available. This exact command needs approval.",
        );
    }
    let command = match arg_str(&args, "command") {
        Ok(c) => c.to_string(),
        Err(e) => return e,
    };
    let cwd = opt_str(&args, "cwd")
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.cwd.clone());
    if !cwd.is_dir() {
        return ToolOutput::error(format!(
            "Working directory {} does not exist.",
            cwd.display()
        ));
    }
    let timeout = Duration::from_secs(
        opt_u64(&args, "timeout")
            .unwrap_or(ctx.host.bash_timeout())
            .clamp(1, 3600),
    );
    let background = opt_bool(&args, "background").unwrap_or(false);
    let progress = if background {
        None
    } else {
        Some(ctx.progress.clone())
    };
    let cwd = match ctx.host.process_directory(&ctx.session_id, &cwd) {
        Ok(d) => d,
        Err(e) => return ToolOutput::error(e),
    };
    let spec = ctx.host.session_sandbox(&ctx.session_id);
    // Only this exact, already-approved call may cross Automatic's
    // unavailable-sandbox fallback. Other callers (including LSP) fail closed.
    let spec = if ctx.host.unconfined_commands_ask() && !crate::sandbox::support().available {
        None
    } else {
        spec
    };
    let sandbox = match cwd.prepare(spec.as_ref()) {
        Ok(s) => s,
        Err(e) => return ToolOutput::error(e),
    };
    let secret_use = ctx
        .host
        .secret_env(&ctx.session_id, &command, SecretPlace::Server);
    if let Some(why) =
        super::secrets::elsewhere(&secret_use, || ctx.host.machines(), SecretPlace::Server)
    {
        return ToolOutput::error(why);
    }
    if !secret_use.used.is_empty() {
        ctx.host.mark_secrets_used(
            &secret_use
                .used
                .iter()
                .map(|(n, _, _)| n.clone())
                .collect::<Vec<_>>(),
        );
    }
    let mut env = agent_environment(std::env::vars_os(), &ctx.host.pass_env())
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect::<Vec<_>>();
    env.extend(secret_use.env);
    let job = match spawn_job(
        (&ctx.session_id, &ctx.admission),
        &command,
        cwd,
        ctx.host.secrets(),
        env,
        sandbox,
        progress,
        Launch::Command,
    ) {
        Ok(j) => j,
        Err(e) => return ToolOutput::error(e),
    };
    if background {
        keep_job(&job);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let early = job.output.lock().clone();
        let state = if job.exit.lock().is_some() {
            "already exited"
        } else {
            "running"
        };
        return ToolOutput::text(format!(
            "Started background job `{}` ({state}). First output:\n{}\nUse `job` with id=\"{}\" to read output, wait, or kill.",
            job.id,
            crate::util::truncate(early.trim_end(), 4000),
            job.id
        )).with_details(json!({"job_id": job.id, "background": true}));
    }
    let done = job.done.notified();
    tokio::pin!(done);
    if job.exit.lock().is_none() {
        tokio::select! {
            _ = &mut done => {}
            _ = tokio::time::sleep(timeout) => {
                keep_job(&job);
                *job.read_pos.lock() = job.output.lock().len();
                let mut out = format_result(&job, ctx);
                out.is_error = false;
                out.content.push(tessera_proto::ContentPart::text(format!(
                    "The command is still running after {}s and continues as background job `{}`. Use `job` to wait for it, read new output, or kill it.",
                    timeout.as_secs(), job.id
                )));
                let details = out.details.get_or_insert_with(|| json!({}));
                details["job_id"] = json!(job.id);
                details["background"] = json!(true);
                return out;
            }
            _ = ctx.cancel.cancelled() => {
                request_stop(&job);
                let _ = tokio::time::timeout(Duration::from_secs(4), &mut done).await;
                let mut out = format_result(&job, ctx);
                out.content.push(tessera_proto::ContentPart::text("Interrupted by the user."));
                out.is_error = true;
                return out;
            }
        }
    }
    let mut out = format_result(&job, ctx);
    // A build or test run is often the next step after an edit.
    super::attach_late_diagnostics(ctx, &mut out);
    out
}

/// Bring the loopback interface of a fresh network namespace up. Runs in
/// the forked child before exec: system calls only.
#[cfg(target_os = "linux")]
fn loopback_up() {
    // SAFETY: plain system calls on a socket this opens and closes.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return;
        }
        let mut req: libc::ifreq = std::mem::zeroed();
        for (i, b) in b"lo".iter().enumerate() {
            req.ifr_name[i] = *b as libc::c_char;
        }
        if libc::ioctl(fd, libc::SIOCGIFFLAGS, &mut req as *mut libc::ifreq) == 0 {
            req.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
            libc::ioctl(fd, libc::SIOCSIFFLAGS, &req as *const libc::ifreq);
        }
        libc::close(fd);
    }
}

/// The name of the read-only helpers' command runner.
pub const READ_ONLY_BASH: &str = "bash_readonly";

/// The command runner of read-only helpers (`readonly: true`, or started by
/// a read-only chat): reviewers used to have no way to run `git diff`, a
/// search or the tests. Where the sandbox confines writes (Linux 5.19+),
/// commands write only to private scratch, without network unless the
/// helper's type allows it. Elsewhere only [`super::policy::review_command`]'s
/// list runs, but cargo still executes project code unconfined.
/// Never with stored secrets, never in the background.
pub struct ReadOnlyBash;

/// How a read-only helper's command runner is restricted.
enum Confined {
    /// The sandbox lets them write only in the scratch folder.
    Sandbox(crate::sandbox::Spec, bool),
    /// No such sandbox here: only listed commands run.
    List,
}

fn confinement(ctx: &ToolCtx) -> Confined {
    match ctx.host.session_sandbox(&ctx.session_id) {
        Some((spec, strict)) if crate::sandbox::confines_writes() => {
            Confined::Sandbox(spec, strict)
        }
        _ => Confined::List,
    }
}

/// What read-only helpers' commands may do on this server, in plain words
/// (`sandboxed`: the chat's sandbox is on and can confine writes).
pub fn read_only_commands(sandboxed: bool) -> &'static str {
    if sandboxed {
        "bash_readonly runs any command sandboxed: it reads everything it may read, writes only to its private scratch folder, and has no network unless its agent type allows it"
    } else {
        "On this server, allowed cargo commands run the project's own build scripts and tests unconfined: they can change files and use the network. bash_readonly permits only git reads, searches and listed cargo commands; no sandbox confines project code"
    }
}

/// Where read-only helper `session_id`'s commands may write (build output,
/// caches, temporary files); removed when the helper finishes.
pub fn scratch_dir(session_id: &str) -> PathBuf {
    #[cfg(unix)]
    let who = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let who = std::env::var("USERNAME").unwrap_or_default();
    let id: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    std::env::temp_dir()
        .join(format!("tessera-readonly-{who}"))
        .join(id)
}

/// Make the scratch folder (owner-only); refuse a parent someone else made
/// or replaced with a link in the shared temporary folder.
fn prepare_scratch(session_id: &str) -> Result<PathBuf, String> {
    let dir = scratch_dir(session_id);
    let parent = dir.parent().unwrap_or(&dir).to_path_buf();
    let fail = |e: std::io::Error| format!("Could not make the scratch folder: {e}");
    match std::fs::create_dir(&parent) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(fail(e)),
    }
    let meta = std::fs::symlink_metadata(&parent).map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "Refused: {} isn't this user's own folder.",
                parent.display()
            ));
        }
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    if !meta.is_dir() {
        return Err(format!("Refused: {} isn't a folder.", parent.display()));
    }
    for sub in ["tmp", "cache", "target"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(fail)?;
    }
    Ok(dir)
}

/// Remove a read-only helper's scratch folder when it finishes, whichever
/// way it ends.
pub struct ScratchGuard(pub String);

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let dir = scratch_dir(&self.0);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Free space where `dir` is, in bytes (`None` if unknown).
fn free_space(dir: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: statvfs fills the zeroed struct for a valid path.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0)
            .then(|| st.f_bavail as u64 * st.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// A cold build of a Rust workspace takes several gigabytes.
const SCRATCH_BUILD_ROOM: u64 = 10 << 30;

impl Tool for ReadOnlyBash {
    fn name(&self) -> &'static str {
        READ_ONLY_BASH
    }
    fn description(&self) -> String {
        "Run git reads, searches, builds, tests and lint. With a write-confining sandbox, commands write only \
         to private scratch. Without it, allowed cargo commands run the project's own build scripts and tests \
         unconfined: they can change files and use the network. Scratch (`TMPDIR`, `CARGO_TARGET_DIR`) is removed \
         when you finish; no stored secrets or background jobs. Unlisted commands are refused without a sandbox."
            .to_string()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer"},
                "cwd": {"type": "string"}
            },
            "required": ["command"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    /// One after the other, like `bash`: a check often depends on the last.
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    fn summary(&self, args: &Value) -> String {
        Bash.summary(args)
    }
    fn approval_summary(&self, args: &Value) -> String {
        Bash.approval_summary(args)
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let Some(cmd) = args.get("command").and_then(Value::as_str) else {
            return Risk::None;
        };
        if let Confined::List = confinement(ctx)
            && let Err(why) = super::policy::review_command(cmd)
        {
            return Risk::Block {
                reason: list_refusal(&why),
            };
        }
        command_risk(cmd, args, ctx, false)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move { run_read_only(args, ctx).await })
    }
}

fn list_refusal(why: &str) -> String {
    format!(
        "Refused: {why}. Only git reads, searches and listed cargo commands run here. {}. Ask the parent for anything else.",
        read_only_commands(false)
    )
}

async fn run_read_only(args: Value, ctx: &ToolCtx) -> ToolOutput {
    let command = match arg_str(&args, "command") {
        Ok(c) => c.to_string(),
        Err(e) => return e,
    };
    let cwd = opt_str(&args, "cwd")
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.cwd.clone());
    if !cwd.is_dir() {
        return ToolOutput::error(format!(
            "Working directory {} does not exist.",
            cwd.display()
        ));
    }
    let timeout = Duration::from_secs(
        opt_u64(&args, "timeout")
            .unwrap_or(ctx.host.bash_timeout())
            .clamp(1, 3600),
    );
    let confined = confinement(ctx);
    // `assess` refused unlisted commands already; this holds however the
    // call arrived.
    if let Confined::List = confined
        && let Err(why) = super::policy::review_command(&command)
    {
        return ToolOutput::error(list_refusal(&why));
    }
    let scratch = match prepare_scratch(&ctx.session_id) {
        Ok(d) => d,
        Err(e) => return ToolOutput::error(e),
    };
    if command.split_whitespace().any(|w| w == "cargo")
        && free_space(&scratch).is_some_and(|free| free < SCRATCH_BUILD_ROOM)
    {
        return ToolOutput::error(format!(
            "Refused: only {:.1} GB are free for the scratch folder, and a cold build needs about {} GB. Ask the parent to run it.",
            free_space(&scratch).unwrap_or(0) as f64 / (1u64 << 30) as f64,
            SCRATCH_BUILD_ROOM >> 30
        ));
    }
    let dir = match ctx.host.process_directory(&ctx.session_id, &cwd) {
        Ok(d) => d,
        Err(e) => return ToolOutput::error(e),
    };
    let network = ctx.host.helper_network(&ctx.session_id);
    let (prepared, offline) = match confined {
        Confined::Sandbox(mut spec, strict) => {
            spec.writable = Some(vec![scratch.clone()]);
            (dir.prepare(Some(&(spec, strict))), !network)
        }
        Confined::List => (
            dir.prepare(ctx.host.session_sandbox(&ctx.session_id).as_ref()),
            false,
        ),
    };
    let sandbox = match prepared {
        Ok(s) => s,
        Err(e) => return ToolOutput::error(e),
    };
    let mut env = agent_environment(std::env::vars_os(), &ctx.host.pass_env())
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect::<Vec<_>>();
    let at = |sub: &str| scratch.join(sub).display().to_string();
    env.extend(
        [
            ("TMPDIR", at("tmp")),
            ("TMP", at("tmp")),
            ("TEMP", at("tmp")),
            ("XDG_CACHE_HOME", at("cache")),
            ("CARGO_TARGET_DIR", at("target")),
            ("CARGO_INCREMENTAL", "0".into()),
            // `git status` would otherwise refresh the index file.
            ("GIT_OPTIONAL_LOCKS", "0".into()),
        ]
        .map(|(k, v)| (k.to_string(), v)),
    );
    if !network {
        env.push(("CARGO_NET_OFFLINE".into(), "true".into()));
    }
    let job = match spawn_job(
        (&ctx.session_id, &ctx.admission),
        &command,
        dir,
        ctx.host.secrets(),
        env,
        sandbox,
        Some(ctx.progress.clone()),
        Launch::Reading { offline },
    ) {
        Ok(j) => j,
        Err(e) => return ToolOutput::error(e),
    };
    let done = job.done.notified();
    tokio::pin!(done);
    let stopped = if job.exit.lock().is_none() {
        tokio::select! {
            _ = &mut done => None,
            _ = tokio::time::sleep(timeout) => Some(format!(
                "Stopped after {}s: a read-only helper's commands don't continue in the background. Pass a larger `timeout` (up to 3600) for a long build or test run.",
                timeout.as_secs()
            )),
            _ = ctx.cancel.cancelled() => Some("Interrupted by the user.".to_string()),
        }
    } else {
        None
    };
    if let Some(why) = stopped {
        request_stop(&job);
        let _ = tokio::time::timeout(Duration::from_secs(10), &mut done).await;
        let mut out = format_result(&job, ctx);
        out.content.push(tessera_proto::ContentPart::text(why));
        out.is_error = true;
        return out;
    }
    format_result(&job, ctx)
}

pub struct Job;

impl Tool for Job {
    fn name(&self) -> &'static str {
        "job"
    }
    fn description(&self) -> String {
        "Background shell jobs: `list`, `output` (new since last read), `wait` (up to `timeout` s, default 60), `kill`."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "output", "wait", "kill"]},
                "id": {"type": "string"},
                "timeout": {"type": "integer"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn action_kind(&self, args: &Value) -> ToolKind {
        match opt_str(args, "action").unwrap_or("list") {
            "list" | "output" | "wait" => ToolKind::Read,
            _ => ToolKind::Exec,
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        if opt_str(args, "action").unwrap_or("list") != "list"
            && let Some(id) = opt_str(args, "id")
            && own_job(id, &ctx.session_id).is_none()
        {
            return Risk::Block {
                reason: format!("No job `{id}` in this session. Use action=list."),
            };
        }
        Risk::None
    }
    fn concurrent(&self, args: &Value) -> bool {
        matches!(
            args.get("action").and_then(Value::as_str),
            Some("list" | "output")
        )
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let action = opt_str(&args, "action").unwrap_or("list");
            if action == "list" {
                let list = jobs()
                    .lock()
                    .values()
                    .filter(|j| j.session == ctx.session_id)
                    .cloned()
                    .collect::<Vec<_>>();
                if list.is_empty() {
                    return ToolOutput::text("No background jobs.");
                }
                let lines: Vec<String> = list
                    .iter()
                    .map(|j| {
                        let state = match *j.exit.lock() {
                            Some(c) => format!("exited {c}"),
                            None => "running".into(),
                        };
                        let cleanup = cleanup_state(j)
                            .map(|s| format!(" · {s}"))
                            .unwrap_or_default();
                        format!(
                            "{} · {state}{cleanup} · {:.0}s · {}",
                            j.id,
                            j.started.elapsed().as_secs_f32(),
                            crate::util::one_line(&j.command, 80)
                        )
                    })
                    .collect();
                return ToolOutput::text(lines.join("\n"));
            }
            let Some(id) = opt_str(&args, "id") else {
                return ToolOutput::error("`id` is required.");
            };
            let Some(job) = own_job(id, &ctx.session_id) else {
                return ToolOutput::error(format!("No job `{id}`. Use action=list."));
            };
            match action {
                "kill" => {
                    let done = job.done.notified();
                    let ended = job.exit.lock().is_some();
                    if !ended || job.stopped.lock().is_none() {
                        request_stop(&job);
                    }
                    if cleanup_pending(&job) {
                        let _ = tokio::time::timeout(Duration::from_secs(10), done).await;
                    }
                    let state = job.stopped.lock().clone().unwrap_or(Stopped {
                        not_checked: true,
                        incomplete: true,
                        ..Default::default()
                    });
                    // An expired wait must never hide the job while its worker
                    // still owns cleanup. Its final state is published later.
                    remove_published(&job);
                    let text =
                        cleanup_state(&job).unwrap_or_else(|| killed_text(id, ended, &state));
                    ToolOutput::text(text)
                        .with_details(json!({"job_id": id, "cleanup_pending": state.pending,
                            "output_draining": !state.pending && job.exit.lock().is_none()}))
                }
                "wait" => {
                    let secs = opt_u64(&args, "timeout").unwrap_or(60).clamp(1, 1800);
                    if job.exit.lock().is_none() || cleanup_pending(&job) {
                        tokio::select! {
                            _ = job.done.notified() => {}
                            _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
                            _ = ctx.cancel.cancelled() => {}
                        }
                    }
                    let out = format_result(&job, ctx);
                    remove_published(&job);
                    out
                }
                _ => {
                    let all = job.output.lock().clone();
                    let mut pos = job.read_pos.lock();
                    let start = crate::util::floor_char(&all, (*pos).min(all.len()));
                    let fresh = all[start..].to_string();
                    *pos = all.len();
                    drop(pos);
                    let state = match *job.exit.lock() {
                        Some(c) => format!("[exited {c}]"),
                        None => format!("[running · {:.0}s]", job.started.elapsed().as_secs_f32()),
                    };
                    let state = match cleanup_state(&job) {
                        Some(cleanup) => format!("{state}\n{cleanup}"),
                        None => state,
                    };
                    let (body, _) = super::fit_output(fresh.trim_end(), ctx, "job output");
                    ToolOutput::text(if body.is_empty() {
                        format!("(no new output) {state}")
                    } else {
                        format!("{body}\n{state}")
                    })
                }
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn tree_watcher_pins_startup_forks_then_slows_down() {
        assert_eq!(
            tree_poll_interval(Duration::ZERO),
            Duration::from_millis(20)
        );
        assert_eq!(
            tree_poll_interval(Duration::from_millis(1999)),
            Duration::from_millis(20)
        );
        assert_eq!(tree_poll_interval(Duration::from_secs(2)), TREE_POLL_STEADY);
        assert_eq!(
            tree_poll_interval(Duration::from_secs(60)),
            TREE_POLL_STEADY
        );
        assert_eq!(TREE_POLL_STEADY, Duration::from_millis(500));
        assert_eq!(KEPT_TREE_POLL, Duration::from_millis(250));
    }
    #[cfg(target_os = "linux")]
    pub(super) fn wait_child(child: &mut std::process::Child) -> std::process::ExitStatus {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < until,
                "child {} did not reap within 5s",
                child.id()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn subprocess_check(name: &str, env: &[(&str, &str)], deadline: Duration) {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .envs(env.iter().copied())
            .spawn()
            .unwrap();
        let guard = ChildTree::capture(child.id());
        let until = Instant::now() + deadline;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= until {
                guard.stop();
                let _ = child.kill();
                let _ = wait_child(&mut child);
                panic!("{name}: subprocess exceeded {deadline:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        guard.disarm();
        assert!(status.success(), "{name}: {status}");
    }

    /// Real tree fixtures need pidfds and a test-only adopter so an orphaned
    /// zombie is actually reaped, not hidden by a 'no running processes' check.
    #[cfg(target_os = "linux")]
    pub(crate) fn cleanup_fixture(name: &str) -> bool {
        if std::env::var_os("TESSERA_CLEANUP_FIXTURE").is_none() {
            subprocess_check(
                name,
                &[("TESSERA_CLEANUP_FIXTURE", "1")],
                Duration::from_secs(40),
            );
            return false;
        }
        if !tree::real_capability_for_test() {
            return false;
        }
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::EINVAL | libc::EPERM | libc::EACCES)
            ) {
                eprintln!("SKIP real cleanup fixture: test adopter unavailable: {error}");
                return false;
            }
            panic!("test adopter failed: {error}");
        }
        true
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn reaped_disappearance(pids: &[i32]) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            // All direct Child owners have already waited before this call.
            while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
            if pids
                .iter()
                .all(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists())
            {
                return;
            }
            if Instant::now() >= until {
                let states = pids
                    .iter()
                    .map(|pid| (*pid, std::fs::read_to_string(format!("/proc/{pid}/stat"))))
                    .collect::<Vec<_>>();
                panic!("processes did not disappear (including zombies): {states:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// What an agent starts goes before the server when memory runs out.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_launch_enters_the_verified_object_after_a_name_swap() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let outside = temp.path().join("outside");
        let saved = temp.path().join("saved");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(path.join("marker"), "checked").unwrap();
        std::fs::write(outside.join("marker"), "outside").unwrap();
        let directory = super::super::file_object::DirectoryScope::capture(&path)
            .unwrap()
            .open()
            .unwrap();
        std::fs::rename(&path, &saved).unwrap();

        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let job = spawn_job(
            (
                "swap-regression",
                &super::super::admission::ActionAdmission::default(),
            ),
            "cat marker; printf original > launched",
            directory,
            vec![],
            vec![],
            None,
            None,
            Launch::Command,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while job.exit.lock().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(*job.exit.lock(), Some(0));
        assert_eq!(job.output.lock().trim(), "checked");
        assert_eq!(
            std::fs::read_to_string(saved.join("launched")).unwrap(),
            "original"
        );
        assert!(!outside.join("launched").exists());
    }

    #[cfg(target_os = "linux")]
    async fn finished(job: &BgJob) -> (Option<i32>, String) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while job.exit.lock().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        (*job.exit.lock(), job.output.lock().clone())
    }

    /// The Bash tool's launcher in the command sandbox (whichever mode this
    /// computer has; `TESSERA_TEST_LANDLOCK_ABI=1` for the shared one): paths
    /// relative to the working folder see the sandbox's view, and a folder
    /// replaced after its check runs nothing.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandboxed_bash_launch_keeps_relative_paths_inside_and_refuses_a_swapped_folder() {
        if crate::sandbox::test_mode("sandboxed bash launch").is_none() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let work = base.join("work");
        let hidden = base.join("hidden");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("secret"), "hidden sentinel").unwrap();
        let spec = crate::sandbox::Spec {
            hide: vec![hidden.clone()],
            folders: vec![(base.clone(), false), (work.clone(), true)],
            group: "sandboxed bash launch".into(),
            ..Default::default()
        };
        let admission = super::super::admission::ActionAdmission::default();
        let start = |command: &str, swap: bool| {
            let directory = super::super::file_object::DirectoryScope::capture(&work)
                .unwrap()
                .open()
                .unwrap();
            let sandbox = directory.prepare(Some(&(spec.clone(), false))).unwrap();
            assert!(sandbox.is_some());
            if swap {
                std::fs::rename(&work, base.join("saved")).unwrap();
                std::fs::create_dir(&work).unwrap();
            }
            spawn_job(
                ("sandboxed-launch", &admission),
                command,
                directory,
                vec![],
                vec![],
                sandbox,
                None,
                Launch::Command,
            )
        };
        let up = "../".repeat(work.components().count() - 1);
        let job = start(
            &format!(
                "cat ../hidden/secret 2>/dev/null || echo hidden-unreadable; \
                 [ {up}proc -ef /proc ] && echo proc-same-view; \
                 echo x 2>/dev/null > ../outside-work && echo WROTE || echo refused-ro; \
                 echo ok > inside && echo wrote-inside"
            ),
            false,
        )
        .unwrap();
        let (code, out) = finished(&job).await;
        assert_eq!(code, Some(0), "{out}");
        for want in [
            "hidden-unreadable",
            "proc-same-view",
            "refused-ro",
            "wrote-inside",
        ] {
            assert!(out.contains(want), "{want}: {out}");
        }
        assert!(!out.contains("sentinel") && !out.contains("WROTE"), "{out}");
        assert!(work.join("inside").is_file() && !base.join("outside-work").exists());

        match start("echo ran > launched", true) {
            Err(e) => assert!(e.contains("Nothing was run"), "{e}"),
            Ok(job) => {
                let (code, out) = finished(&job).await;
                assert_eq!(code, Some(crate::sandbox::FOLDER_CHANGED_STATUS), "{out}");
                assert!(out.contains("Nothing was run"), "{out}");
            }
        }
        assert!(!work.join("launched").exists() && !base.join("saved/launched").exists());
    }

    /// Running processes whose arguments include `arg`.
    #[cfg(target_os = "linux")]
    fn running_with(arg: &str) -> Vec<i32> {
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let pid: i32 = e.file_name().to_str()?.parse().ok()?;
                let args = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                let state = stat
                    .get(stat.rfind(')')? + 1..)?
                    .split_whitespace()
                    .next()?;
                (state != "Z" && args.split(|b| *b == 0).any(|a| a == arg.as_bytes()))
                    .then_some(pid)
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    async fn job_in_temp(command: &str) -> (tempfile::TempDir, Arc<BgJob>) {
        let temp = tempfile::tempdir().unwrap();
        let directory = super::super::file_object::DirectoryScope::capture(temp.path())
            .unwrap()
            .open()
            .unwrap();
        let job = spawn_job(
            (
                "tree-kill",
                &super::super::admission::ActionAdmission::default(),
            ),
            command,
            directory,
            vec![],
            vec![],
            None,
            None,
            Launch::Command,
        )
        .unwrap();
        (temp, job)
    }

    /// Stop retains owned handles and also finds marker-bearing setsid children.
    /// The test-only adopter reaps orphans; production has no per-job subreaper.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn killing_a_job_stops_its_whole_tree() {
        if !cleanup_fixture("tools::bash::tests::killing_a_job_stops_its_whole_tree") {
            return;
        }
        let (_temp, job) = job_in_temp(
            "timeout 300 sleep 3011 & setsid sleep 3012 & (setsid sleep 3013 &); sleep 3014 & echo started; wait",
        )
        .await;
        let args = ["3011", "3012", "3013", "3014"];
        let started = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let n: usize = args.iter().map(|a| running_with(a).len()).sum();
                // `timeout` and its `sleep`, and the three other sleeps.
                if n >= 5 && job.output.lock().contains("started") {
                    break n;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the job's processes start");
        assert_eq!(started, 5);
        let mut pids = vec![job.pid as i32];
        pids.extend(args.iter().flat_map(|arg| running_with(arg)));

        let done = job.done.notified();
        job.kill.cancel();
        tokio::time::timeout(Duration::from_secs(10), done)
            .await
            .unwrap();
        let stopped = job.stopped.lock().take().unwrap();
        assert!(
            stopped.left.iter().all(|pid| *pid <= 0),
            "known live processes remain: {stopped:?}"
        );
        assert!(stopped.signalled >= 5, "{stopped:?}");
        for a in args {
            assert!(running_with(a).is_empty(), "sleep {a} survived");
        }
        assert_eq!(*job.exit.lock(), Some(-9));
        assert!(killed_text(&job.id, false, &stopped).starts_with("Stopped `job-"));
        reaped_disappearance(&pids);

        // A job whose shell already ended: what it left running is stopped.
        let (_temp, job) = job_in_temp("setsid sleep 3015 & echo ok").await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while job.exit.lock().is_none() || running_with("3015").is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let pids = running_with("3015");
        let stopped = stop_job(&job, false).await;
        assert!(
            stopped.left.iter().all(|pid| *pid <= 0) && stopped.signalled == 1,
            "{stopped:?}"
        );
        assert!(running_with("3015").is_empty());
        assert!(killed_text(&job.id, true, &stopped).contains("had already ended"));
        // A shell that ends before its tree is captured leaves only the marker
        // search, which honestly can't claim a clean tree; what it left is
        // still stopped (checked above).
        if !stopped.incomplete {
            assert!(
                killed_text(&job.id, true, &stopped).contains("known processes"),
                "{stopped:?}: {}",
                killed_text(&job.id, true, &stopped)
            );
        }
        reaped_disappearance(&pids);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn killing_a_job_discovers_env_i_setsid_grandchild_after_leader_dies() {
        if !cleanup_fixture(
            "tools::bash::tests::killing_a_job_discovers_env_i_setsid_grandchild_after_leader_dies",
        ) {
            return;
        }
        let (_temp, job) = job_in_temp(
            "env -i /bin/sh -c 'trap \"\" TERM; parent=$1; echo child-ready; while kill -0 \"$parent\" 2>/dev/null; do read -r _ _ state rest < \"/proc/$parent/stat\" || break; case \"$state\" in Z|X|x) break;; esac; /bin/sleep 0.02; done; /usr/bin/setsid /bin/sleep 3019 & echo forked-after-leader:$!; wait' child \"$$\" & wait",
        ).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !job.output.lock().contains("child-ready") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let done = job.done.notified();
        job.kill.cancel();
        tokio::time::timeout(Duration::from_secs(10), done)
            .await
            .unwrap();
        let stopped = job.stopped.lock().take().unwrap();
        assert!(
            job.output.lock().contains("forked-after-leader"),
            "child never exercised post-leader fork"
        );
        assert!(
            stopped.signalled >= 3 && stopped.left.iter().all(|pid| *pid <= 0),
            "{stopped:?}"
        );
        assert!(
            running_with("3019").is_empty(),
            "setsid grandchild survived"
        );
        let output = job.output.lock();
        let grandchild: i32 = output
            .lines()
            .find_map(|line| line.strip_prefix("forked-after-leader:"))
            .unwrap()
            .parse()
            .unwrap();
        drop(output);
        reaped_disappearance(&[job.pid as i32, grandchild]);
    }
    #[cfg(target_os = "linux")]
    async fn kill_env_cleared_job(command: &str, argument: &str, leader: bool) {
        let (_temp, job) = job_in_temp(command).await;
        let pid = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(pid) = running_with(argument).first().copied() {
                    let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
                    if env.is_empty() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("env -i sleep starts with an empty environment");
        assert_eq!(pid == job.pid as i32, leader);
        let done = job.done.notified();
        job.kill.cancel();
        tokio::time::timeout(Duration::from_secs(10), done)
            .await
            .unwrap();
        let stopped = job.stopped.lock().take().unwrap();
        assert!(
            stopped.left.iter().all(|pid| *pid <= 0),
            "known live processes remain: {stopped:?}"
        );
        assert!(
            stopped.signalled >= if leader { 1 } else { 2 },
            "{stopped:?}"
        );
        assert!(
            running_with(argument).is_empty(),
            "env -i sleep survived: {pid}"
        );
        reaped_disappearance(&[job.pid as i32, pid]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn killing_a_job_stops_its_leader_after_exec_env_i() {
        if !cleanup_fixture("tools::bash::tests::killing_a_job_stops_its_leader_after_exec_env_i") {
            return;
        }
        kill_env_cleared_job("exec env -i /bin/sleep 3017", "3017", true).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn killing_a_job_stops_a_descendant_after_env_i() {
        if !cleanup_fixture("tools::bash::tests::killing_a_job_stops_a_descendant_after_env_i") {
            return;
        }
        // SIGTERM exits the owned leader; this child ignores it across exec,
        // so SIGKILL must use its recorded birth identity after reparenting.
        kill_env_cleared_job(
            "env -i /bin/sh -c 'trap \"\" TERM; exec env -i /bin/sleep 3018' & wait",
            "3018",
            false,
        )
        .await;
    }
    /// Treat a current, unrelated job's pid as a recycled ended-job id.
    /// Group/session matches alone must not stop it; its marker is different.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn ended_job_cleanup_does_not_signal_a_reused_group() {
        if !cleanup_fixture("tools::bash::tests::ended_job_cleanup_does_not_signal_a_reused_group")
        {
            return;
        }
        let (_temp, job) = job_in_temp("echo ready; sleep 3016").await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !job.output.lock().contains("ready") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let stopped = tree::stop_marker(format!("{JOB_MARKER}=a-different-ended-job"));
        let intact = job.exit.lock().is_none();
        let done = job.done.notified();
        job.kill.cancel();
        tokio::time::timeout(Duration::from_secs(10), done)
            .await
            .unwrap();
        assert_eq!(stopped.signalled, 0, "{stopped:?}");
        // A full /proc scan may encounter another user's unreadable environ:
        // that is correctly disclosed as uncertainty, never a false clean.
        assert_eq!(
            stopped.left.iter().filter(|pid| **pid > 0).count(),
            0,
            "{stopped:?}"
        );
        assert!(intact, "cleanup stopped an unrelated group/session");
    }

    #[test]
    fn read_only_command_disclosure_names_unconfined_project_code() {
        let description = ReadOnlyBash.description();
        assert!(description.chars().count() <= 600);
        for text in [
            description.as_str(),
            read_only_commands(false),
            &list_refusal("not listed"),
        ] {
            assert!(
                text.contains("build scripts and tests unconfined"),
                "{text}"
            );
            assert!(text.contains("change files and use the network"), "{text}");
            assert!(!text.contains("can't change anything"), "{text}");
        }
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn agent_work_is_first_in_line_when_memory_runs_out() {
        let mut cmd = tokio::process::Command::new("cat");
        cmd.arg("/proc/self/oom_score_adj");
        // SAFETY: yield_to_server makes plain system calls only.
        unsafe {
            cmd.pre_exec(|| {
                yield_to_server();
                Ok(())
            });
        }
        let out = cmd.output().await.unwrap();
        let child: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        let me: i32 = std::fs::read_to_string("/proc/self/oom_score_adj")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            child >= 500 && child > me.min(499),
            "child {child}, server {me}"
        );
    }

    fn recorded_job(id: &str, session: &str, exit: Option<i32>, pending: bool) -> Arc<BgJob> {
        Arc::new(BgJob {
            id: id.into(),
            session: session.into(),
            command: "fixture".into(),
            started: Instant::now(),
            output: Mutex::new("fixture output".into()),
            exit: Mutex::new(exit),
            done: tokio::sync::Notify::new(),
            kill: CancellationToken::new(),
            read_pos: Mutex::new(0),
            #[cfg(target_os = "linux")]
            pid: 0,
            #[cfg(windows)]
            windows_job: None,
            #[cfg(target_os = "linux")]
            tree: None,
            stopped: Mutex::new(Some(Stopped {
                pending,
                ..Default::default()
            })),
            joined_namespace: false,
        })
    }

    #[test]
    fn pending_caps_refuse_session_eight_and_global_thirty_two_without_pruning() {
        let mut all = HashMap::new();
        for n in 0..7 {
            let job = recorded_job(&format!("cap{n}"), "own", Some(-9), true);
            all.insert(job.id.clone(), job);
        }
        assert!(pending_command_refusal(&all, "own").is_none());
        let eighth = recorded_job("cap7", "own", None, true);
        all.insert(eighth.id.clone(), eighth);
        assert_eq!(
            pending_command_refusal(&all, "own").unwrap(),
            "8 earlier commands are still being cleaned up; wait or check `job list`."
        );
        assert!(pending_command_refusal(&all, "another").is_none());
        for n in 8..31 {
            let job = recorded_job(&format!("cap{n}"), "other", Some(-9), true);
            all.insert(job.id.clone(), job);
        }
        assert!(pending_command_refusal(&all, "new").is_none());
        let last = recorded_job("cap31", "other", Some(-9), true);
        all.insert(last.id.clone(), last);
        assert_eq!(
            pending_command_refusal(&all, "new").unwrap(),
            "32 earlier commands are still being cleaned up; wait or check `job list`."
        );
        drop_oldest_finished(&mut all, 0);
        assert_eq!(
            all.len(),
            32,
            "pending records must never be pruned to admit a command"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pending_cap_refuses_both_command_entry_points_before_spawn() {
        if std::env::var_os("TESSERA_PENDING_CAP_CASE").is_none() {
            subprocess_check(
                "tools::bash::tests::pending_cap_refuses_both_command_entry_points_before_spawn",
                &[("TESSERA_PENDING_CAP_CASE", "1")],
                Duration::from_secs(10),
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for n in 0..8 {
            let job = recorded_job(&format!("admit{n}"), "blocked", Some(-9), true);
            jobs().lock().insert(job.id.clone(), job);
        }
        for launch in [Launch::Command, Launch::Reading { offline: false }] {
            let directory = crate::tools::file_object::ProcessDirectory::open(dir.path()).unwrap();
            let result = spawn_job(
                ("blocked", &Default::default()),
                "echo must-not-spawn",
                directory,
                vec![],
                vec![],
                None,
                None,
                launch,
            );
            assert!(
                matches!(result, Err(ref message) if message.starts_with("8 earlier commands are still being cleaned up"))
            );
        }
    }

    #[tokio::test]
    async fn read_only_session_can_read_own_pending_jobs_but_not_kill_or_cross_sessions() {
        let (ctx, _dir) = crate::engine::test_support::read_only_ctx().await;
        let own = recorded_job(&crate::util::new_id(), &ctx.session_id, None, true);
        let other = recorded_job(
            &crate::util::new_id(),
            "unrelated-fixture-session",
            None,
            true,
        );
        jobs().lock().insert(own.id.clone(), own.clone());
        jobs().lock().insert(other.id.clone(), other.clone());
        struct Remove(Vec<String>);
        impl Drop for Remove {
            fn drop(&mut self) {
                for id in &self.0 {
                    jobs().lock().remove(id);
                }
            }
        }
        let _remove = Remove(vec![own.id.clone(), other.id.clone()]);
        // A read-only chat keeps `job` for its own kept work; a read-only
        // helper's restricted runner can't own kept jobs, so it isn't offered.
        let offered = |depth| {
            super::super::Registry::builtin()
                .specs(tessera_proto::PermissionMode::ReadOnly, depth, false)
                .iter()
                .any(|tool| tool.name == "job")
        };
        assert!(offered(0));
        assert!(!offered(1));
        assert_eq!(
            Job.description(),
            "Background shell jobs: `list`, `output` (new since last read), `wait` (up to `timeout` s, default 60), `kill`."
        );
        ctx.cancel.cancel(); // wait should return pending without a test delay
        for action in ["list", "output", "wait"] {
            let args = json!({"action": action, "id": own.id, "timeout": 1});
            assert_eq!(Job.action_kind(&args), ToolKind::Read);
            assert!(matches!(
                super::super::permission_risk(
                    &Job,
                    &args,
                    &ctx,
                    tessera_proto::PermissionMode::ReadOnly
                ),
                Risk::None
            ));
            let output = Job.run(args, &ctx).await;
            assert!(!output.is_error, "{action}: {output:?}");
            let text = tessera_proto::plain_text(&output.content);
            assert!(text.contains("cleanup still running"), "{text}");
            assert!(!text.contains(&other.id), "list leaked another session");
        }
        let kill = json!({"action": "kill", "id": own.id});
        assert_eq!(Job.action_kind(&kill), ToolKind::Exec);
        assert!(matches!(
            super::super::permission_risk(
                &Job,
                &kill,
                &ctx,
                tessera_proto::PermissionMode::ReadOnly
            ),
            Risk::Block { .. }
        ));
        for action in ["output", "wait", "kill"] {
            let args = json!({"action": action, "id": other.id});
            assert!(matches!(
                super::super::permission_risk(
                    &Job,
                    &args,
                    &ctx,
                    tessera_proto::PermissionMode::ReadOnly
                ),
                Risk::Block { .. }
            ));
            assert!(
                Job.run(args, &ctx).await.is_error,
                "run must also enforce own-session scope"
            );
        }
        assert!(
            jobs().lock().contains_key(&own.id),
            "reading a pending job removed it"
        );
    }

    #[tokio::test]
    async fn repeated_kill_and_wait_keep_draining_record_until_final_exit_is_published() {
        let (ctx, _dir) = crate::engine::test_support::read_only_ctx().await;
        let job = recorded_job(&crate::util::new_id(), &ctx.session_id, None, false);
        job.kill.cancel();
        jobs().lock().insert(job.id.clone(), job.clone());
        ctx.cancel.cancel();
        assert!(!publication_complete(&job));
        assert!(
            cleanup_state(&job)
                .unwrap()
                .contains("Tree cleaned up; output still draining")
        );
        assert!(
            !cleanup_state(&job)
                .unwrap()
                .contains("cleanup still running")
        );
        for action in ["kill", "kill", "wait"] {
            let result = Job
                .run(json!({"action": action, "id": job.id, "timeout": 1}), &ctx)
                .await;
            assert!(!result.is_error, "{action}: {result:?}");
            assert!(tessera_proto::plain_text(&result.content).contains("output still draining"));
            assert!(
                jobs().lock().contains_key(&job.id),
                "{action} removed the record before final drain/exit"
            );
        }
        // Drain can still append output until it publishes exit. Retention
        // and repeated kill must not discard that final output/state.
        job.output.lock().push_str("\nlast drained line");
        *job.exit.lock() = Some(0);
        job.done.notify_waiters();
        assert!(publication_complete(&job));
        assert!(
            !cleanup_state(&job)
                .unwrap()
                .contains("output still draining")
        );
        let result = Job.run(json!({"action": "wait", "id": job.id}), &ctx).await;
        assert!(tessera_proto::plain_text(&result.content).contains("last drained line"));
        assert!(!jobs().lock().contains_key(&job.id));
    }

    #[test]
    fn wait_removal_rechecks_pending_publication_in_registry_critical_section() {
        let job = recorded_job("wait-pending-race", "fixture", Some(0), false);
        *job.stopped.lock() = None; // normal completed command, not yet stopped
        let all = Arc::new(Mutex::new(HashMap::from([(job.id.clone(), job.clone())])));
        let (snapshot_ready, snapshot_taken) = std::sync::mpsc::channel();
        let (publication_ready, published) = std::sync::mpsc::channel();
        let (wait_job, wait_all) = (job.clone(), all.clone());
        let waiter = std::thread::spawn(move || {
            assert!(publication_complete(&wait_job)); // original stale snapshot
            snapshot_ready.send(()).unwrap();
            published.recv_timeout(Duration::from_secs(2)).unwrap();
            remove_published_locked(&mut wait_all.lock(), &wait_job)
        });
        snapshot_taken.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(publish_stop_locked(&mut all.lock(), &job));
        publication_ready.send(()).unwrap();
        assert!(
            !waiter.join().unwrap(),
            "wait removed the newly pending cleanup"
        );
        assert!(all.lock().contains_key(&job.id));
        assert!(cleanup_pending(&job));
        *job.stopped.lock() = Some(Stopped::default());
        assert!(remove_published_locked(&mut all.lock(), &job));
    }

    #[test]
    fn only_the_newest_finished_jobs_are_kept() {
        let job = |n: u64, exited: bool| {
            Arc::new(BgJob {
                id: format!("j{n}"),
                session: "s".into(),
                command: "true".into(),
                started: Instant::now() + Duration::from_secs(n),
                output: Mutex::new(String::new()),
                exit: Mutex::new(exited.then_some(0)),
                done: tokio::sync::Notify::new(),
                kill: CancellationToken::new(),
                read_pos: Mutex::new(0),
                #[cfg(target_os = "linux")]
                pid: 0,
                #[cfg(windows)]
                windows_job: None,
                #[cfg(target_os = "linux")]
                tree: None,
                stopped: Mutex::new(None),
                joined_namespace: false,
            })
        };
        let mut all: HashMap<String, Arc<BgJob>> = HashMap::new();
        for n in 0..10 {
            let j = job(n, n % 2 == 0);
            all.insert(j.id.clone(), j);
        }
        drop_oldest_finished(&mut all, 3);
        let mut left: Vec<&str> = all.keys().map(String::as_str).collect();
        left.sort();
        // Every running one stays; of the finished (0 2 4 6 8), the newest three.
        assert_eq!(left, ["j1", "j3", "j4", "j5", "j6", "j7", "j8", "j9"]);
        // An ended command is still addressable while cleanup is pending,
        // even when the finished-job retention budget is zero.
        let pending = job(10, true);
        *pending.stopped.lock() = Some(Stopped {
            pending: true,
            ..Default::default()
        });
        all.insert(pending.id.clone(), pending.clone());
        drop_oldest_finished(&mut all, 0);
        assert!(all.contains_key(&pending.id));
        assert!(
            cleanup_state(&pending)
                .unwrap()
                .contains("Stopped; cleanup still running (job `j10`)")
        );
        *pending.stopped.lock() = Some(Stopped {
            not_checked: true,
            incomplete: true,
            ..Default::default()
        });
        assert!(!cleanup_pending(&pending));
        assert!(
            cleanup_state(&pending)
                .unwrap()
                .contains("cleanup not checked")
        );
        // A completed cleanup still retains its record while output drains.
        *pending.exit.lock() = None;
        assert!(!publication_complete(&pending));
        assert!(
            cleanup_state(&pending)
                .unwrap()
                .contains("Cleanup attempt finished; output still draining")
        );
        drop_oldest_finished(&mut all, 0);
        assert!(all.contains_key(&pending.id));
        *pending.exit.lock() = Some(-9);
        assert!(publication_complete(&pending));
        drop_oldest_finished(&mut all, 0);
        assert!(!all.contains_key(&pending.id));
    }

    #[test]
    fn incomplete_cleanup_state_is_separate_from_exit_code_and_names_joined_limits() {
        let job = BgJob {
            id: "cleanup-fixture".into(),
            session: "s".into(),
            command: "true".into(),
            started: Instant::now(),
            output: Mutex::new(String::new()),
            exit: Mutex::new(Some(-9)),
            done: tokio::sync::Notify::new(),
            kill: CancellationToken::new(),
            read_pos: Mutex::new(0),
            #[cfg(all(test, target_os = "linux"))]
            pid: 0,
            #[cfg(windows)]
            windows_job: None,
            #[cfg(target_os = "linux")]
            tree: None,
            stopped: Mutex::new(Some(Stopped {
                incomplete: true,
                joined_namespace: true,
                left: vec![-1],
                ..Default::default()
            })),
            joined_namespace: true,
        };
        let state = cleanup_state(&job).unwrap();
        assert!(
            state.contains("stopped; some processes could not be checked"),
            "{state}"
        );
        assert!(state.contains("best-effort, not namespace-wide"), "{state}");
        assert!(!state.contains("pid -1"));
        assert!(!state.contains("nothing it started"));
        assert_eq!(*job.exit.lock(), Some(-9));
        for remaining in [0, 2] {
            let text = killed_text(
                "owned-job",
                false,
                &Stopped {
                    remaining: Some(remaining),
                    ..Default::default()
                },
            );
            assert!(text.contains("Windows Job checked"));
            assert_eq!(text.contains("no processes still running"), remaining == 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stop_expiry_keeps_pending_job_addressable_until_final_cleanup() {
        if !cleanup_fixture(
            "tools::bash::tests::stop_expiry_keeps_pending_job_addressable_until_final_cleanup",
        ) {
            return;
        }
        let (_temp, job) = job_in_temp("exec /bin/sleep 3039").await;
        let tree = job.tree.as_ref().unwrap().clone();
        let (locked, ready) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = tree.lock();
            locked.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(350));
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let done = job.done.notified();
        request_stop(&job);
        assert!(
            tokio::time::timeout(Duration::from_millis(40), done)
                .await
                .is_err()
        );
        assert!(cleanup_pending(&job));
        assert!(
            cleanup_state(&job)
                .unwrap()
                .contains("cleanup still running")
        );
        assert!(jobs().lock().contains_key(&job.id));
        holder.join().unwrap();
        if job.exit.lock().is_none() {
            tokio::time::timeout(Duration::from_secs(8), job.done.notified())
                .await
                .unwrap();
        }
        assert!(!cleanup_pending(&job));
        assert!(
            !cleanup_state(&job)
                .unwrap()
                .contains("cleanup still running")
        );
        assert!(
            jobs().lock().contains_key(&job.id),
            "final result should remain readable"
        );
        jobs().lock().remove(&job.id);
        reaped_disappearance(&[job.pid as i32]);
    }
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn observer_mutex_wait_does_not_block_tokio_heartbeat() {
        if !cleanup_fixture(
            "tools::bash::tests::observer_mutex_wait_does_not_block_tokio_heartbeat",
        ) {
            return;
        }
        let (_temp, job) = job_in_temp("exec /bin/sleep 3038").await;
        let tree = job.tree.as_ref().unwrap().clone();
        let (locked, ready) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = tree.lock();
            locked.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        tokio::time::sleep(Duration::from_millis(80)).await;
        // With the observer running tree.lock() on this sole worker, this
        // timer cannot be polled until the 300ms holder releases it.
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "observer blocked Tokio for {:?}",
            start.elapsed()
        );
        holder.join().unwrap();
        let done = job.done.notified();
        job.kill.cancel();
        tokio::time::timeout(Duration::from_secs(8), done)
            .await
            .unwrap();
        reaped_disappearance(&[job.pid as i32]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn external_guard_drop_cleans_error_timeout_and_cancelled_calls() {
        if !cleanup_fixture(
            "tools::bash::tests::external_guard_drop_cleans_error_timeout_and_cancelled_calls",
        ) {
            return;
        }
        for case in ["error", "timeout", "cancel"] {
            let mut child = std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let pid = child.id();
            let guard = ChildTree::capture(pid);
            assert!(
                guard.tree.is_some(),
                "fixture failed to capture child handle"
            );
            match case {
                "error" => {
                    let failure = async move {
                        let _guard = guard;
                        Err::<(), _>("fixture I/O failure")
                    }
                    .await;
                    assert!(failure.is_err());
                }
                "timeout" => {
                    assert!(
                        tokio::time::timeout(Duration::from_millis(20), async move {
                            let _guard = guard;
                            std::future::pending::<()>().await;
                        })
                        .await
                        .is_err()
                    );
                }
                _ => {
                    let (started, ready) = tokio::sync::oneshot::channel();
                    let task = tokio::spawn(async move {
                        let _guard = guard;
                        let _ = started.send(());
                        std::future::pending::<()>().await;
                    });
                    ready.await.unwrap();
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                }
            }
            let until = Instant::now() + Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= until {
                    let _ = child.kill();
                    let _ = wait_child(&mut child);
                    panic!("{case}: dropped guard left child running");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            reaped_disappearance(&[pid as i32]);
        }
        // Accepted completion is the only way to discard ownership without
        // cleanup; the test itself remains responsible for this live child.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        ChildTree::capture(child.id()).disarm();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let still_running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = wait_child(&mut child);
        assert!(still_running, "disarmed guard killed an accepted child");
        reaped_disappearance(&[child.id() as i32]);
    }

    #[test]
    fn only_named_non_secret_environment_is_inherited() {
        let names = [
            "PATH",
            "HOME",
            "LC_ALL",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "VIRTUAL_ENV",
            "SSH_AUTH_SOCK",
            "XDG_CONFIG_DIRS",
            "CLAUDE_CODE_GIT_BASH_PATH",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "OTHER_VALUE",
            "BASH_ENV",
            "NODE_OPTIONS",
            "TESSERA_HOME",
        ];
        let inherited = || names.iter().map(|n| ((*n).into(), "fixture".into()));
        let clean = agent_environment(inherited(), &[]);
        for name in &names[..9] {
            assert!(clean.iter().any(|(k, _)| k == name));
        }
        for name in &names[9..] {
            assert!(!clean.iter().any(|(k, _)| k == name));
        }
        let extra = agent_environment(inherited(), &["OTHER_VALUE".into(), "TESSERA_HOME".into()]);
        assert!(extra.iter().any(|(k, _)| k == "OTHER_VALUE"));
        assert!(!extra.iter().any(|(k, _)| k == "TESSERA_HOME"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleared_environment_keeps_explicit_harness_and_vault_values() {
        let mut cmd = tokio::process::Command::new("/usr/bin/env");
        cmd.env("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1")
            .env("TESSERA", "1");
        withhold_env(&mut cmd);
        cmd.env("FIXTURE_KEY", "requested-value");
        let out = String::from_utf8(cmd.output().await.unwrap().stdout).unwrap();
        assert!(out.contains("CLAUDE_CODE_ENABLE_TODO_TOOLS=1\n"));
        assert!(out.contains("TESSERA=1\n"));
        assert!(out.contains("FIXTURE_KEY=requested-value\n"));
        let expected = agent_environment(std::env::vars_os(), &[]);
        assert!(out.lines().all(|line| {
            let k = line.split_once('=').unwrap().0;
            ["CLAUDE_CODE_ENABLE_TODO_TOOLS", "TESSERA", "FIXTURE_KEY"].contains(&k)
                || expected.iter().any(|(name, _)| name == k)
        }));
    }

    #[test]
    fn provider_keys_are_not_inherited_but_vault_secrets_are() {
        let mut cmd = shell_command("true", None);
        withhold_env(&mut cmd);
        cmd.env("OPENAI_API_KEY", "from-the-vault");
        let envs: HashMap<_, _> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|v| v.to_os_string())))
            .collect();
        assert!(!envs.contains_key(std::ffi::OsStr::new("ANTHROPIC_API_KEY")));
        assert!(!envs.contains_key(std::ffi::OsStr::new("TESSERA_HOME")));
        assert_eq!(
            envs.get(std::ffi::OsStr::new("OPENAI_API_KEY")),
            Some(&Some("from-the-vault".into()))
        );
    }

    /// What an agent starts runs nicer than the server, so it can't starve it.
    #[cfg(target_os = "linux")]
    #[test]
    fn agent_commands_run_below_the_server() {
        let ours = std::fs::read_to_string("/proc/self/stat").unwrap();
        let nice = |stat: &str| -> i64 {
            // Fields after the ")" that ends the command name; niceness is field 19.
            let rest = &stat[stat.rfind(')').unwrap() + 2..];
            rest.split_whitespace().nth(16).unwrap().parse().unwrap()
        };
        let mut cmd = std::process::Command::new("cat");
        cmd.arg("/proc/self/stat");
        // SAFETY: only system calls on the child itself.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                yield_to_server();
                Ok(())
            });
        }
        let out = String::from_utf8(cmd.output().unwrap().stdout).unwrap();
        assert_eq!(nice(&out), (nice(&ours) + 10).min(19), "{out}");
    }

    #[test]
    fn ansi_is_stripped() {
        assert_eq!(
            strip_ansi("\x1b[31mred\x1b[0m\r\n10%\r50%\r100%\ndone"),
            "red\n100%\ndone"
        );
    }
}
