use std::time::{SystemTime, UNIX_EPOCH};

/// Open a regular file inside `root`, retaining the checked descriptor through
/// use. Resolving a link may select a file inside the root, never outside it.
pub fn open_file_in(
    path: &std::path::Path,
    root: &std::path::Path,
) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    let opened = crate::tools::file_object::Opened::open_in(path, root)?;
    Ok((opened.path, opened.file))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sortable, URL-safe unique id: 10 chars of time + 12 random, Crockford base32.
pub fn new_id() -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut out = String::with_capacity(22);
    let mut t = now_ms();
    let mut time_part = [0u8; 10];
    for slot in time_part.iter_mut().rev() {
        *slot = ALPHABET[(t & 31) as usize];
        t >>= 5;
    }
    out.push_str(std::str::from_utf8(&time_part).unwrap());
    for _ in 0..12 {
        out.push(ALPHABET[rand::random_range(0..32)] as char);
    }
    out
}

pub fn random_token(bytes: usize) -> String {
    use base64::Engine;
    let buf: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Rough token estimate. Calibrated per session against provider-reported
/// usage (see `context::Calibration`); only needs to be monotone and cheap.
pub fn estimate_tokens(text: &str) -> u64 {
    // ~4 chars/token for prose, denser for code/JSON; bytes/3.6 is a
    // conservative middle that rarely underestimates.
    (text.len() as f64 / 3.6).ceil() as u64
}

/// Largest char boundary <= idx.
pub fn floor_char(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary >= idx.
pub fn ceil_char(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Truncate to at most `max` bytes on a char boundary, appending an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", &s[..floor_char(s, max)])
}

/// Keep the head and tail of `s` so the result is at most ~`max` bytes.
/// Returns (text, was_truncated).
pub fn head_tail(s: &str, max: usize, head_share: f32) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let head = floor_char(s, (max as f32 * head_share) as usize);
    let tail_start = ceil_char(s, s.len() - (max - head).min(s.len()));
    let omitted = &s[head..tail_start];
    let lines = omitted.lines().count();
    (
        format!(
            "{}\n\n[… {} bytes / {} lines omitted …]\n\n{}",
            &s[..head],
            omitted.len(),
            lines,
            &s[tail_start..]
        ),
        true,
    )
}

/// First line, trimmed and shortened; used for previews and titles.
pub fn one_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    truncate(line, max)
}

/// Control characters spelled out (`\r`, `\u{1b}`) instead of acted on, for
/// text a person must see exactly as it is: what an approval would run.
/// Invisible characters that reorder or hide text (right-to-left overrides,
/// zero-width spaces) are spelled out too.
pub fn visible_controls(s: &str) -> String {
    spell_out(s, |_| false)
}

/// The same, keeping line breaks and tabs: a whole command, line by line.
pub fn visible_controls_lines(s: &str) -> String {
    spell_out(s, |c| c == '\n' || c == '\t')
}

fn spell_out(s: &str, keep: impl Fn(char) -> bool) -> String {
    s.chars()
        .map(|c| {
            if (c.is_control() || crate::scan::always_hidden(c)) && !keep(c) {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub fn expand_home(path: &str) -> std::path::PathBuf {
    // Windows: Git Bash's `/c/Users/…` (what the agent's commands there
    // write) is `C:\Users\…`.
    if cfg!(windows)
        && let Some(p) = crate::platform::git_bash_path(path)
    {
        return std::path::PathBuf::from(p);
    }
    let rest = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\").filter(|_| cfg!(windows)));
    if let Some(rest) = rest {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    std::path::PathBuf::from(path)
}

/// Addresses other devices on the LAN can use for a server bound to `bind`
/// (none when it only listens on loopback).
pub fn lan_urls(bind: &str) -> Vec<String> {
    let Ok(addr) = bind.parse::<std::net::SocketAddr>() else {
        return Vec::new();
    };
    let ip = if addr.ip().is_unspecified() {
        lan_ip()
    } else {
        Some(addr.ip()).filter(|ip| !ip.is_loopback())
    };
    ip.map(|ip| match ip {
        std::net::IpAddr::V6(v6) => format!("http://[{v6}]:{}", addr.port()),
        v4 => format!("http://{v4}:{}", addr.port()),
    })
    .into_iter()
    .collect()
}

/// Primary LAN address (no packets are sent).
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr()
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_loopback())
}

/// `1.2 MB`-style size.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Lift this process's open-files limit to the most it may have. Agents,
/// their commands, browsers and every connected device each hold several
/// descriptors, and the usual soft limit (1024) is easy to reach on a busy
/// server; new connections would then be refused. Best effort.
pub fn raise_open_files_limit() {
    #[cfg(unix)]
    // SAFETY: getrlimit/setrlimit on a valid struct for this process.
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < lim.rlim_max {
            lim.rlim_cur = lim.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

/// Paths walked one name at a time from an open folder, never through a
/// symbolic link: for files that someone else (an agent's command) can
/// change while Tessera works on them outside that command's sandbox, where
/// a folder swapped for a link would otherwise lead it anywhere.
#[cfg(unix)]
pub mod beneath {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io::{Error, ErrorKind, Result};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path};

    fn cname(name: &OsStr) -> Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "NUL in name"))
    }

    /// Open `name` in `dir` with `flags` (plus `O_NOFOLLOW | O_CLOEXEC`).
    pub fn open_at(dir: &File, name: &OsStr, flags: i32, mode: u32) -> Result<File> {
        let c = cname(name)?;
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// An open folder (the path itself is trusted).
    pub fn open_dir(path: &Path) -> Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)
    }

    /// The folder `rel` (plain names only) beneath `root`, made (mode 0755)
    /// where missing when `create` is set.
    pub fn dir(root: &File, rel: &Path, create: bool) -> Result<File> {
        let mut cur = root.try_clone()?;
        for c in rel.components() {
            let name = match c {
                Component::Normal(n) => n,
                Component::CurDir => continue,
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "not a plain relative path",
                    ));
                }
            };
            if create {
                let c = cname(name)?;
                if unsafe { libc::mkdirat(cur.as_raw_fd(), c.as_ptr(), 0o755) } != 0 {
                    let e = Error::last_os_error();
                    if e.kind() != ErrorKind::AlreadyExists {
                        return Err(e);
                    }
                }
            }
            cur = open_at(&cur, name, libc::O_DIRECTORY | libc::O_RDONLY, 0)?;
        }
        Ok(cur)
    }

    /// `name` in `dir` without following it: its metadata.
    pub fn lstat_at(dir: &File, name: &OsStr) -> Result<libc::stat> {
        let c = cname(name)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                c.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(Error::last_os_error());
        }
        Ok(st)
    }

    /// Where the link `name` in `dir` points.
    pub fn read_link_at(dir: &File, name: &OsStr) -> Result<std::ffi::OsString> {
        let c = cname(name)?;
        let mut buf = vec![0u8; 4096];
        let n = unsafe {
            libc::readlinkat(
                dir.as_raw_fd(),
                c.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(Error::last_os_error());
        }
        buf.truncate(n as usize);
        Ok(std::ffi::OsString::from_vec(buf))
    }

    /// Make the link `name` in `dir`, pointing to `target`.
    pub fn symlink_at(target: &OsStr, dir: &File, name: &OsStr) -> Result<()> {
        let (t, c) = (cname(target)?, cname(name)?);
        if unsafe { libc::symlinkat(t.as_ptr(), dir.as_raw_fd(), c.as_ptr()) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_by_time_and_are_unique() {
        let a = new_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = new_id();
        assert!(a < b);
        assert_eq!(a.len(), 22);
    }

    #[test]
    fn head_tail_keeps_ends() {
        let s: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let (t, cut) = head_tail(&s, 200, 0.3);
        assert!(cut);
        assert!(t.starts_with("line 0"));
        assert!(t.trim_end().ends_with("line 999"));
        assert!(t.contains("omitted"));
    }

    #[test]
    fn controls_are_shown_not_obeyed() {
        assert_eq!(
            visible_controls("rm -rf ~/x\r\x1b[2K ls"),
            "rm -rf ~/x\\r\\u{1b}[2K ls"
        );
        // Text that reorders or hides itself is spelled out too.
        assert_eq!(
            visible_controls("ls \u{202e}txt.exe\u{200b}"),
            "ls \\u{202e}txt.exe\\u{200b}"
        );
        // A whole command keeps its lines.
        assert_eq!(
            visible_controls_lines("cd x\n\tmake\r\n"),
            "cd x\n\tmake\\r\n"
        );
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("héllo", 2), "h…");
    }
}
