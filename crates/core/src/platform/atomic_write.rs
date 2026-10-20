//! Windows config/vault and downloaded-binary publication. Stage in the
//! same directory, sync complete bytes, then replace the name in one call.
//! The algorithm is tested on Linux; only the final Windows syscall differs.

#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

pub(crate) fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    write_with(path, data, publish)
}

fn write_with(
    path: &Path,
    data: &[u8],
    publish: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = parent.join(format!(".tessera-file-{:032x}.tmp", rand::random::<u128>()));
    #[cfg(windows)]
    let mut file = super::private_file::create_new(&tmp)?;
    #[cfg(not(windows))]
    let mut file = {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&tmp)?
    };
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        publish(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(windows)]
fn publish(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |p: &Path| -> io::Result<Vec<u16>> {
        let mut p: Vec<u16> = p.as_os_str().encode_wide().collect();
        if p.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains a NUL",
            ));
        }
        p.push(0);
        Ok(p)
    };
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: both paths are NUL-terminated and remain alive for the call.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn publish(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_write_and_replacement_publish_complete_bytes_without_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("auth.json");
        write(&file, b"old").unwrap();
        write(&file, b"complete new content").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"complete new content");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_publication_keeps_the_old_file_and_removes_staged_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        write(&file, b"old").unwrap();
        let error = write_with(&file, b"new", |from, to| {
            assert_eq!(std::fs::read(from).unwrap(), b"new");
            assert_eq!(std::fs::read(to).unwrap(), b"old");
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test denial",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&file).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn competing_writers_never_share_a_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let ready = std::sync::Barrier::new(2);
        let staged = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            let mut tasks = Vec::new();
            for data in [b"one", b"two"] {
                let (file, ready, staged) = (&file, &ready, &staged);
                tasks.push(scope.spawn(move || {
                    write_with(file, data, |from, to| {
                        staged.lock().unwrap().push(from.to_path_buf());
                        ready.wait();
                        publish(from, to)
                    })
                }));
            }
            for task in tasks {
                task.join().unwrap().unwrap();
            }
        });
        let staged = staged.into_inner().unwrap();
        assert_ne!(staged[0], staged[1]);
        assert!(
            [b"one".as_slice(), b"two".as_slice()]
                .contains(&std::fs::read(&file).unwrap().as_slice())
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_new_file_publication_leaves_no_partial_download() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cloudflared.exe");
        assert!(
            write_with(&file, b"new", |_, _| Err(io::Error::other(
                "test disk failure"
            )))
            .is_err()
        );
        assert!(!file.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
