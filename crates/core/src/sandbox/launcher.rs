//! Native entry into a shared mount/PID namespace. Only identity values cross
//! exec: the host directory stays open in the server, never in this process.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path};

pub(super) const FLAG: &str = "--tessera-enter-directory";

/// Called before runtime/config initialization by every Tessera executable
/// that can host an engine. No shell or interpreter runs on this path.
pub fn entry() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(FLAG)) {
        return None;
    }
    // Inherit non-dumpability at fork: there is no same-uid ptrace window
    // before the requested child's first pre-exec syscall.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Some(126);
    }
    let args: Vec<_> = args.collect();
    let result = (|| {
        if args.len() < 5 || args[3] != "--" {
            return Err(io::Error::other("invalid internal directory arguments"));
        }
        let env = super::environment::receive()?;
        let private = super::metadata::receive()?;
        // Test control is in the sealed payload, not the trusted environment.
        #[cfg(debug_assertions)]
        if env.iter().any(|(k, _)| k == "TESSERA_TEST_DIRECTORY_STOP") {
            unsafe { libc::raise(libc::SIGSTOP) };
        }
        let number = |s: &OsString| {
            s.to_str()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| io::Error::other("invalid directory identity"))
        };
        if args[1] == "-" && args[2] == "-" {
            // Unbound launches retain entry-by-name/fallback-to-root semantics,
            // still entirely inside the mounts and without an interpreter.
            std::env::set_current_dir(&args[0]).or_else(|_| std::env::set_current_dir("/"))?;
        } else {
            let directory = checked(Path::new(&args[0]), (number(&args[1])?, number(&args[2])?))?;
            if unsafe { libc::fchdir(directory.as_raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // Opened inside these mounts, closed before the requested program.
            drop(directory);
        }
        // nsenter --no-fork left this trusted process outside the group's
        // PID namespace. Only the requested command is born inside it, after
        // entry/check/close; no peer can stop or inspect the trusted launcher.
        let mut cmd = std::process::Command::new(&args[4]);
        cmd.args(&args[5..]).env_clear().envs(env);
        super::supervision::run(cmd, private)
    })();
    match result {
        Err(e) if e.raw_os_error() == Some(libc::ESTALE) => {
            eprintln!(
                "Tessera: the command's working folder is not the folder that was checked: it was replaced or moved, or the command sandbox hides it. Nothing was run."
            );
            Some(super::FOLDER_CHANGED_STATUS)
        }
        Err(e) => {
            eprintln!(
                "Tessera: could not enter the command's working folder or start its program: {e}. Nothing was run."
            );
            Some(126)
        }
        Ok(status) => {
            if let Some(signal) = status.signal() {
                // Preserve signal termination for the supervising server.
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                    libc::raise(signal);
                }
                Some(128 + signal)
            } else {
                Some(status.code().unwrap_or(126))
            }
        }
    }
}

/// Walk from this namespace's root, refusing links in every component, then
/// fstat the object actually entered. Renames after this cannot redirect cwd.
pub(super) fn checked(path: &Path, expected: (u64, u64)) -> io::Result<File> {
    let stale = || io::Error::from_raw_os_error(libc::ESTALE);
    if !path.is_absolute() {
        return Err(stale());
    }
    let mut directory = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                use std::os::fd::FromRawFd;
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| stale())?;
                let fd = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(stale());
                }
                directory = unsafe { File::from_raw_fd(fd) };
            }
            _ => return Err(stale()),
        }
    }
    let meta = directory.metadata()?;
    if (meta.dev(), meta.ino()) != expected {
        return Err(stale());
    }
    Ok(directory)
}
