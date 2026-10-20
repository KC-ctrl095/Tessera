//! Windows system calls Tessera needs: process trees in Job Objects, whether
//! a process runs, a named event a server waits on to stop cleanly, file
//! locks, the real path of an open file, and the per-user "run at sign-in"
//! registry entries. Small wrappers; every handle is closed by its owner.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    STILL_ACTIVE, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetDriveTypeW, GetFileInformationByHandle,
    GetFinalPathNameByHandleW, GetLogicalDrives, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, GetSystemPowerStatus, SYSTEM_POWER_STATUS,
    SetThreadExecutionState,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW,
    RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess, GetExitCodeProcess,
    INFINITE, OpenEventW, OpenProcess, OpenThread, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, ResumeThread, SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
    THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
};

pub use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP,
    CREATE_NO_WINDOW, DETACHED_PROCESS,
};

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// A handle we own, or `None` for the null handle an API returns on failure.
fn owned(h: HANDLE) -> Option<OwnedHandle> {
    // SAFETY: a fresh, valid handle (checked non-null) that nobody else owns.
    (!h.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

fn raw(h: &impl AsRawHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

fn open_process(pid: u32, access: u32) -> Option<OwnedHandle> {
    // SAFETY: plain call; the result is checked.
    owned(unsafe { OpenProcess(access, 0, pid) })
}

// ---- processes ---------------------------------------------------------------

/// Whether process `pid` is still running.
pub fn alive(pid: u32) -> bool {
    let Some(p) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };
    let mut code = 0u32;
    // SAFETY: `p` is a valid process handle; `code` outlives the call.
    unsafe { GetExitCodeProcess(raw(&p), &mut code) != 0 && code == STILL_ACTIVE as u32 }
}

/// The program file of process `pid` (`C:\…\tessera.exe`).
pub fn image_path(pid: u32) -> Option<PathBuf> {
    let p = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has room for `len` characters.
    let ok = unsafe {
        QueryFullProcessImageNameW(raw(&p), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// Whether `pid` runs a program whose file name starts with `prefix`
/// (`tessera` matches `tessera.exe` and `tessera-service.exe`).
pub fn runs(pid: u32, prefix: &str) -> bool {
    image_path(pid)
        .and_then(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
        })
        .is_some_and(|n| n.starts_with(prefix))
}

/// End process `pid` at once (its children live on: see [`kill_tree`]).
pub fn terminate(pid: u32) -> bool {
    let Some(p) = open_process(pid, PROCESS_TERMINATE) else {
        return false;
    };
    // SAFETY: valid handle with terminate access.
    unsafe { TerminateProcess(raw(&p), 1) != 0 }
}

/// End this process at once with exit code `code`: no unwinding and no exit
/// handlers (a stuck thread's locks could hold those up), and no "stopped
/// working" box from Windows Error Reporting, which `abort` may bring up
/// and which keeps the process alive until someone clicks it. Whatever
/// started it (`tessera-service.exe`) sees the code and starts it again.
/// What it started in jobs that end with it ends too.
pub fn end_now(code: u32) -> ! {
    // SAFETY: the pseudo-handle of this very process.
    unsafe {
        TerminateProcess(GetCurrentProcess(), code);
    }
    std::process::abort()
}

/// Process trees by the pid that started them: a Job Object each, which
/// every process they start joins automatically.
fn jobs() -> &'static Mutex<Vec<(u32, OwnedHandle)>> {
    static J: std::sync::OnceLock<Mutex<Vec<(u32, OwnedHandle)>>> = std::sync::OnceLock::new();
    J.get_or_init(Default::default)
}

fn active_processes_checked(job: &OwnedHandle) -> std::io::Result<u32> {
    // SAFETY: plain-old-data struct, zero is a valid value.
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is the struct this class fills, with its exact size.
    let ok = unsafe {
        QueryInformationJobObject(
            raw(job),
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(info.ActiveProcesses)
    }
}

fn active_processes(job: &OwnedHandle) -> u32 {
    active_processes_checked(job).unwrap_or(u32::MAX)
}

/// Put the process tree started as `pid` in a Job Object of its own, so
/// [`kill_tree`] reaches everything it starts. Called right after the
/// start, before a shell has had time to start anything itself. Best
/// effort: without it, `kill_tree` falls back to `taskkill /T`.
pub fn adopt(pid: u32) {
    adopt_tree(pid, Job::Plain);
}

/// The kind of Job Object a process tree goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// Ends with nothing in particular; a program may leave it.
    Plain,
    /// A helper (the browser, cloudflared): it also ends when this process
    /// ends, however it ends (a crash, or the watchdog ending a stuck
    /// server), so it can't hold its profile or tunnel against the next
    /// start; a program may leave it.
    Helper,
    /// Agent work: nothing in it can leave it (a program that asks to is
    /// refused by Windows), and all of it ends when this process ends, so
    /// stopping it (or this machine going away) reaches everything it started.
    Agent,
    /// Private speech: killed with the server, cannot break away, bounded
    /// before its first instruction (including any subprocesses it starts).
    Voice { memory: usize },
}

/// Put the tree started as `pid` in a Job Object of kind `kind`. False if
/// it couldn't be.
fn adopt_tree(pid: u32, kind: Job) -> bool {
    let Some(process) = open_process(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE) else {
        return false;
    };
    // SAFETY: an unnamed job with default security.
    let Some(job) = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) }) else {
        return false;
    };
    let flags = match kind {
        // A program that asks to leave the job (as a daemon leaves a
        // process group on Linux) may; otherwise Windows would refuse to
        // start it.
        Job::Plain => JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        Job::Helper => JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        Job::Voice { .. } => JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY,
        Job::Agent => JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // SAFETY: plain-old-data struct, zero is a valid value; then the size
    // the class expects.
    let limited = unsafe {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = flags;
        if let Job::Voice { memory } = kind {
            limits.JobMemoryLimit = memory;
        }
        SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
    };
    if !limited {
        tracing::debug!(
            "couldn't set the limits of a job: {}",
            std::io::Error::last_os_error()
        );
        return false;
    }
    // SAFETY: both handles are valid.
    if unsafe { AssignProcessToJobObject(raw(&job), raw(&process)) } == 0 {
        tracing::debug!(
            "couldn't put process {pid} in a job: {}",
            std::io::Error::last_os_error()
        );
        return false;
    }
    let mut all = jobs().lock();
    // Forget trees that have ended (and any older entry for a reused pid).
    all.retain(|(p, j)| *p != pid && active_processes(j) > 0);
    all.push((pid, job));
    true
}

/// Start `cmd` with `flags`, suspended; put it in a Job Object of kind
/// `kind`; then let it run. Agent work that can't be put in its job is
/// ended before it ran a single instruction and the start fails (it would
/// run where stopping it couldn't reach everything it starts). A process
/// that can't be let run is ended too (it would wait forever).
pub fn spawn_in_job(
    cmd: &mut tokio::process::Command,
    flags: u32,
    kind: Job,
) -> std::io::Result<tokio::process::Child> {
    cmd.creation_flags(flags | CREATE_SUSPENDED);
    let child = cmd.spawn()?;
    let Some(pid) = child.id() else {
        return Ok(child);
    };
    if !adopt_tree(pid, kind) && matches!(kind, Job::Agent | Job::Voice { .. }) {
        terminate(pid);
        return Err(std::io::Error::other(
            "it couldn't be put where stopping it reaches everything it starts (a Windows job), so it wasn't run",
        ));
    }
    // Agent work on Windows is Tessera Guard's to end on the user's Stop,
    // whatever it (or this program) does: taken in before it runs, or not
    // run at all.
    if kind == Job::Agent {
        let taken = if crate::guard::windows::installed() {
            crate::guard::adopt_child(pid)
        } else {
            Err(crate::guard::NEEDED.to_string())
        };
        if let Err(why) = taken {
            terminate(pid);
            return Err(std::io::Error::other(format!("it wasn't run: {why}")));
        }
    }
    if !resume(pid) {
        terminate(pid);
        return Err(std::io::Error::other(
            "it was started but couldn't be let run",
        ));
    }
    Ok(child)
}

/// Let the threads of a process started suspended run (it has one then).
pub(crate) fn resume(pid: u32) -> bool {
    // SAFETY: a snapshot of the system's threads, walked with a correctly
    // sized entry; every handle is owned and closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let Some(snap) = owned(snap) else {
            return false;
        };
        let mut e: THREADENTRY32 = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        let mut more = Thread32First(raw(&snap), &mut e) != 0;
        while more {
            if e.th32OwnerProcessID == pid
                && let Some(t) = owned(OpenThread(THREAD_SUSPEND_RESUME, 0, e.th32ThreadID))
                && ResumeThread(raw(&t)) != u32::MAX
            {
                resumed = true;
            }
            more = Thread32Next(raw(&snap), &mut e) != 0;
        }
        resumed
    }
}

/// The Job Object of the tree started as `pid` (by [`spawn_in_job`]),
/// handed to the caller to keep for as long as the command is its own: it
/// ends the tree through it ([`TreeJob::end`]), never by a process id,
/// which may come to name another process once that one has ended.
pub fn take_job(pid: u32) -> Option<TreeJob> {
    let mut all = jobs().lock();
    let i = all.iter().position(|(p, _)| *p == pid)?;
    Some(TreeJob(all.swap_remove(i).1))
}

/// A command's own Job Object (see [`take_job`]).
pub struct TreeJob(OwnedHandle);

impl TreeJob {
    /// A failed query is not an empty Job. Keep the owned handle/profile
    /// until Windows positively confirms every helper has exited.
    pub async fn wait_empty(&self) {
        while !matches!(active_processes_checked(&self.0), Ok(0)) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// End every process still in it.
    pub fn end(&self) {
        // SAFETY: a job handle this owns.
        unsafe {
            TerminateJobObject(raw(&self.0), 1);
        }
    }

    /// Terminate only through this owned Job, then check its process count with
    /// a deadline. Call on a blocking worker, never on a Tokio worker.
    pub fn end_checked(&self, timeout: std::time::Duration) -> std::io::Result<u32> {
        if active_processes_checked(&self.0)? == 0 {
            return Ok(0);
        }
        // SAFETY: this object retains the owned Job handle, never a PID lookup.
        if unsafe { TerminateJobObject(raw(&self.0), 1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = active_processes_checked(&self.0)?;
            if remaining == 0 || std::time::Instant::now() >= deadline {
                return Ok(remaining);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Fire-and-forget shutdown for synchronous owners such as LSP Drop. Both
    /// termination and the bounded count check run away from the async worker.
    pub fn end_off_worker(self) {
        std::thread::spawn(move || {
            if !matches!(self.end_checked(std::time::Duration::from_secs(3)), Ok(0)) {
                tracing::warn!("Windows Job cleanup not checked or incomplete");
            }
        });
    }
}

/// Stop every process of the tree started as `pid`: its Job Object when it
/// has one, else `taskkill /T /F` (which follows parent links only).
pub fn kill_tree(pid: u32) {
    let job = {
        let mut all = jobs().lock();
        all.iter()
            .position(|(p, _)| *p == pid)
            .map(|i| all.swap_remove(i).1)
    };
    if let Some(job) = job {
        // SAFETY: valid job handle.
        if unsafe { TerminateJobObject(raw(&job), 1) } != 0 {
            return;
        }
    }
    let mut cmd = std::process::Command::new("taskkill");
    cmd.args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    super::no_window_std(&mut cmd);
    let _ = cmd.status();
}

// ---- asking a server to stop ---------------------------------------------------

/// The event process `pid` waits on to stop cleanly. `Local\`: this sign-in
/// session only; the default security lets only this user (and
/// administrators) open it.
fn stop_event_name(pid: u32) -> Vec<u16> {
    wide(format!("Local\\tessera-stop-{pid}"))
}

/// Resolves when another program asks this process to stop with
/// [`request_stop`] (what SIGTERM is on Linux). Never resolves if the event
/// can't be made.
pub async fn stop_requested() {
    let name = stop_event_name(std::process::id());
    // SAFETY: manual-reset event, initially unset, default security.
    let Some(event) = owned(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) }) else {
        tracing::warn!(
            "can't make the stop event: {}",
            std::io::Error::last_os_error()
        );
        return std::future::pending().await;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    // A plain thread, not `spawn_blocking`: the runtime would wait for it
    // when it shuts down.
    std::thread::spawn(move || {
        // SAFETY: valid event handle, kept alive by `event` in this thread.
        let r = unsafe { WaitForSingleObject(raw(&event), INFINITE) };
        if r == WAIT_OBJECT_0 {
            let _ = tx.send(());
        }
        drop(event);
    });
    if rx.await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Ctrl+C or Ctrl+Break, the console window closing, signing out or
/// shutting down, or [`request_stop`]: what SIGINT and SIGTERM are on Linux.
pub async fn stop_signal() {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let brk = async {
        match ctrl_break() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    let close = async {
        match ctrl_close() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    let logoff = async {
        match ctrl_logoff() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    let shutdown = async {
        match ctrl_shutdown() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        _ = interrupt => {}
        _ = stop_requested() => {}
        _ = brk => {}
        _ = close => {}
        _ = logoff => {}
        _ = shutdown => {}
    }
}

/// Ask process `pid` (a Tessera server or machine) to stop cleanly. False
/// when it doesn't listen for that (not a Tessera, or an older one).
pub fn request_stop(pid: u32) -> bool {
    let name = stop_event_name(pid);
    // SAFETY: plain call; the result is checked.
    let Some(event) = owned(unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) }) else {
        return false;
    };
    // SAFETY: valid event handle with modify access.
    unsafe { SetEvent(raw(&event)) != 0 }
}

/// Started by the desktop app, which ties this machine to its own lifetime
/// (a Windows job) right after starting it: nothing is done here before the
/// app says it has, by setting the event `TESSERA_START_GATE` names. An
/// error when it doesn't within 10 seconds (the app ends this process then
/// anyway); nothing to wait for without that variable.
pub fn wait_start_gate() -> std::io::Result<()> {
    let Some(name) = std::env::var_os("TESSERA_START_GATE") else {
        return Ok(());
    };
    let name = wide(&name);
    // SAFETY: plain call; the result is checked.
    let Some(event) = owned(unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) })
    else {
        return Err(std::io::Error::last_os_error());
    };
    // SAFETY: a valid event handle with synchronize access.
    match unsafe { WaitForSingleObject(raw(&event), 10_000) } {
        WAIT_OBJECT_0 => Ok(()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the app didn't tie this machine to itself in time",
        )),
    }
}

// ---- files -----------------------------------------------------------------------

/// Hold an exclusive lock on `file` (what `flock` does on Linux); false
/// when another process holds it.
pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
    // SAFETY: zeroed OVERLAPPED = offset 0, no event.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: valid file handle; `ov` outlives the (synchronous) call.
    let ok = unsafe {
        LockFileEx(
            raw(file),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut ov,
        ) != 0
    };
    super::windows_lock_result(if ok {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    })
}

/// How many names (hard links) an open file has; `None` when Windows
/// won't say.
pub fn link_count(file: &std::fs::File) -> Option<u32> {
    // SAFETY: a valid handle; `info` is sized for the call.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        (GetFileInformationByHandle(raw(file), &mut info) != 0).then_some(info.nNumberOfLinks)
    }
}

/// Stable volume/file identity of an open directory, including across restarts.
pub fn file_identity(file: &std::fs::File) -> Option<(u64, u64)> {
    // SAFETY: the owned file keeps a valid handle and info is sized for the call.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        (GetFileInformationByHandle(raw(file), &mut info) != 0).then_some((
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ))
    }
}

/// Where an open file really is (after links), in plain `C:\…` form when
/// that means the same file.
pub fn real_path(file: &std::fs::File) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: `buf` has room for `buf.len()` characters.
        let n =
            unsafe { GetFinalPathNameByHandleW(raw(file), buf.as_mut_ptr(), buf.len() as u32, 0) }
                as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            let s = String::from_utf16_lossy(&buf[..n]);
            return Some(PathBuf::from(super::plain_windows_path(&s).unwrap_or(s)));
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        buf.resize(n + 1, 0);
    }
}

/// The same folder on one of this session's mapped network drives, for a
/// `\\server\share\…` path (cmd.exe can't start in a UNC folder). Only a
/// drive whose path leads back to exactly `unc` counts. The drive mappings
/// are read locally; the candidate folder's real path is checked again.
pub fn mapped_drive_path(unc: &std::path::Path) -> Option<PathBuf> {
    const DRIVE_REMOTE: u32 = 4;
    let text = unc.to_str()?;
    if !text.starts_with(r"\\") {
        return None;
    }
    // SAFETY: no arguments.
    let drives = unsafe { GetLogicalDrives() };
    for i in 0..26u8 {
        if drives & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i);
        // SAFETY: a NUL-terminated root path.
        if unsafe { GetDriveTypeW(wide(format!("{letter}:\\")).as_ptr()) } != DRIVE_REMOTE {
            continue;
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` has room for `len` characters.
        if unsafe {
            WNetGetConnectionW(
                wide(format!("{letter}:")).as_ptr(),
                buf.as_mut_ptr(),
                &mut len,
            )
        } != 0
        {
            continue;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let remote = String::from_utf16_lossy(&buf[..end]);
        let Some(rest) = super::unc_rest(text, &remote) else {
            continue;
        };
        let candidate = PathBuf::from(format!("{letter}:\\{rest}"));
        if super::canonicalize(&candidate).ok().as_deref() == Some(unc) {
            return Some(candidate);
        }
    }
    None
}

/// Let child processes inherit `handle` (for a pipe end handed to a
/// browser), or stop that.
pub fn set_inheritable(handle: &impl AsRawHandle, on: bool) -> std::io::Result<()> {
    // SAFETY: valid handle; only the inherit flag changes.
    if unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT, u32::from(on)) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Keep this process's own standard handles from the programs it starts.
/// Windows gives a child every inheritable handle of its parent (Rust starts
/// children with inheritance on, whatever their stdio is), and a pipe a
/// caller reads this process's output through is inheritable here. So a
/// long-lived program started from such a command (the desktop app runs
/// `tessera service install` that way; it starts the launcher, which starts
/// the server) would hold the caller's pipe open, and the caller would wait
/// for its end for as long as the server runs. Children meant to share our
/// output still get it: Rust duplicates the handle for `Stdio::inherit`.
pub fn keep_std_handles_private() {
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain calls; a missing handle is skipped and only the
        // inherit flag of a valid one changes.
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Point this process's standard error at `file` (Rust's `eprintln!` and
/// logging look it up on every write); returns the previous handle for
/// [`restore_stderr`].
pub fn redirect_stderr(file: &std::fs::File) -> Option<usize> {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    // SAFETY: plain calls; the file stays open while it is stderr (the
    // caller keeps it).
    unsafe {
        let old = GetStdHandle(STD_ERROR_HANDLE);
        (SetStdHandle(STD_ERROR_HANDLE, raw(file)) != 0).then_some(old as usize)
    }
}

/// Give standard error back (see [`redirect_stderr`]).
pub fn restore_stderr(old: usize) {
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
    // SAFETY: the handle stderr had before.
    unsafe {
        SetStdHandle(STD_ERROR_HANDLE, old as HANDLE);
    }
}

// ---- staying awake ---------------------------------------------------------------

/// Keep Windows from going to sleep by itself while this process runs and
/// the computer is plugged in, so devices can reach a server left on. On
/// battery Windows sleeps as it usually does; the screen turns off as
/// usual either way, and closing a laptop's lid or choosing Sleep still
/// works. Looked at again every minute. `powercfg /requests` lists it.
pub fn keep_awake_while_plugged_in() {
    let spawned = std::thread::Builder::new()
        .name("tessera-awake".into())
        .spawn(|| {
            let mut awake = None;
            loop {
                let want = super::stays_awake(ac_line_status());
                if let Some(ok) = super::update_awake(&mut awake, want, || {
                    let flags = if want {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                    } else {
                        ES_CONTINUOUS
                    };
                    // SAFETY: plain call. It holds for this thread, which
                    // lives as long as the process.
                    unsafe { SetThreadExecutionState(flags) != 0 }
                }) {
                    if want && ok {
                        tracing::info!(
                            "keeping this computer awake while Tessera runs (it is plugged in)"
                        );
                    } else if want {
                        tracing::warn!("couldn't keep this computer awake; it may go to sleep");
                    } else if ok {
                        tracing::info!(
                            "on battery: Windows may let this computer sleep as it usually does"
                        );
                    } else {
                        tracing::warn!("couldn't release the keep-awake request; retrying soon");
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("can't keep this computer awake: {e}");
    }
}

/// Windows' power line status: 0 on battery, 1 plugged in, 255 unknown.
fn ac_line_status() -> u8 {
    // SAFETY: plain-old-data struct, zero is a valid value.
    let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: `s` outlives the call.
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
        return 255;
    }
    s.ACLineStatus
}

// ---- the user's Stop, kept in the registry too -------------------------------

/// Where a machine's Stop count is also kept (per Tessera folder: `name`),
/// beside its files: a second, independent place, so a Stop survives a
/// restart when its files couldn't be written.
const STOP_KEY: &str = r"Software\Tessera\Stops";

/// The Stop count kept for `name` (`None`: none kept yet).
pub fn stop_record(name: &str) -> std::io::Result<Option<u32>> {
    let (key, value) = (wide(STOP_KEY), wide(name));
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `data` holds `size` bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    match r {
        ERROR_SUCCESS => Ok(Some(data)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        e => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
}

/// Keep `count` as the Stop count for `name`.
pub fn set_stop_record(name: &str, count: u32) -> std::io::Result<()> {
    let (key, value) = (wide(STOP_KEY), wide(name));
    // SAFETY: four bytes of a REG_DWORD.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            REG_DWORD,
            (&count as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(r as i32));
    }
    Ok(())
}

// ---- starting at sign-in -----------------------------------------------------

/// The per-user list of programs Windows starts at sign-in (Task Manager →
/// Startup apps shows it, and lets the user turn an entry off). Writing it
/// needs no administrator rights.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The command line of the sign-in entry `name`, if there is one.
pub fn run_entry(name: &str) -> Option<String> {
    user_value(RUN_KEY, name)
}

/// Where the installer notes Tessera's startup entries before an update
/// takes them out (see `hooks.nsh`): each one's whole command line under
/// its name, the install folder (`Folder`) and the installer's process
/// (`Installer`). Kept until they are back.
pub const UPDATE_KEY: &str = r"Software\Tessera\Update";

/// The text value `name` of `key` (under HKEY_CURRENT_USER).
pub fn user_value(key: &str, name: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(name));
    let mut size = 0u32;
    // SAFETY: size query (null buffer).
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if r != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `bytes` bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Add or replace the sign-in entry `name`.
pub fn set_run_entry(name: &str, command_line: &str) -> std::io::Result<()> {
    set_user_value(RUN_KEY, name, command_line)
}

/// Write the text value `name` of `key` (under HKEY_CURRENT_USER).
pub fn set_user_value(key: &str, name: &str, text: &str) -> std::io::Result<()> {
    let (key, value, data) = (wide(key), wide(name), wide(text));
    // SAFETY: `data` is a NUL-terminated UTF-16 string of the given size.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(r as i32));
    }
    Ok(())
}

/// Remove the sign-in entry `name` (fine if there is none).
pub fn remove_run_entry(name: &str) -> std::io::Result<()> {
    remove_user_value(RUN_KEY, name)
}

/// Remove the value `name` of `key` (fine if there is none).
pub fn remove_user_value(key: &str, name: &str) -> std::io::Result<()> {
    let (key, value) = (wide(key), wide(name));
    // SAFETY: plain call with NUL-terminated strings.
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
    if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
        return Err(std::io::Error::from_raw_os_error(r as i32));
    }
    Ok(())
}

/// Start `program` on its own, outside this console and its job, so it
/// keeps running after this terminal closes. Returns its pid.
pub fn spawn_detached(
    program: &std::path::Path,
    args: &[String],
    envs: &[(String, String)],
) -> std::io::Result<u32> {
    use std::os::windows::process::CommandExt;
    // It outlives this command: it must not hold whatever reads our output.
    keep_std_handles_private();
    let start = |flags: u32| {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .envs(envs.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags);
        cmd.spawn().map(|c| c.id())
    };
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // A job that doesn't allow leaving it refuses the first form.
    start(base | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| start(base))
}
