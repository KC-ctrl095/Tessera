//! One desktop process owns an app profile. Acquire before loading settings
//! and retain the handle until the app exits. The server and launcher locks
//! own different files; none of them protects servers.json. No fallback can
//! safely keep writing this profile when locking is unavailable.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(crate) struct Owner {
    _file: File,
}

impl Owner {
    pub(crate) fn acquire(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            opts.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            opts.share_mode(0);
        }
        acquire_with(|| opts.open(dir.join("profile.lock")), lock)
    }
}

fn acquire_with(
    open: impl FnOnce() -> io::Result<File>,
    lock: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<Owner> {
    let held = (|| {
        let file = open()?;
        lock(&file)?;
        Ok(Owner { _file: file })
    })();
    held.map_err(|e: io::Error| {
        let busy = e.kind() == io::ErrorKind::WouldBlock
            || (cfg!(windows) && e.raw_os_error() == Some(32));
        if busy {
            io::Error::new(io::ErrorKind::WouldBlock, "Tessera is already open with this app profile. Close that window before opening it again.")
        } else {
            io::Error::new(e.kind(), format!("Couldn't securely own this app profile; refusing to load or write its settings: {e}"))
        }
    })
}

fn lock(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        flock_with(|| {
            // SAFETY: the descriptor stays valid while the owner holds File.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }
    #[cfg(windows)]
    {
        // Exclusive sharing was enforced when opening the file.
        let _ = file;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "app profile ownership is not supported here",
        ))
    }
}

#[cfg(unix)]
fn flock_with(mut call: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    loop {
        match call() {
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    #[test]
    fn second_profile_owner_cannot_load_or_write_until_first_closes() {
        let dir = tempfile::tempdir().unwrap();
        let first = Owner::acquire(dir.path()).unwrap();
        let file = dir.path().join("servers.json");
        super::super::private_file::write_verified(&file, b"saved login").unwrap();
        assert_eq!(
            Owner::acquire(dir.path()).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"saved login");
        // A distinct profile is independent; no global app/process lock.
        let other = tempfile::tempdir().unwrap();
        let _other = Owner::acquire(other.path()).unwrap();
        drop(first);
        let _next = Owner::acquire(dir.path()).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn separate_process_profile_owner_is_refused_then_allowed_after_close() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Owner::acquire(dir.path()).unwrap();
        let child = |expect_busy: bool| {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "profile_owner::tests::profile_owner_child",
                    "--nocapture",
                ])
                .env("TESSERA_PROFILE_OWNER_TEST_DIR", dir.path())
                .env(
                    "TESSERA_PROFILE_OWNER_TEST_BUSY",
                    if expect_busy { "yes" } else { "no" },
                )
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "profile ownership child failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                String::from_utf8_lossy(&result.stdout).contains("1 passed"),
                "child must run its exact fixture"
            );
        };
        child(true);
        drop(owner);
        child(false);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn profile_owner_child() {
        let Some(dir) = std::env::var_os("TESSERA_PROFILE_OWNER_TEST_DIR") else {
            return;
        };
        let result = Owner::acquire(Path::new(&dir));
        if std::env::var("TESSERA_PROFILE_OWNER_TEST_BUSY").unwrap() == "yes" {
            assert_eq!(result.err().unwrap().kind(), io::ErrorKind::WouldBlock);
        } else {
            let _owner = result.unwrap();
        }
    }

    #[test]
    fn profile_open_and_lock_errors_never_fall_back_to_an_unowned_profile() {
        let dir = tempfile::tempdir().unwrap();
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Unsupported,
            io::ErrorKind::Other,
        ] {
            let opened = acquire_with(
                || Err(io::Error::new(kind, "open failed")),
                |_| panic!("no file"),
            );
            assert!(
                opened
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("refusing to load or write")
            );
            let locked = acquire_with(
                || File::create(dir.path().join("lock")),
                |_| Err(io::Error::new(kind, "lock failed")),
            );
            assert!(
                locked
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("refusing to load or write")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn profile_flock_retries_interrupts_but_never_uses_an_unsupported_fallback() {
        let mut calls = 0;
        flock_with(|| {
            calls += 1;
            if calls == 1 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, 2);
        for errno in [
            libc::ENOLCK,
            libc::EOPNOTSUPP,
            libc::ENOSYS,
            libc::EBADF,
            libc::EIO,
            libc::EACCES,
        ] {
            let mut calls = 0;
            let e = flock_with(|| {
                calls += 1;
                Err(io::Error::from_raw_os_error(errno))
            })
            .unwrap_err();
            assert_eq!(e.raw_os_error(), Some(errno));
            assert_eq!(calls, 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn profile_claim_does_not_follow_a_planted_lock_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unrelated");
        std::fs::write(&target, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("profile.lock")).unwrap();
        assert!(Owner::acquire(dir.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    }
}
