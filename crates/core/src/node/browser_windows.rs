//! Windows web views use a small, private relay process. Only that relay
//! creates inheritable CDP handles: concurrent agent commands in the node
//! cannot inherit a browser's control pipe. The relay and browser tree are
//! held in a mandatory, non-breakaway, kill-on-close job before either runs.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::Arc;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

pub(super) struct Job(OwnedHandle);

impl Job {
    pub(super) fn end(&self) {
        // SAFETY: our live job handle, never a reused process id.
        unsafe { TerminateJobObject(self.0.as_raw_handle() as HANDLE, 1) };
    }

    /// A relay exit alone does not confirm its nested Chrome tree exited.
    /// Query failures keep the profile reserved, never assume empty.
    pub(super) async fn wait_empty(&self) {
        loop {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: our owned Job and the exact accounting POD size.
            let empty = unsafe {
                QueryInformationJobObject(
                    self.0.as_raw_handle() as HANDLE,
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                ) != 0
                    && info.ActiveProcesses == 0
            };
            if empty {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Whole-job peak committed memory, not current PSS/working set.
    /// The parent warns at80% of the enforced ceiling before shedding.
    pub(super) fn memory(&self) -> Option<u64> {
        // SAFETY: this class fills this exact POD struct.
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            (QueryInformationJobObject(
                self.0.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            ) != 0)
                .then_some(info.PeakJobMemoryUsed as u64)
        }
    }
}

pub(super) fn spawn(
    bin: &Path,
    base: &Path,
    scale: Option<f64>,
    env: &[(String, String)],
    cap: u64,
) -> Result<(tokio::process::Child, Arc<Job>), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--tessera-webview-worker")
        .arg(bin)
        .arg(base)
        .arg(scale.map(|s| s.to_string()).unwrap_or_default())
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    crate::platform::windows::keep_std_handles_private();
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let hold = || -> std::io::Result<Job> {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("no browser process"))?;
        // SAFETY: handles are checked, owned immediately and closed on every error.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let job = Job(OwnedHandle::from_raw_handle(handle.cast()));
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = cap as usize;
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let process = OwnedHandle::from_raw_handle(process.cast());
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
            ) == 0
                || AssignProcessToJobObject(handle, process.as_raw_handle() as HANDLE) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if !crate::platform::windows::resume(pid) {
                return Err(std::io::Error::other("the browser couldn't be let run"));
            }
            Ok(job)
        }
    };
    match hold() {
        Ok(job) => Ok((child, Arc::new(job))),
        Err(e) => {
            let _ = child.start_kill();
            Err(format!(
                "the browser wasn't run: its Windows job couldn't be secured: {e}"
            ))
        }
    }
}

/// Internal CLI entry, before any agent, data store or provider starts.
/// This process has no other children to inherit its short-lived CDP handles.
pub async fn worker(bin: &Path, base: &Path, scale: Option<f64>) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(super::browser::args(&base.join("profile"), scale));
    let (mut child, (reader, mut writer)) = crate::browser::process::spawn_piped(&mut cmd, None)?;
    let job = Arc::new(
        child
            .id()
            .and_then(crate::platform::windows::take_job)
            .ok_or("the browser has no owned process group")?,
    );
    let _log = crate::browser::process::keep_log(&mut child);
    let input =
        tokio::spawn(async move { tokio::io::copy(&mut tokio::io::stdin(), &mut writer).await });
    let output = tokio::spawn(super::browser_relay::copy(reader, tokio::io::stdout()));
    let pipes = async move {
        tokio::select! { _ = input => {}, _ = output => {} }
    };
    // The worker's parent owns a non-breakaway Job and the profile lease.
    // If this worker returns before Chrome confirms exit, the parent waits
    // for that whole Job to empty; it cannot admit a new profile user.
    let tree = job.clone();
    crate::browser::process::relay_shutdown(
        async move {
            let _ = crate::browser::process::confirmed_exit(child).await;
        },
        pipes,
        move || tree.end(),
        job,
    )
    .await;
    Ok(())
}
