//! Which computer a Tessera program runs on, so the server can show the
//! desktop app, the terminal and the machine of one computer as one entry
//! (Settings → Devices).
//!
//! The key is a hash of the system's own id (`/etc/machine-id`, Windows'
//! `MachineGuid`, the Mac's platform UUID) and the computer's name, under a
//! label of Tessera's own: the system id never leaves the computer, and the
//! key means nothing to anyone else (as systemd asks of programs that need a
//! stable id). The name is part of it so two computers cloned from one image
//! (same system id, different names) never count as one.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// This computer's key, the same for every Tessera program on it; `None`
/// where the system has no id to read (phones, most containers).
pub fn computer_key() -> Option<String> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(|| {
        let id = system_id()?;
        let host = host_name().unwrap_or_default().to_lowercase();
        Some(key_of(&id, &host))
    })
    .clone()
}

fn key_of(id: &str, host: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"tessera computer 1\0");
    h.update(id.trim().to_lowercase().as_bytes());
    h.update(b"\0");
    h.update(host.as_bytes());
    h.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// This computer's own name (its host name), as the system has it.
pub fn host_name() -> Option<String> {
    #[cfg(unix)]
    let name = {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its whole length; the name ends at
        // the first zero byte (or the buffer's end).
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..end]).trim().to_string()
        } else {
            String::new()
        }
    };
    #[cfg(not(unix))]
    let name = std::env::var("COMPUTERNAME").unwrap_or_default();
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    (!name.is_empty() && !name.eq_ignore_ascii_case("localhost")).then_some(name)
}

/// The system's own id for this computer, unchanged across restarts.
fn system_id() -> Option<String> {
    let id = read_system_id()?;
    let id = id.trim();
    // Reject an empty or placeholder id (all zeros, a template's "uninitialized").
    let useful = id.len() >= 16
        && id.chars().any(|c| c.is_ascii_hexdigit() && c != '0')
        && !id.eq_ignore_ascii_case("uninitialized");
    useful.then(|| id.to_string())
}

#[cfg(target_os = "linux")]
fn read_system_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
}

#[cfg(target_os = "macos")]
fn read_system_id() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("\"IOPlatformUUID\""))?;
    line.split('"').nth(3).map(str::to_string)
}

#[cfg(windows)]
fn read_system_id() -> Option<String> {
    use std::os::windows::process::CommandExt;
    /// No console window flashing up from the desktop app.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
            "/reg:64",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("MachineGuid"))?;
    line.split_whitespace().last().map(str::to_string)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn read_system_id() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_tells_computers_apart() {
        let a = key_of("0123456789abcdef0123456789abcdef", "desktop");
        assert_eq!(a, key_of("0123456789ABCDEF0123456789ABCDEF\n", "desktop"));
        assert_eq!(a.len(), 32);
        // A clone of the same image under another name is another computer.
        assert_ne!(a, key_of("0123456789abcdef0123456789abcdef", "desktop-2"));
        assert_ne!(a, key_of("1123456789abcdef0123456789abcdef", "desktop"));
        // The system id itself is nowhere in it.
        assert!(!a.contains("0123456789abcdef"));
    }

    #[test]
    fn this_computer_has_a_name_and_one_key() {
        if let Some(n) = host_name() {
            assert!(!n.is_empty() && n.len() <= 64);
        }
        assert_eq!(computer_key(), computer_key());
    }
}
