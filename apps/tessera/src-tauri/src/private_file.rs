//! Private app settings. Local login rotation also needs a durable replace
//! and exact read-back before an earlier saved login can be retired.

use std::io::{self, Write};
use std::path::Path;

pub fn write(file: &Path, data: &[u8]) -> io::Result<()> {
    replace(file, data, false)
}

pub fn write_verified(file: &Path, data: &[u8]) -> io::Result<()> {
    replace(file, data, true)?;
    verify(file, data)
}

fn verify(file: &Path, data: &[u8]) -> io::Result<()> {
    if std::fs::read(file)? != data {
        return Err(io::Error::other(
            "saved app settings did not match; earlier logins were not retired",
        ));
    }
    Ok(())
}

fn replace(file: &Path, data: &[u8], durable: bool) -> io::Result<()> {
    // Only directories created by this write belong in its durability set.
    // Android's searchable /data/user/0 ancestor is not readable by the app.
    let mut created = Vec::new();
    if let Some(dir) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        let mut missing = Vec::new();
        let mut next = dir;
        while !next.as_os_str().is_empty() && !next.try_exists()? {
            missing.push(next.to_path_buf());
            next = next.parent().unwrap_or(Path::new("."));
        }
        #[cfg(windows)]
        tessera_core::platform::private_file::create_dir_all(dir)?;
        for path in missing.into_iter().rev() {
            match std::fs::create_dir(&path) {
                Ok(()) => created.push(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(e),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let result = (|| {
        #[cfg(windows)]
        let mut f = tessera_core::platform::private_file::create_new(&tmp)?;
        #[cfg(not(windows))]
        let mut f = {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(&tmp)?
        };
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        #[cfg(windows)]
        if durable {
            durable_replace(&tmp, file)?;
        } else {
            std::fs::rename(&tmp, file)?;
        }
        #[cfg(not(windows))]
        std::fs::rename(&tmp, file)?;
        #[cfg(unix)]
        if durable {
            // Persist the new name inside the app-owned containing directory.
            // Never open untouched ancestors, even if they are searchable.
            let parent = file
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::File::open(parent)?.sync_all()?;
            for dir in created.iter().rev().filter(|dir| dir.as_path() != parent) {
                std::fs::File::open(dir)?.sync_all()?;
            }
        }
        #[cfg(not(any(unix, windows)))]
        if durable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "durable app settings are not supported here",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(windows)]
fn durable_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings path contains a NUL",
            ));
        }
        value.push(0);
        Ok(value)
    };
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: both paths are NUL-terminated and live until the call ends.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Android-like ancestor permissions without touching any real app directory.
#[cfg(all(unix, test))]
pub(crate) struct ProtectedAppDir {
    _temp: tempfile::TempDir,
    ancestor: std::path::PathBuf,
    pub app: std::path::PathBuf,
}

#[cfg(all(unix, test))]
impl ProtectedAppDir {
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let ancestor = temp.path().join("searchable-only");
        let app = ancestor.join("owned-app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o111)).unwrap();
        assert!(
            std::fs::File::open(&ancestor).is_err(),
            "fixture requires an unprivileged test runner"
        );
        Self {
            _temp: temp,
            ancestor,
            app,
        }
    }
}

#[cfg(all(unix, test))]
impl Drop for ProtectedAppDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.ancestor, std::fs::Permissions::from_mode(0o700));
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn durable_save_never_opens_searchable_unreadable_ancestors() {
        let dir = ProtectedAppDir::new();
        let file = dir.app.join("new/nested/settings.json");
        write_verified(&file, b"previews off").unwrap();
        write_verified(&file, b"accepted receipt").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"accepted receipt");
    }
    #[cfg(windows)]
    #[test]
    fn durable_replace_rejects_embedded_nuls_before_ffi() {
        use std::os::windows::ffi::OsStringExt;
        let bad = std::path::PathBuf::from(std::ffi::OsString::from_wide(&[97, 0, 98]));
        let good = Path::new("unused-settings-test-path");
        for (from, to) in [(bad.as_path(), good), (good, bad.as_path())] {
            assert_eq!(
                durable_replace(from, to).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn durable_save_replaces_and_verifies_the_exact_private_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new/app/servers.json");
        write_verified(&file, b"old login").unwrap();
        write_verified(&file, b"new login").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new login");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(file.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn failed_replace_preserves_the_previous_file_and_removes_temporary_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("old"), b"old login").unwrap();
        assert!(write_verified(&file, b"new login").is_err());
        assert_eq!(std::fs::read(file.join("old")).unwrap(), b"old login");
        assert!(
            !file
                .with_extension(format!("tmp{}", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn missing_or_mismatched_read_back_is_not_a_verified_save() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        assert!(verify(&file, b"new login").is_err());
        write(&file, b"old login").unwrap();
        let e = verify(&file, b"new login").unwrap_err().to_string();
        assert!(!e.contains("old login") && !e.contains("new login"));
    }
}
