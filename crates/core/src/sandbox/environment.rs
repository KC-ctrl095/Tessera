//! Private requested-program environment. The trusted loaders see only fixed
//! PATH/locale and an fd number, never pass_env or the payload in argv.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

pub(super) const FD_VAR: &str = "TESSERA_INTERNAL_ENV_FD";
const LIMIT: usize = 2 * 1024 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

/// Must be last, after the caller adds its requested program environment.
/// All production callers already use an explicit, cleared agent environment.
/// Unspecified names use the ordinary *program* defaults, never the server's.
pub(super) fn protect(cmd: &mut std::process::Command) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut env: Vec<_> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(OsStr::to_owned)))
        .collect();
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push(("PATH".into(), Some("/usr/bin:/bin".into())));
    }
    #[cfg(debug_assertions)]
    let stop_ready = env.iter().find_map(|(k, v)| {
        (k == "TESSERA_TEST_WATCHDOG_STOP_READY")
            .then_some(v.as_ref()?)
            .and_then(|s| s.to_str()?.parse::<usize>().ok())
    });
    #[cfg(debug_assertions)]
    let fail_after = env.iter().find_map(|(k, v)| {
        (k == "TESSERA_TEST_ANCHOR_FAIL_AFTER")
            .then_some(v.as_ref()?)
            .and_then(|s| s.to_str()?.parse::<usize>().ok())
    });
    #[cfg(debug_assertions)]
    let probe_fd = env.iter().find_map(|(k, v)| {
        (k == "TESSERA_TEST_ANCHOR_IDENTITIES_FD")
            .then_some(v.as_ref()?)
            .and_then(|s| s.to_str()?.parse::<i32>().ok())
    });
    #[cfg(not(debug_assertions))]
    let (stop_ready, fail_after, probe_fd) = (None, None, None);
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    for (key, value) in env {
        let Some(value) = value else { continue };
        if key.is_empty()
            || key.as_bytes().contains(&0)
            || key.as_bytes().contains(&b'=')
            || value.as_bytes().contains(&0)
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    if bytes.len() > LIMIT {
        return Err(io::Error::from_raw_os_error(libc::E2BIG));
    }
    let fd = unsafe {
        libc::memfd_create(
            c"tessera-program-env".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(&bytes)?;
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Avoid caller-specific fd3 (sudo's pipe), stdio and low-fd IPC conventions.
    let high = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 64) };
    if high < 0 {
        return Err(io::Error::last_os_error());
    }
    let held = unsafe { File::from_raw_fd(high) };
    cmd.env(FD_VAR, high.to_string());
    // The test pause is control metadata, not supplied to either loader by
    // pass_env. Pass it through the sealed payload, like every program value.
    super::metadata::protect(cmd, stop_ready, fail_after, probe_fd)?;
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(held.as_raw_fd(), libc::F_GETFD);
            if flags < 0
                || libc::fcntl(held.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

pub(super) fn receive() -> io::Result<Vec<(OsString, OsString)>> {
    receive_from(std::env::var_os(FD_VAR).as_deref())
}

fn receive_from(value: Option<&OsStr>) -> io::Result<Vec<(OsString, OsString)>> {
    let fd = value
        .and_then(OsStr::to_str)
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|n| *n >= 3)
        .ok_or_else(|| io::Error::other("missing private program environment"))?;
    // Validate before assuming ownership; malformed/closed fds must not
    // become an invalid OwnedFd or close stdin/stdout/stderr.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    // Never accept a pipe/device/path. A finite, sealed regular memfd only.
    if !meta.is_file()
        || unsafe { libc::fcntl(fd, libc::F_GET_SEALS) } != SEALS
        || meta.len() > LIMIT as u64
    {
        return Err(io::Error::other("invalid private program environment"));
    }
    use std::os::unix::fs::FileExt;
    let mut bytes = zeroize::Zeroizing::new(vec![0; meta.len() as usize]);
    file.read_exact_at(&mut bytes, 0)?;
    drop(file);
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> io::Result<Vec<(OsString, OsString)>> {
    if bytes.len() > LIMIT || (!bytes.is_empty() && bytes.last() != Some(&0)) {
        return Err(io::Error::other("invalid program environment size"));
    }
    let mut result = Vec::new();
    let mut names = std::collections::HashSet::new();
    for value in bytes.split(|b| *b == 0).filter(|v| !v.is_empty()) {
        let at = value
            .iter()
            .position(|b| *b == b'=')
            .filter(|n| *n > 0)
            .ok_or_else(|| io::Error::other("invalid program environment entry"))?;
        let name = OsString::from_vec(value[..at].to_vec());
        if !names.insert(name.clone()) {
            return Err(io::Error::other("duplicate program environment entry"));
        }
        result.push((name, OsString::from_vec(value[at + 1..].to_vec())));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(bytes: &[u8], seals: i32) -> File {
        let fd = unsafe {
            libc::memfd_create(
                c"environment-test".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        assert!(fd >= 3);
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes).unwrap();
        if seals != 0 {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) }, 0);
        }
        file
    }

    fn identity(fd: i32) -> Option<(u64, u64)> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        (unsafe { libc::fstat(fd, &mut stat) } == 0).then_some((stat.st_dev, stat.st_ino))
    }

    fn read(file: File) -> io::Result<Vec<(OsString, OsString)>> {
        let expected = identity(file.as_raw_fd());
        // Keep the original alive, so even another test reusing this fd number
        // cannot reuse this inode and fake a failure of receiver closure.
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 512) };
        assert!(fd >= 512);
        let result = receive_from(Some(OsStr::new(&fd.to_string())));
        assert_ne!(
            identity(fd),
            expected,
            "receiver closes its own copy (parallel tests may reuse the number)"
        );
        result
    }

    #[test]
    fn malformed_missing_closed_and_low_descriptors_are_refused_without_closing_stdio() {
        for value in [
            None,
            Some(""),
            Some("abc"),
            Some("-1"),
            Some("0"),
            Some("1"),
            Some("2"),
            Some("2147483647"),
        ] {
            assert!(receive_from(value.map(OsStr::new)).is_err(), "{value:?}");
        }
        assert!(unsafe { libc::fcntl(1, libc::F_GETFD) } >= 0);
        let file = payload(b"KEY=value\0", SEALS);
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 768) };
        assert!(fd >= 768);
        unsafe {
            libc::close(fd);
        }
        assert!(receive_from(Some(OsStr::new(&fd.to_string()))).is_err());
    }

    #[test]
    fn non_memfd_and_every_missing_seal_are_refused() {
        assert!(read(File::open("/dev/null").unwrap()).is_err());
        assert!(read(File::open("/proc/self/exe").unwrap()).is_err());
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let r = unsafe { File::from_raw_fd(pipe[0]) };
        let _w = unsafe { File::from_raw_fd(pipe[1]) };
        assert!(read(r).is_err());
        for missing in [
            0,
            libc::F_SEAL_WRITE,
            libc::F_SEAL_GROW,
            libc::F_SEAL_SHRINK,
            libc::F_SEAL_SEAL,
        ] {
            let seals = if missing == 0 { 0 } else { SEALS & !missing };
            assert!(
                read(payload(b"KEY=value\0", seals)).is_err(),
                "missing seal {missing}"
            );
        }
    }

    #[test]
    fn payload_bounds_termination_names_and_duplicates_are_checked() {
        assert!(read(payload(b"", SEALS)).unwrap().is_empty());
        assert!(read(payload(b"KEY=value", SEALS)).is_err());
        assert!(read(payload(b"=value\0", SEALS)).is_err());
        assert!(read(payload(b"KEY\0", SEALS)).is_err());
        assert!(read(payload(b"KEY=first\0KEY=second\0", SEALS)).is_err());
        let mut maximum = b"K=".to_vec();
        maximum.resize(LIMIT - 1, b'x');
        maximum.push(0);
        assert_eq!(maximum.len(), LIMIT);
        assert_eq!(read(payload(&maximum, SEALS)).unwrap().len(), 1);
        maximum.push(0);
        assert!(read(payload(&maximum, SEALS)).is_err());
        let mut cmd = std::process::Command::new("/bin/true");
        cmd.env_clear().env("BIG", "x".repeat(LIMIT));
        assert_eq!(
            protect(&mut cmd).unwrap_err().raw_os_error(),
            Some(libc::E2BIG)
        );
    }

    #[test]
    fn duplicates_share_sealed_contents_not_offsets_and_receiver_closes_only_its_copy() {
        let file = payload(b"KEY=value\0OTHER=two=parts\0", SEALS);
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
        assert!(fd >= 64);
        use std::io::{Seek, SeekFrom};
        let mut other = file.try_clone().unwrap();
        other.seek(SeekFrom::End(0)).unwrap();
        let values = read(unsafe { File::from_raw_fd(fd) }).unwrap();
        assert_eq!(
            values,
            vec![
                ("KEY".into(), "value".into()),
                ("OTHER".into(), "two=parts".into())
            ]
        );
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) },
            SEALS
        );
        assert_eq!(
            unsafe { libc::write(file.as_raw_fd(), c"x".as_ptr().cast(), 1) },
            -1
        );
    }

    #[test]
    fn sender_uses_only_fixed_trusted_environment_and_close_on_exec_parent_fd() {
        let mut cmd = std::process::Command::new("/usr/bin/nsenter");
        cmd.env_clear()
            .env("LD_PRELOAD", "program-only")
            .env("GLIBC_TUNABLES", "program-only")
            .env("PRIVATE_TOKEN", "value-not-argv");
        protect(&mut cmd).unwrap();
        let env: Vec<_> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect();
        assert_eq!(env.len(), 5);
        assert!(env.iter().all(|(k, _)| {
            [
                "PATH",
                "LANG",
                "LC_ALL",
                FD_VAR,
                super::super::metadata::VAR,
            ]
            .contains(&k.to_str().unwrap())
        }));
        assert!(!cmd.get_args().any(|v| v == "value-not-argv"));
        let fd: i32 = env
            .iter()
            .find(|(k, _)| *k == FD_VAR)
            .unwrap()
            .1
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GET_SEALS) }, SEALS);
        let expected = identity(fd);
        drop(cmd);
        assert_ne!(identity(fd), expected);
    }
}
