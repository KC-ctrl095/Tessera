//! Nonblocking server data lock: interruption retries, only known missing
//! filesystem support degrades to server.json, and other errors stop startup.

use std::fs::File;
use std::io;

#[derive(Debug, PartialEq, Eq)]
pub enum Locked {
    Held,
    Busy,
    #[cfg_attr(windows, allow(dead_code))] // Only Unix has a known unsupported-filesystem path.
    Unsupported,
}

pub fn lock_now(file: &File) -> io::Result<Locked> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        flock_with(|| {
            // SAFETY: a valid descriptor for as long as `file` lives.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }
    #[cfg(windows)]
    {
        Ok(if tessera_core::platform::windows::try_lock(file)? {
            Locked::Held
        } else {
            Locked::Busy
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(Locked::Unsupported)
    }
}

#[cfg(unix)]
fn flock_with(mut call: impl FnMut() -> io::Result<()>) -> io::Result<Locked> {
    loop {
        match call() {
            Ok(()) => return Ok(Locked::Held),
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
            Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => return Ok(Locked::Busy),
            Err(e)
                if [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOSYS]
                    .contains(&e.raw_os_error().unwrap_or(0)) =>
            {
                return Ok(Locked::Unsupported);
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn interrupted_lock_retries_until_held_or_busy() {
        for expected in [Locked::Held, Locked::Busy] {
            let mut calls = 0;
            let got = flock_with(|| {
                calls += 1;
                match calls {
                    1 | 2 => Err(io::Error::from_raw_os_error(libc::EINTR)),
                    _ if expected == Locked::Held => Ok(()),
                    _ => Err(io::Error::from_raw_os_error(libc::EWOULDBLOCK)),
                }
            })
            .unwrap();
            assert_eq!(got, expected);
            assert_eq!(calls, 3);
        }
    }

    #[test]
    fn only_known_missing_lock_support_uses_the_explicit_fallback() {
        for errno in [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOSYS] {
            assert_eq!(
                flock_with(|| Err(io::Error::from_raw_os_error(errno))).unwrap(),
                Locked::Unsupported
            );
        }
    }

    #[test]
    fn unexpected_lock_errors_fail_closed_without_retry() {
        for errno in [
            libc::EBADF,
            libc::EIO,
            libc::EACCES,
            libc::EPERM,
            libc::EINVAL,
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
        assert!(flock_with(|| Err(io::Error::other("lock failed"))).is_err());
    }

    #[test]
    fn a_real_lock_is_held_until_the_file_closes() {
        let dir = tempfile::tempdir().unwrap();
        let open = || {
            File::options()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.path().join("server.lock"))
                .unwrap()
        };
        let first = open();
        let second = open();
        assert_eq!(lock_now(&first).unwrap(), Locked::Held);
        assert_eq!(lock_now(&second).unwrap(), Locked::Busy);
        drop(first);
        // A process another test forks meanwhile shares the open file, and
        // with it the lock, until it starts its program: wait for that copy
        // to go too (it failed once in a full workspace run).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match lock_now(&second).unwrap() {
                Locked::Held => break,
                Locked::Busy if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => panic!("still {other:?} after the first holder closed"),
            }
        }
    }
}
