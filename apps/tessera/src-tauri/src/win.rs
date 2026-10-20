//! Windows pieces of the desktop app: starting `tessera.exe` without a
//! console window, asking a Tessera server or machine to stop cleanly (the
//! named event it waits on; see `tessera_core::platform::windows`), and
//! tying this computer's machine to the app's lifetime (a Job Object that
//! ends it when the app ends, even if the app crashes).

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, OpenEventW, OpenProcess, PROCESS_SET_QUOTA,
    PROCESS_TERMINATE, SetEvent,
};

pub use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

fn owned(h: HANDLE) -> Option<OwnedHandle> {
    // SAFETY: a fresh, non-null handle nobody else owns.
    (!h.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

/// A console program (`tessera.exe`) started from this windowless app would
/// otherwise open a console window.
pub fn quiet(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd.creation_flags(CREATE_NO_WINDOW)
}

/// Start the server on this computer through `tessera-service.exe` (next
/// to `tessera.exe`), without adding it to the programs Windows starts at
/// sign-in: it runs without a window, keeps running when the app closes,
/// and is started again if it crashes or its watchdog ends it because it
/// was stuck. It doesn't keep the computer awake (`TESSERA_KEEP_AWAKE=0`):
/// that comes with "Start Tessera when I sign in". `None` when there is no
/// `tessera-service.exe` (start it some other way); else whether it started.
pub fn start_server_supervised(
    tessera: &std::path::Path,
    log: &std::path::Path,
) -> Option<Result<(), String>> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, DETACHED_PROCESS};
    let launcher = tessera.with_file_name("tessera-service.exe");
    if !launcher.is_file() {
        return None;
    }
    let start = |flags: u32| {
        std::process::Command::new(&launcher)
            .arg("--log")
            .arg(log)
            .arg("serve")
            .env("TESSERA_KEEP_AWAKE", "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    // Out of any job the app runs in, so it outlives the app; a job that
    // doesn't allow that refuses the first form.
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    Some(
        start(base | CREATE_BREAKAWAY_FROM_JOB)
            .or_else(|_| start(base))
            .map(|_| ())
            .map_err(|e| format!("couldn't start the server: {e}")),
    )
}

/// Ask Tessera process `pid` to stop cleanly. False when it isn't one (only
/// Tessera servers and machines have this event).
pub fn request_stop(pid: u32) -> bool {
    let name: Vec<u16> = format!("Local\\tessera-stop-{pid}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: plain call; the result is checked.
    let Some(event) = owned(unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) }) else {
        return false;
    };
    // SAFETY: valid event handle with modify access.
    unsafe { SetEvent(event.as_raw_handle() as HANDLE) != 0 }
}

/// The machine this app starts waits for this before doing anything
/// (`TESSERA_START_GATE` names it): set once [`end_with_app`] tied it to
/// the app.
pub struct StartGate {
    event: OwnedHandle,
    pub name: String,
}

impl StartGate {
    pub fn new() -> Result<Self, String> {
        // A name nobody guesses (the standard hasher's keys are random).
        use std::hash::{BuildHasher, Hasher};
        let nonce = std::hash::RandomState::new().build_hasher().finish();
        let name = format!(
            "Local\\tessera-node-start-{}-{nonce:016x}",
            std::process::id()
        );
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: plain call (manual reset, not set); the result is checked.
        let event = owned(unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) })
            .ok_or_else(|| {
                format!(
                    "couldn't make its start signal: {}",
                    std::io::Error::last_os_error()
                )
            })?;
        Ok(Self { event, name })
    }

    /// Let it start.
    pub fn open(&self) -> bool {
        // SAFETY: our own event handle.
        unsafe { SetEvent(self.event.as_raw_handle() as HANDLE) != 0 }
    }
}

/// End process `pid` (and what it started) when this app ends, however it
/// ends: a Job Object that kills its processes when its last handle closes,
/// and that handle is held until the app's process goes. False when it
/// couldn't be (the caller ends the process then: it must not outlive the
/// app).
#[must_use]
pub fn end_with_app(pid: u32) -> bool {
    // SAFETY: plain calls; every result is checked, handles are owned.
    unsafe {
        let Some(process) = owned(OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid))
        else {
            return false;
        };
        let Some(job) = owned(CreateJobObjectW(std::ptr::null(), std::ptr::null())) else {
            return false;
        };
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let job_h = job.as_raw_handle() as HANDLE;
        if SetInformationJobObject(
            job_h,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
            || AssignProcessToJobObject(job_h, process.as_raw_handle() as HANDLE) == 0
        {
            return false;
        }
        // Kept open on purpose until this process ends.
        std::mem::forget(job);
        true
    }
}
