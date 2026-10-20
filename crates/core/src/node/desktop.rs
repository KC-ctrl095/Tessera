//! Desktop control: the graphical session a machine's commands get while it
//! is on (off by default, switched on only at that computer; see
//! docs/MACHINES.md).
//!
//! The session is read when each command starts, since a machine usually
//! runs as a background service that started before (or outlives) the
//! session it would talk to:
//!
//! 1. this process's own environment, when it has a display (a machine run
//!    from a terminal inside the session, or a service whose session
//!    imported its variables): used as it is, never mixed with another;
//! 2. otherwise what the session handed to the user's service manager
//!    (`systemctl --user show-environment`: Hyprland, Plasma and GNOME do
//!    this at login, many setups with `dbus-update-activation-environment`);
//! 3. otherwise a running compositor or desktop shell of this user.
//!
//! Only known session variables are taken, each checked: sockets must exist
//! and belong to this user. Commands then see those sockets (the Wayland or
//! X11 display, Hyprland's or sway's control socket) inside the otherwise
//! hidden runtime folder, and the session bus only through a filter
//! (`xdg-dbus-proxy`) that lets them talk to the desktop (notifications,
//! settings, the window manager, user services, media players) but not to
//! the keyring or wallet that hold the user's passwords. Without the filter
//! installed, commands get no session bus.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What desktop control hands a command.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Session variables, checked.
    pub vars: Vec<(String, String)>,
    /// Sockets and folders commands need inside hidden places (the runtime
    /// folder, `/tmp/.X11-unix`).
    pub keep: Vec<PathBuf>,
    /// The session bus address, for the filter.
    pub bus: Option<String>,
    /// e.g. "Hyprland on Wayland".
    pub label: String,
}

/// Variables that describe the session (taken as they are, if plain).
const DESCRIBE: &[&str] = &[
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
    "KDE_FULL_SESSION",
    "KDE_SESSION_VERSION",
    "QT_QPA_PLATFORM",
    "GDK_BACKEND",
];

/// Bus names commands may talk to through the filter (`NAME.*` covers the
/// names below it). The keyring and wallets (`org.freedesktop.secrets`,
/// `org.gnome.keyring`, `org.kde.kwalletd*`), online accounts and
/// everything else stay out of reach.
pub const BUS_TALK: &[&str] = &[
    "org.freedesktop.Notifications",
    "org.freedesktop.portal.Desktop",
    "org.freedesktop.portal.Documents",
    "org.freedesktop.systemd1",
    "org.freedesktop.ScreenSaver",
    "org.freedesktop.PowerManagement",
    "org.freedesktop.FileManager1",
    "org.freedesktop.IBus",
    "ca.desrt.dconf",
    "org.mpris.MediaPlayer2.*",
    "org.kde.KWin",
    "org.kde.plasmashell",
    "org.kde.kglobalaccel",
    "org.kde.KScreen",
    "org.kde.StatusNotifierWatcher",
    "org.kde.kded5",
    "org.kde.kded6",
    "org.kde.ActivityManager",
    "org.kde.Solid.PowerManagement",
    "org.gnome.Shell",
    "org.gnome.Mutter.DisplayConfig",
    "org.gnome.SettingsDaemon.*",
    "org.fcitx.Fcitx5",
];

/// Compositors and desktop shells, to find the session by (step 3).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SESSION_PROGRAMS: &[&str] = &[
    "Hyprland",
    "kwin_wayland",
    "kwin_x11",
    "gnome-shell",
    "sway",
    "niri",
    "river",
    "labwc",
    "wayfire",
    "plasmashell",
    "xfce4-session",
    "i3",
    "cinnamon",
    "mate-session",
];

/// The graphical session commands would get now, if one runs.
pub fn discover() -> Option<Session> {
    let own: HashMap<String, String> = std::env::vars().collect();
    if has_display(&own) {
        return from_env(&own);
    }
    if let Some(env) = service_manager_env().filter(has_display)
        && let Some(s) = from_env(&env)
    {
        return Some(s);
    }
    process_env().and_then(|e| from_env(&e))
}

fn has_display(env: &HashMap<String, String>) -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|k| env.get(*k).is_some_and(|v| !v.trim().is_empty()))
}

/// A plain one-line value (no control characters), not too long.
fn plain(v: &str, max: usize) -> bool {
    !v.is_empty() && v.len() <= max && !v.chars().any(char::is_control)
}

#[cfg(unix)]
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

/// `p` exists, is of the wanted kind and belongs to this user.
#[cfg(unix)]
fn ours(p: &Path, want: Kind) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return false;
    };
    let t = m.file_type();
    let kind = match want {
        Kind::Socket => t.is_socket(),
        Kind::Dir => t.is_dir(),
        Kind::File => t.is_file(),
    };
    kind && m.uid() == uid()
}

#[cfg(not(unix))]
fn ours(_p: &Path, _want: Kind) -> bool {
    false
}

#[derive(Clone, Copy)]
enum Kind {
    Socket,
    Dir,
    File,
}

/// The session described by `env`, checked; `None` without a usable
/// display.
pub fn from_env(env: &HashMap<String, String>) -> Option<Session> {
    let get = |k: &str| env.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    let runtime = get("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && ours(p, Kind::Dir));
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut keep: Vec<PathBuf> = Vec::new();
    let mut kinds: Vec<&str> = Vec::new();

    if let Some(w) = get("WAYLAND_DISPLAY").filter(|w| plain(w, 200)) {
        let sock = if w.starts_with('/') {
            Some(PathBuf::from(w))
        } else if !w.contains('/') {
            runtime.as_ref().map(|r| r.join(w))
        } else {
            None
        };
        if let Some(sock) = sock.filter(|s| ours(s, Kind::Socket)) {
            vars.push(("WAYLAND_DISPLAY".into(), w.into()));
            keep.push(sock);
            kinds.push("Wayland");
        }
    }
    if let Some(d) = get("DISPLAY").filter(|d| plain(d, 40)) {
        // Local displays only: ":N" or ":N.S".
        let n = d
            .strip_prefix(':')
            .map(|r| r.split('.').next().unwrap_or(""))
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if let Some(n) = n {
            let sock = PathBuf::from(format!("/tmp/.X11-unix/X{n}"));
            // The X server owns its socket (often root): it just has to be one.
            if std::fs::symlink_metadata(&sock).is_ok() {
                vars.push(("DISPLAY".into(), d.into()));
                keep.push(sock);
                if !kinds.contains(&"Wayland") {
                    kinds.push("X11");
                }
                if let Some(x) = get("XAUTHORITY")
                    .filter(|x| plain(x, 400) && x.starts_with('/'))
                    .map(PathBuf::from)
                    .filter(|x| ours(x, Kind::File))
                {
                    vars.push(("XAUTHORITY".into(), x.display().to_string()));
                    keep.push(x);
                }
            }
        }
    }
    if vars.is_empty() {
        return None;
    }
    if let Some(r) = &runtime {
        vars.push(("XDG_RUNTIME_DIR".into(), r.display().to_string()));
        let dconf = r.join("dconf");
        if ours(&dconf, Kind::Dir) {
            keep.push(dconf);
        }
    }
    let mut desktop = None;
    if let Some(sig) = get("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| {
        plain(s, 200)
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !s.starts_with('.')
    }) {
        let dirs = runtime
            .iter()
            .map(|r| r.join("hypr").join(sig))
            .chain([PathBuf::from("/tmp/hypr").join(sig)]);
        if let Some(dir) = dirs.into_iter().find(|d| ours(d, Kind::Dir)) {
            vars.push(("HYPRLAND_INSTANCE_SIGNATURE".into(), sig.into()));
            keep.push(dir);
            desktop = Some("Hyprland");
        }
    }
    for (k, name) in [
        ("SWAYSOCK", "sway"),
        ("I3SOCK", "i3"),
        ("NIRI_SOCKET", "niri"),
    ] {
        if let Some(p) = get(k)
            .filter(|p| plain(p, 400) && p.starts_with('/'))
            .map(PathBuf::from)
            .filter(|p| ours(p, Kind::Socket))
        {
            vars.push((k.into(), p.display().to_string()));
            keep.push(p);
            desktop.get_or_insert(name);
        }
    }
    for k in DESCRIBE {
        if let Some(v) = get(k).filter(|v| plain(v, 100)) {
            vars.push(((*k).into(), v.into()));
        }
    }
    let bus = get("DBUS_SESSION_BUS_ADDRESS")
        .filter(|b| plain(b, 512) && b.starts_with("unix:"))
        .map(str::to_string);
    let name = desktop
        .map(str::to_string)
        .or_else(|| {
            get("XDG_CURRENT_DESKTOP")
                .filter(|v| plain(v, 60))
                .map(|v| v.split(':').next().unwrap_or(v).to_string())
        })
        .unwrap_or_else(|| "Your desktop".into());
    Some(Session {
        vars,
        keep,
        bus,
        label: format!("{name} on {}", kinds.join(" and ")),
    })
}

/// What the session handed to the user's service manager.
fn service_manager_env() -> Option<HashMap<String, String>> {
    let out = run_quick(
        std::process::Command::new("systemctl")
            .args(["--user", "show-environment"])
            .env_clear()
            .envs(
                std::env::vars_os()
                    .filter(|(k, _)| k == "XDG_RUNTIME_DIR" || k == "PATH" || k == "HOME"),
            ),
    )?;
    Some(parse_env_lines(&out))
}

/// `KEY=VALUE` lines (`show-environment` quotes odd values `$'…'`; those
/// are left out).
fn parse_env_lines(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, v)| {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !v.starts_with("$'")
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Run briefly; its output if it succeeded.
fn run_quick(cmd: &mut std::process::Command) -> Option<String> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() > Duration::from_secs(3) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child
        .stdout
        .take()?
        .take(256 * 1024)
        .read_to_string(&mut out)
        .ok()?;
    Some(out)
}

/// The session as this user's programs see it: a compositor sets its
/// variables after it starts, so its own environment rarely has them; the
/// programs it started (a panel, a terminal) do. Children of a compositor
/// or desktop shell come first, then those programs themselves, the newest
/// of each.
#[cfg(target_os = "linux")]
fn process_env() -> Option<HashMap<String, String>> {
    use std::os::unix::fs::MetadataExt;
    let me = uid();
    let stat = |pid: u32| -> Option<(u32, u64)> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = s.rsplit_once(')')?.1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        Some((f.get(1)?.parse().ok()?, f.get(19)?.parse().ok()?))
    };
    let comm = |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let mut best: Option<((u8, u64), HashMap<String, String>)> = None;
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == std::process::id() || !e.metadata().is_ok_and(|m| m.uid() == me) {
            continue;
        }
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        let env: HashMap<String, String> = raw
            .split(|b| *b == 0)
            .filter_map(|kv| std::str::from_utf8(kv).ok())
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if !has_display(&env) {
            continue;
        }
        let Some((ppid, started)) = stat(pid) else {
            continue;
        };
        let rank = if SESSION_PROGRAMS.contains(&comm(ppid).trim()) {
            2
        } else if SESSION_PROGRAMS.contains(&comm(pid).trim()) {
            1
        } else {
            0
        };
        if best.as_ref().is_none_or(|(k, _)| (rank, started) > *k) {
            best = Some(((rank, started), env));
        }
    }
    best.map(|(_, e)| e)
}

#[cfg(not(target_os = "linux"))]
fn process_env() -> Option<HashMap<String, String>> {
    None
}

/// The session bus filter (`xdg-dbus-proxy`), when installed.
pub fn bus_filter_program() -> Option<PathBuf> {
    [
        "/usr/bin/xdg-dbus-proxy",
        "/usr/libexec/xdg-dbus-proxy",
        "/usr/lib/xdg-dbus-proxy",
        "/usr/local/bin/xdg-dbus-proxy",
        "/bin/xdg-dbus-proxy",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// A filtered view of the session bus for one command, at a socket of its
/// own; it stops when dropped.
pub struct BusFilter {
    child: std::process::Child,
    socket: PathBuf,
    /// Our end of the pipe the filter watches: closing it stops the filter.
    #[cfg(unix)]
    _sync: std::os::fd::OwnedFd,
}

impl BusFilter {
    /// Start filtering `bus` at `socket` (which must not exist yet).
    #[cfg(unix)]
    pub fn start(bus: &str, socket: &Path) -> Result<Self, String> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        let program = bus_filter_program()
            .ok_or("the session bus filter (xdg-dbus-proxy) isn't installed")?;
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let mut cmd = std::process::Command::new(program);
        cmd.arg("--fd=3")
            .arg(bus)
            .arg(socket)
            .arg("--filter")
            .args(BUS_TALK.iter().map(|n| format!("--talk={n}")))
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let w = write.as_raw_fd();
        unsafe {
            cmd.pre_exec(move || {
                // Its end of the pipe as fd 3 (dup2 clears close-on-exec).
                if libc::dup2(w, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("couldn't start xdg-dbus-proxy: {e}"))?;
        drop(write);
        // It writes one byte once it listens.
        let mut pfd = libc::pollfd {
            fd: read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, 3000) } == 1 && {
            let mut b = [0u8; 1];
            unsafe { libc::read(read.as_raw_fd(), b.as_mut_ptr().cast(), 1) == 1 }
        };
        if !ready || !socket.exists() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the session bus filter didn't start".into());
        }
        Ok(Self {
            child,
            socket: socket.to_path_buf(),
            _sync: read,
        })
    }

    #[cfg(not(unix))]
    pub fn start(_bus: &str, _socket: &Path) -> Result<Self, String> {
        Err("the session bus filter needs Linux".into())
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for BusFilter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_session_is_read_from_its_variables_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().canonicalize().unwrap();
        let _w = UnixListener::bind(rt.join("wayland-7")).unwrap();
        std::fs::create_dir_all(rt.join("hypr/abc_123")).unwrap();
        let rts = rt.display().to_string();
        let s = from_env(&env(&[
            ("XDG_RUNTIME_DIR", &rts),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc_123"),
            ("XDG_CURRENT_DESKTOP", "Hyprland"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1/bus"),
            ("GITHUB_TOKEN", "ghp_x"),
            ("SSH_AUTH_SOCK", "/run/user/1/ssh"),
        ]))
        .unwrap();
        let names: Vec<&str> = s.vars.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            names.contains(&"WAYLAND_DISPLAY")
                && names.contains(&"HYPRLAND_INSTANCE_SIGNATURE")
                && names.contains(&"XDG_RUNTIME_DIR"),
            "{names:?}"
        );
        assert!(
            !names.contains(&"GITHUB_TOKEN")
                && !names.contains(&"SSH_AUTH_SOCK")
                && !names.contains(&"DBUS_SESSION_BUS_ADDRESS"),
            "{names:?}"
        );
        assert_eq!(s.keep, vec![rt.join("wayland-7"), rt.join("hypr/abc_123")]);
        assert_eq!(s.bus.as_deref(), Some("unix:path=/run/user/1/bus"));
        assert_eq!(s.label, "Hyprland on Wayland");

        // A display that isn't there, or isn't a socket, is no session.
        std::fs::write(rt.join("wayland-8"), "").unwrap();
        for w in ["wayland-9", "wayland-8", "../wayland-7"] {
            assert!(
                from_env(&env(&[("XDG_RUNTIME_DIR", &rts), ("WAYLAND_DISPLAY", w)])).is_none(),
                "{w}"
            );
        }
        // A signature that tries to climb out is left out.
        let s = from_env(&env(&[
            ("XDG_RUNTIME_DIR", &rts),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "../../etc"),
        ]))
        .unwrap();
        assert!(
            !s.vars
                .iter()
                .any(|(k, _)| k == "HYPRLAND_INSTANCE_SIGNATURE")
        );
        assert!(from_env(&env(&[("XDG_RUNTIME_DIR", &rts)])).is_none());
    }

    #[test]
    fn service_manager_lines_are_parsed_plainly() {
        let e =
            parse_env_lines("WAYLAND_DISPLAY=wayland-1\nODD=$'a\\nb'\nPATH=/usr/bin\nnot a line\n");
        assert_eq!(
            e.get("WAYLAND_DISPLAY").map(String::as_str),
            Some("wayland-1")
        );
        assert!(!e.contains_key("ODD") && e.contains_key("PATH"));
    }
}
