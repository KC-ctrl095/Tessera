//! "Administrator" on a machine (`MachineScope::admin`): agents may run a
//! command as this computer's administrator (`run` with `admin`). Such a
//! command runs outside the sandbox (the sandbox's user namespace is what
//! keeps `sudo` from gaining anything), as the user, with `sudo` working.
//!
//! * Only this computer turns it on (`tessera node admin on`, or the desktop
//!   app here), like every other loosening; it needs commands on, and is off
//!   by default. Turning it off, pausing or revoking the machine stops admin
//!   commands at once.
//! * By default each admin command asks the user first, showing the exact
//!   command, whatever the chat's mode (`AdminAccess::Ask`); the node itself
//!   refuses one that wasn't approved.
//! * `sudo`'s password, if it needs one, is saved here only
//!   (`node-admin-password` in Tessera's settings folder, readable by this
//!   account alone, hidden from sandboxed commands, refused to file
//!   actions), never sent anywhere. A command gets no copy of it: before the
//!   command runs, its shell hands the password to `sudo -S -v` on a pipe
//!   (file descriptor 3, closed again before the command), so `sudo` inside
//!   the command finds its credentials cached; `sudo -k` forgets them when
//!   the command ends. The password is also scrubbed from the output.
//! * Every admin command is in the machine's log, marked "as administrator".

use std::path::PathBuf;

use crate::config::Paths;

/// Where the password for `sudo` is kept on this computer.
pub fn password_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("node-admin-password")
}

/// The password saved for `sudo` here, if any.
pub fn password(paths: &Paths) -> Option<String> {
    let text = std::fs::read_to_string(password_file(paths)).ok()?;
    let p = text.strip_suffix('\n').unwrap_or(&text).to_string();
    (!p.is_empty()).then_some(p)
}

/// Save `password` for `sudo` (readable by this account only).
pub fn save_password(paths: &Paths, password: &str) -> anyhow::Result<()> {
    check_password_text(password).map_err(anyhow::Error::msg)?;
    crate::config::write_atomic(
        &password_file(paths),
        format!("{password}\n").as_bytes(),
        true,
    )
}

/// Forget the saved password.
pub fn forget_password(paths: &Paths) -> std::io::Result<()> {
    match std::fs::remove_file(password_file(paths)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A password `sudo -S` can read: one line, not empty.
pub fn check_password_text(p: &str) -> Result<(), String> {
    if p.is_empty() {
        return Err("the password is empty".into());
    }
    if p.contains('\n') || p.contains('\r') || p.contains('\0') {
        return Err("the password must be one line".into());
    }
    if p.len() > 1024 {
        return Err("that password is too long".into());
    }
    Ok(())
}

/// The shell script an admin command runs as: with a password, first hand
/// it from file descriptor 3 (then closed) to `sudo` — the system's own, by
/// its full path (`sudo`: never one found on the `PATH`, where a command
/// could have put a stand-in) — so the command's own `sudo` calls find
/// their credentials cached; forget them at the end. The command itself
/// follows unchanged, on its own lines.
pub fn script(command: &str, sudo: Option<&std::path::Path>) -> String {
    let mut s = String::new();
    let forget = match sudo {
        Some(path) => {
            let q = shell_quote(&path.to_string_lossy());
            s.push_str(&format!(
                "if ! {q} -S -p '' -v <&3 2>/dev/null; then exec 3<&-; \
                 echo '[Tessera: sudo did not accept the password saved on this computer for administrator commands. The user can save it again there: Settings → Devices → This computer → Administrator, or `tessera node admin password`.]' >&2; \
                 exit 126; fi\n"
            ));
            format!("{q} -k 2>/dev/null")
        }
        None => "sudo -k 2>/dev/null".to_string(),
    };
    s.push_str("exec 3<&-\n");
    s.push_str(&format!("trap '{}' EXIT\n", forget.replace('\'', "'\\''")));
    s.push_str(command);
    s.push('\n');
    s
}

/// `s` as one word for `sh`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The system's `sudo`: a file owned by root (as an installed `sudo` is),
/// never one a user could have put there.
#[cfg(unix)]
pub fn system_sudo() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    [
        "/usr/bin/sudo",
        "/bin/sudo",
        "/usr/local/bin/sudo",
        "/run/wrappers/bin/sudo",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.uid() == 0))
}

/// A pipe holding `password` and a newline, for the command's descriptor 3
/// (its write end already closed). The password is short: it fits the
/// pipe's buffer, so nothing waits.
#[cfg(unix)]
pub fn password_pipe(password: &str) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::io::Write as _;
    let (reader, mut writer) = std::io::pipe()?;
    writer.write_all(password.as_bytes())?;
    writer.write_all(b"\n")?;
    drop(writer);
    Ok(reader.into())
}

/// Check that `sudo` accepts `password` here (or, without one, that it
/// needs none), forgetting the credentials again. `Err` says what to do.
#[cfg(unix)]
pub fn sudo_accepts(password: Option<&str>) -> Result<(), String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let Some(sudo) = system_sudo() else {
        return Err("sudo isn't installed on this computer".into());
    };
    let _ = Command::new(&sudo)
        .arg("-k")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let mut cmd = Command::new(&sudo);
    match password {
        Some(_) => cmd.args(["-S", "-p", "", "-v"]),
        None => cmd.args(["-n", "-v"]),
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run sudo: {e}"))?;
    if let (Some(p), Some(mut stdin)) = (password, child.stdin.take()) {
        let _ = writeln!(stdin, "{p}");
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("couldn't run sudo: {e}"))?;
    let _ = Command::new(&sudo)
        .arg("-k")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(match password {
        None if why.contains("password is required") => {
            "sudo needs this computer's password here; save it too (it stays on this computer)"
                .into()
        }
        None => format!(
            "sudo doesn't work for this account: {}",
            crate::util::one_line(why.trim(), 200)
        ),
        Some(_) if why.contains("not in the sudoers") || why.contains("may not run sudo") => {
            "this account isn't allowed to use sudo on this computer".into()
        }
        Some(_) => "sudo didn't accept that password".into(),
    })
}

/// Administrator commands aren't available on Windows yet.
#[cfg(not(unix))]
pub fn sudo_accepts(_password: Option<&str>) -> Result<(), String> {
    Err("administrator commands aren't available on Windows yet".into())
}

/// A line for the agent after an admin command `sudo` refused for want of
/// a password (none saved), or after `sudo` failed in the sandbox.
pub fn sudo_hint(output: &str, admin: bool) -> Option<&'static str> {
    if admin
        && (output.contains("a password is required")
            || output.contains("a terminal is required to read the password"))
    {
        return Some(
            "[Tessera: sudo needs this computer's password, and none is saved there. The user can save it on that computer (Settings → Devices → This computer → Administrator, or `tessera node admin password`); it stays there and you never see it.]",
        );
    }
    let sandboxed_sudo = output.contains("effective uid is not 0")
        || output.contains("\"no new privileges\" flag is set")
        || output.contains("no new privileges");
    (!admin && sandboxed_sudo).then_some("[Tessera: sudo can't work in this machine's sandbox.]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_hands_the_password_to_the_system_sudo_and_closes_it_before_the_command() {
        let s = script(
            "sudo systemctl restart bluetooth",
            Some(std::path::Path::new("/usr/bin/sudo")),
        );
        let lines: Vec<&str> = s.lines().collect();
        assert!(
            lines[0].starts_with("if ! '/usr/bin/sudo' -S -p '' -v <&3"),
            "{s}"
        );
        assert_eq!(lines[1], "exec 3<&-");
        assert_eq!(
            lines[2],
            "trap ''\\''/usr/bin/sudo'\\'' -k 2>/dev/null' EXIT"
        );
        assert_eq!(lines[3], "sudo systemctl restart bluetooth");
        // Without a password nothing is read.
        let s = script("id -u", None);
        assert!(!s.contains(" -S "), "{s}");
        assert!(s.starts_with("exec 3<&-\n"), "{s}");
    }

    #[test]
    fn passwords_are_one_line() {
        assert!(check_password_text("testpw2").is_ok());
        assert!(check_password_text("").is_err());
        assert!(check_password_text("a\nb").is_err());
    }

    #[test]
    fn saved_passwords_are_private_and_can_be_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().join("tessera-home"));
        assert_eq!(password(&paths), None);
        save_password(&paths, "correct horse").unwrap();
        assert_eq!(password(&paths).as_deref(), Some("correct horse"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(password_file(&paths))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        forget_password(&paths).unwrap();
        forget_password(&paths).unwrap();
        assert_eq!(password(&paths), None);
    }

    #[test]
    fn sudo_failures_get_a_plain_hint() {
        assert!(
            sudo_hint("sudo: a password is required", true)
                .unwrap()
                .contains("save it on that computer")
        );
        assert!(
            sudo_hint(
                "sudo: effective uid is not 0, is /usr/bin/sudo on a file system with the 'nosuid' option set",
                false
            )
            .is_some()
        );
        assert_eq!(sudo_hint("all fine", false), None);
    }
}
