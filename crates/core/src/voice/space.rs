//! Runtime and download admission keep the server's disk reserve intact.
use std::path::Path;

pub(super) const RESERVE: u64 = 20 * 1024 * 1024 * 1024;

pub(super) fn admit(free: u64, work: u64) -> Result<(), String> {
    if RESERVE.checked_add(work).is_none_or(|needed| free < needed) {
        Err(
            "Not enough free disk for speech while keeping 20 GB free. Free some space and retry."
                .into(),
        )
    } else {
        Ok(())
    }
}

pub(super) fn available(path: &Path) -> Result<u64, String> {
    // Scratch/retention may not exist yet. Admit on its nearest existing
    // ancestor before creating anything on that same filesystem.
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or("can't find the speech data disk")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(existing.as_os_str().as_bytes())
            .map_err(|_| "invalid speech folder")?;
        let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and a live, correctly sized struct.
        if unsafe { libc::statvfs(path.as_ptr(), &mut info) } != 0 {
            return Err("can't check free disk for speech downloads".into());
        }
        // statvfs counters have different widths across supported Unix hosts.
        #[allow(clippy::unnecessary_cast)]
        let bytes = (info.f_bavail as u64).saturating_mul(info.f_frsize as u64);
        Ok(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0;
        // SAFETY: terminated wide path, output lives through this call.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err("can't check free disk for speech downloads".into());
        }
        Ok(free)
    }
    #[cfg(not(any(unix, windows)))]
    Err("can't check free disk on this server".into())
}
