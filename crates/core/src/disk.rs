//! Shared disk admission, extracted from T8 voice/space.rs for browser staging too.
use std::path::Path;

pub(crate) const RESERVE: u64 = 20 * 1024 * 1024 * 1024;

pub(crate) fn admit(free: u64, work: u64) -> Result<(), String> {
    if RESERVE.checked_add(work).is_none_or(|needed| free < needed) {
        Err("Not enough free disk while keeping 20 GB free. Free some space and retry.".into())
    } else {
        Ok(())
    }
}

pub(crate) fn available(path: &Path) -> Result<u64, String> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or("can't find the download disk")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(existing.as_os_str().as_bytes())
            .map_err(|_| "invalid download folder")?;
        let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and a live, correctly sized struct.
        if unsafe { libc::statvfs(path.as_ptr(), &mut info) } != 0 {
            return Err("can't check free disk for downloads".into());
        }
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
            return Err("can't check free disk for downloads".into());
        }
        Ok(free)
    }
    #[cfg(not(any(unix, windows)))]
    Err("can't check free disk on this server".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserve_is_fail_closed_and_checks_overflow() {
        assert!(admit(RESERVE - 1, 0).is_err());
        assert!(admit(RESERVE, 1).is_err());
        assert!(admit(u64::MAX, u64::MAX).is_err());
        assert!(admit(RESERVE + 1, 1).is_ok());
    }
}
