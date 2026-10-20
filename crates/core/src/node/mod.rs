//! A *machine*: this computer offering itself to the user's Tessera server
//! (`tessera node`), so agents in any chat, on any device, can read and
//! change files and run commands here, within what this computer allows.
//!
//! Security model (user guide: docs/MACHINES.md):
//!
//! * Turning a machine on and widening what it allows (its preset, folders,
//!   asking, who can use it, desktop control) happen only on this computer
//!   (`tessera node …`, or the desktop app running here). The server has no
//!   request that does either. The scope lives in this computer's own
//!   Tessera settings (`node.json`), which the sandbox hides from every
//!   command and the file actions refuse, so nothing an agent does here can
//!   change it.
//! * The node enforces its scope itself on every action and never trusts
//!   the server's view of it. Paths are resolved and checked here, and
//!   checked again on what was actually opened (a symlink swapped in
//!   between can't redirect a read or a write). Commands run in the kernel
//!   sandbox (`crate::sandbox`) with Tessera's data and other programs'
//!   credential stores hidden, and may change files only in the read-write
//!   folders (Landlock), never in startup files (unless desktop control is
//!   on) or the Tessera program; without that sandbox they are refused.
//! * Other devices can only reduce what it does: pause and resume it (never
//!   beyond its scope) or revoke it, after which it turns itself off until
//!   it is turned on here again.
//! * Every action, allowed or refused, goes to a local log.

pub mod admin;
pub mod backups;
pub mod browser;
mod browser_queue;
#[cfg(any(windows, test))]
mod browser_relay;
#[cfg(windows)]
pub mod browser_windows;
pub mod desktop;
pub mod exec;
pub mod log;
mod runner;
pub mod screen;
pub mod stopbar;
pub mod terminal;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tessera_proto::{
    AdminAccess, DesktopOp, FolderAccess, MachineAsk, MachinePreset, MachineScope, NodeAction,
    TerminalAccess,
};

use crate::config::Paths;
use crate::tools::Protected;

pub use runner::{Exit, Link, LinkEvent, Node};

/// Most folder rules a machine can have (each is a mount for commands).
pub const MAX_FOLDERS: usize = 32;
/// Most bots a machine can be limited to.
pub const MAX_BOTS: usize = 32;
/// Actions one machine runs at once.
pub const MAX_CALLS: usize = 8;
/// The shortest time limit per action.
pub const MIN_TIMEOUT_SECS: u64 = 10;

/// This computer's machine settings (`node.json` in the settings
/// directory). Only `tessera node` on this computer writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Turned off from another device; cleared when turned on here again.
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub scope: MachineScope,
    /// Grows each time the machine is turned on here, so a revoke from
    /// another device holds until then.
    #[serde(default)]
    pub generation: u64,
    /// The folders chosen for "Look only" and "Work in folders", kept while
    /// another preset is on so switching back finds them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            revoked: false,
            name: hostname(),
            scope: MachineScope::default(),
            generation: 0,
            chosen: Vec::new(),
        }
    }
}

pub fn config_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("node.json")
}

/// What the running node publishes for `tessera node status`.
pub fn state_file(paths: &Paths) -> PathBuf {
    paths.data_dir.join("node-state.json")
}

/// The user's Stop on this computer (`StopLatch`).
pub fn stop_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("stop.json")
}

/// The user's Stop on this computer, in its own file: written only by Stop
/// and Resume (the Stop bar, `tessera node stop`/`resume`, This computer in
/// the app), never by a change of settings, so no older copy of the
/// settings can undo it. `node.json`'s scope only mirrors it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StopLatch {
    #[serde(default)]
    pub stopped: bool,
    /// How many times it was stopped (only grows).
    #[serde(default)]
    pub stops: u32,
    /// When it last changed (ms since the epoch).
    #[serde(default)]
    pub at: u64,
}

/// A Stop kept beside `stop.json` without waiting for its lock (see
/// [`StopLatch::stop`]): only Stops are written here, as their count; a later
/// Resume (in `stop.json`, with a count at least as high) overrides it, so
/// nothing ever has to remove it.
fn pending_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("stop.pending")
}

#[derive(Serialize, Deserialize)]
struct PendingStop {
    stops: u32,
    #[serde(default)]
    at: u64,
}

/// Stops kept without the lock, each in its own `stop.pending-<id>` file
/// (see [`StopLatch::stop`]), with their names. An error when one can't be
/// read (that stops everything).
fn unacknowledged(paths: &Paths) -> std::io::Result<Vec<(PathBuf, PendingStop)>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&paths.config_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("stop.pending-") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())?;
        let p: PendingStop = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        out.push((entry.path(), p));
    }
    Ok(out)
}

/// Keep Stop `l` in a file of its own (a name no other Stop gets).
fn keep_own_stop(paths: &Paths, l: &StopLatch) -> bool {
    use std::hash::{BuildHasher, Hasher};
    let id = std::hash::RandomState::new().build_hasher().finish();
    let file = paths
        .config_dir
        .join(format!("stop.pending-{}-{id:016x}", std::process::id()));
    serde_json::to_string(&PendingStop {
        stops: l.stops,
        at: l.at,
    })
    .ok()
    .is_some_and(|text| crate::config::write_atomic(&file, text.as_bytes(), true).is_ok())
}

/// The name a Tessera folder's Stop is kept under elsewhere (the registry
/// on Windows): the same for the same folder, every time.
#[cfg_attr(not(windows), allow(dead_code))]
fn record_name(paths: &Paths) -> String {
    let path = paths.config_dir.to_string_lossy().to_lowercase();
    // FNV-1a: stable across builds (unlike the standard hasher).
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

impl StopLatch {
    const UNREADABLE: Self = Self {
        stopped: true,
        stops: 1,
        at: 0,
    };

    /// As it is now. With Tessera Guard (Windows) the guard's Stop, which
    /// only a Resume approved in Windows' administrator prompt undoes (a
    /// Resume kept here counts for nothing then; a Stop counted here that
    /// the guard hasn't got yet stops all the same).
    pub fn load(paths: &Paths) -> Self {
        let l = Self::load_local(paths);
        match crate::guard::here() {
            crate::guard::Here::Says(g) => {
                let m = crate::guard::with_guard(l.stopped, l.stops, g);
                Self {
                    stopped: m.stopped,
                    stops: m.stops,
                    at: l.at,
                }
            }
            _ => l,
        }
    }

    /// As kept on this computer: `stop.json`, and the other places a Stop
    /// is kept when that file couldn't be written (a count above the file's
    /// is a Stop it hasn't got). Any of them that can't be read stops
    /// everything (a Stop is never lost to a damaged file).
    pub fn load_local(paths: &Paths) -> Self {
        let mut l = match std::fs::read_to_string(stop_file(paths)) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(l) => l,
                Err(_) => return Self::UNREADABLE,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => return Self::UNREADABLE,
        };
        let mut kept = |count: u32| {
            if count > l.stops {
                l.stops = count;
                l.stopped = true;
            }
        };
        match std::fs::read_to_string(pending_file(paths)) {
            Ok(text) => match serde_json::from_str::<PendingStop>(&text) {
                Ok(p) => kept(p.stops),
                Err(_) => return Self::UNREADABLE,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Self::UNREADABLE,
        }
        #[cfg(windows)]
        match crate::platform::windows::stop_record(&record_name(paths)) {
            Ok(Some(count)) => kept(count),
            Ok(None) => {}
            Err(_) => return Self::UNREADABLE,
        }
        // Stops kept without the lock, each in a file of its own: there,
        // stopped (only a Resume that read one removes it).
        match unacknowledged(paths) {
            Ok(own) => {
                for (_, p) in &own {
                    l.stops = l.stops.max(p.stops);
                }
                if !own.is_empty() {
                    l.stopped = true;
                }
            }
            Err(_) => return Self::UNREADABLE,
        }
        l
    }

    /// Stop or resume this computer (the only ways it changes); each is
    /// written even when it already was, so a waiting machine notices. The
    /// count never goes below `at_least` (what a machine already counted).
    pub fn set(paths: &Paths, stopped: bool, at_least: u32) -> Result<Self> {
        if stopped {
            return Self::stop(paths, at_least);
        }
        with_stop_lock(paths, || {
            // The Stops kept without the lock that this Resume sees: only
            // those it answers (one made after this look stays and stops).
            let seen = unacknowledged(paths)?;
            // Every place's count taken in, so the Resume is above them all.
            let mut l = Self::load(paths);
            l.stops = l.stops.max(at_least);
            l.stopped = false;
            l.at = crate::util::now_ms().max(l.at + 1);
            crate::config::write_atomic(
                &stop_file(paths),
                serde_json::to_string(&l)?.as_bytes(),
                true,
            )?;
            for (file, _) in seen {
                let _ = std::fs::remove_file(file);
            }
            // A damaged place is written anew (its count no higher than the
            // Resume's, so it doesn't stop anything again).
            Self::repair(paths, l.stops);
            Ok(l)
        })
    }

    /// Stop this computer. One change at a time, like a Resume (read,
    /// counted and written under the lock), and always counted anew: a
    /// Resume never covers a Stop it didn't see. Kept in every place that
    /// takes it (beside `stop.json`, in the registry on Windows, and in
    /// `stop.json`), so a file that can't be written doesn't lose it, also
    /// across a restart. When the lock can't be had (something holds it
    /// too long), beside `stop.json` all the same, counted above everything
    /// read by then. An error only when no place took it (it then holds in
    /// the machine's memory).
    pub fn stop(paths: &Paths, at_least: u32) -> Result<Self> {
        // To Tessera Guard first (Windows), before anything here can fail:
        // it ends what the machine started, and only a Resume approved in
        // Windows' prompt undoes it.
        let forwarded =
            crate::guard::forward_stop(Self::load(paths).stops.max(at_least).saturating_add(1));
        let next = |paths: &Paths| {
            let now = Self::load(paths);
            Self {
                stopped: true,
                stops: now.stops.max(at_least).wrapping_add(1),
                at: crate::util::now_ms().max(now.at + 1),
            }
        };
        let locked = with_stop_lock(paths, || {
            let l = next(paths);
            let kept = Self::keep_stop(paths, &l);
            match crate::config::write_atomic(
                &stop_file(paths),
                serde_json::to_string(&l)?.as_bytes(),
                true,
            ) {
                Ok(()) => Ok(l),
                Err(_) if kept => Ok(l),
                Err(e) => Err(e),
            }
        });
        let kept = match locked {
            Ok(l) => Ok(l),
            Err(e) => {
                // Without the lock: in a file of its own (two such Stops never
                // share one, so a Resume can't answer one it didn't see), and
                // in the other places as well.
                let _ = std::fs::create_dir_all(&paths.config_dir);
                let l = next(paths);
                let own = keep_own_stop(paths, &l);
                if Self::keep_stop(paths, &l) || own {
                    Ok(l)
                } else {
                    Err(e)
                }
            }
        };
        let counted = kept
            .as_ref()
            .map_or(at_least.saturating_add(1), |l| l.stops);
        match kept {
            Err(_) if forwarded => Ok(Self {
                stopped: true,
                stops: counted,
                at: crate::util::now_ms(),
            }),
            other => other,
        }
    }

    /// `l` (a Stop) beside `stop.json` and, on Windows, in the registry:
    /// whether any of them took it.
    fn keep_stop(paths: &Paths, l: &Self) -> bool {
        let pending = serde_json::to_string(&PendingStop {
            stops: l.stops,
            at: l.at,
        })
        .ok()
        .is_some_and(|text| {
            crate::config::write_atomic(&pending_file(paths), text.as_bytes(), true).is_ok()
        });
        #[cfg(windows)]
        let record =
            crate::platform::windows::set_stop_record(&record_name(paths), l.stops).is_ok();
        #[cfg(not(windows))]
        let record = false;
        pending || record
    }

    /// Write the other places anew where they can't be read (after a
    /// Resume: at its count). Best effort.
    fn repair(paths: &Paths, stops: u32) {
        let pending = pending_file(paths);
        let damaged = match std::fs::read_to_string(&pending) {
            Ok(text) => serde_json::from_str::<PendingStop>(&text).is_err(),
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        };
        if damaged && let Ok(text) = serde_json::to_string(&PendingStop { stops, at: 0 }) {
            let _ = crate::config::write_atomic(&pending, text.as_bytes(), true);
        }
        #[cfg(windows)]
        if crate::platform::windows::stop_record(&record_name(paths)).is_err() {
            let _ = crate::platform::windows::set_stop_record(&record_name(paths), stops);
        }
    }
}

impl StopLatch {
    /// Bring the file's count up to `at_least` (what a machine counted, a
    /// Stop it held in memory included); the rest stays as it is.
    pub fn raise(paths: &Paths, at_least: u32) -> Result<Self> {
        with_stop_lock(paths, || {
            let mut l = Self::load(paths);
            if l.stops < at_least {
                l.stops = at_least;
                crate::config::write_atomic(
                    &stop_file(paths),
                    serde_json::to_string(&l)?.as_bytes(),
                    true,
                )?;
            }
            Ok(l)
        })
    }
}

/// Run `f` holding `stop.lock` (one Stop or Resume at a time, across
/// processes). Never without it: after two seconds it fails instead (a
/// Stop then holds in the machine's memory, and says so).
fn with_stop_lock<T>(paths: &Paths, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::fs::create_dir_all(&paths.config_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.config_dir.join("stop.lock"))?;
    let mut held = false;
    for _ in 0..100 {
        if try_lock_file(&file) {
            held = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !held {
        anyhow::bail!("another Stop or Resume is being written right now");
    }
    // The lock goes with the file.
    let r = f();
    drop(file);
    r
}

/// The user's Stop for settings read from `node.json`: its own file's, or
/// (before `stop.json` existed) the one the previous build kept in
/// `node.json`, moved to its own file once. If that can't be written yet,
/// the old Stop still holds: an update never undoes it.
fn stop_for(paths: &Paths, legacy: &MachineScope) -> StopLatch {
    if stop_file(paths).exists() || !(legacy.stopped || legacy.stops > 0) {
        return StopLatch::load(paths);
    }
    let kept = StopLatch {
        stopped: legacy.stopped,
        stops: legacy.stops,
        at: crate::util::now_ms(),
    };
    let _ = with_stop_lock(paths, || {
        if !stop_file(paths).exists() {
            crate::config::write_atomic(
                &stop_file(paths),
                serde_json::to_string(&kept)?.as_bytes(),
                true,
            )?;
        }
        Ok(())
    });
    if stop_file(paths).exists() {
        StopLatch::load(paths)
    } else {
        kept
    }
}

/// An exclusive lock on `file`, without waiting (true only when held).
fn try_lock_file(file: &std::fs::File) -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid descriptor for as long as `file` lives.
        // Held only when it really is: any other answer (no locks on this
        // file system, say) fails closed.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }
    #[cfg(windows)]
    {
        // Lock contention and any OS/filesystem error leave it unheld.
        // Stop can use its pending record; Resume never proceeds unlocked.
        crate::platform::windows::try_lock(file).unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        true
    }
}

pub fn log_file(paths: &Paths) -> PathBuf {
    paths.data_dir.join("node-log.jsonl")
}

/// The server login handed to the node by the desktop app (`tessera node
/// enable --server-stdin`) or a pairing link (`tessera node enable <link>`);
/// otherwise the CLI's own `client.json` is used.
pub fn server_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("node-server.json")
}

impl NodeConfig {
    pub fn load(paths: &Paths) -> Result<Self> {
        let mut c: Self = match std::fs::read_to_string(config_file(paths)) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("reading {}", config_file(paths).display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        let latch = stop_for(paths, &c.scope);
        mirror_stop(&mut c.scope, latch);
        Ok(c)
    }

    /// Write the settings. The user's Stop isn't one of them: what is
    /// written mirrors its own file as it is now, whatever this copy says.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let mut me = self.clone();
        // What `node.json` holds now (an older Stop kept there included).
        let on_disk = std::fs::read_to_string(config_file(paths))
            .ok()
            .and_then(|t| serde_json::from_str::<Self>(&t).ok())
            .map(|c| c.scope)
            .unwrap_or_default();
        mirror_stop(&mut me.scope, stop_for(paths, &on_disk));
        crate::config::write_atomic(
            &config_file(paths),
            serde_json::to_string_pretty(&me)?.as_bytes(),
            true,
        )
    }

    /// Set the scope, remembering the folders of "Look only" and "Work in
    /// folders" for when the user comes back to them.
    pub fn set_scope(&mut self, mut scope: MachineScope, home: Option<&Path>) {
        // The user's Stop isn't a setting a new scope undoes (only Resume
        // does; `save` writes it from its own file anyway).
        scope.stopped = self.scope.stopped;
        scope.stops = self.scope.stops;
        if matches!(
            preset_of(&scope, home),
            MachinePreset::LookOnly | MachinePreset::WorkInFolders
        ) {
            self.chosen = scope.folders.iter().map(|f| f.path.clone()).collect();
        }
        self.scope = scope;
    }
}

/// The running node's state, for `tessera node status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeState {
    pub pid: u32,
    pub connected: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub calls: u32,
    #[serde(default)]
    pub updated_at: u64,
    /// The Stop bar (Windows, desktop control on): its key, or why it
    /// couldn't start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_bar: Option<String>,
    /// Why Tessera Guard didn't take this machine in (Windows), if it didn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_error: Option<String>,
}

impl NodeState {
    pub fn load(paths: &Paths) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(state_file(paths)).ok()?).ok()
    }
}

/// This computer's name.
pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
            if !name.is_empty() && name != "localhost" {
                return crate::util::one_line(&name, 60);
            }
        }
    }
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| crate::util::one_line(&s, 60))
        .unwrap_or_else(|| "This computer".into())
}

/// Desktop control is Linux's command session or native screen/input.
fn desktop_built(linux: bool, screen: bool) -> bool {
    linux || screen
}
/// Backends this node implements, not permissions; shared by status and hello.
pub fn features() -> Vec<String> {
    let mut out = Vec::new();
    if screen::AVAILABLE {
        out.push("desktop".to_string());
    }
    if cfg!(target_os = "linux") {
        out.push("desktop_session".to_string());
    }
    if cfg!(unix) {
        out.extend(["terminal", "admin", "webviews"].map(String::from));
    }
    if cfg!(windows) {
        out.extend(["terminal", "webviews"].map(String::from));
    }
    if crate::sandbox::confines_writes() {
        out.push("sandbox".into());
    }
    out
}

/// A scope shows the user's Stop as its own file has it.
pub(crate) fn mirror_stop(scope: &mut MachineScope, latch: StopLatch) {
    scope.stopped = latch.stopped;
    scope.stops = latch.stops;
}

/// Why a machine its user stopped refuses everything.
pub const STOPPED_HERE: &str = "the user stopped Tessera on this computer (its Stop bar, the Stop key or `tessera node stop`); it does nothing until they resume it there. Don't retry: tell the user";

/// Why an action asked for before the user's latest Stop never runs, even
/// after a Resume.
pub const ASKED_BEFORE_STOP: &str = "this was asked for before the user's latest Stop on this computer; ask again if it is still needed";

/// What `action` does, in a few words, for the Stop bar (never typed text).
pub fn doing(action: &NodeAction) -> String {
    match action {
        NodeAction::Desktop { desktop, look } => {
            let mut d = desktop.describe();
            if *look && desktop.acts() {
                d.push_str(", then a look at the screen");
            }
            d
        }
        // Not its text: any part of it may hold a password.
        NodeAction::Run { admin, .. } => if *admin {
            "a command as administrator"
        } else {
            "a command"
        }
        .to_string(),
        a => format!("{} {}", a.name(), crate::util::one_line(&a.target(), 60)),
    }
}

/// e.g. `linux x86_64`.
pub fn os() -> String {
    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    // Agents elsewhere must know which shell their commands meet here.
    #[cfg(windows)]
    let os = format!("{os}, shell {}", crate::platform::shell().name());
    os
}

/// This computer's home folder (`~` in a machine's paths), resolved.
pub fn home() -> Option<PathBuf> {
    dirs::home_dir()
        .filter(|h| h.is_absolute())
        .map(|h| crate::tools::canonical(&h))
}

/// The account's home folder as the system records it: `$HOME` may point
/// elsewhere, and Tessera's own files there stay protected all the same.
pub fn account_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buf = vec![0u8; 16 * 1024];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let r = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut out,
            )
        };
        if r != 0 || out.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        use std::os::unix::ffi::OsStrExt;
        let dir = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) };
        let p = PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes()));
        (p.is_absolute() && p.parent().is_some()).then(|| crate::tools::canonical(&p))
    }
    #[cfg(not(unix))]
    None
}

/// Whether commands can run on this computer with `scope`: on Linux with
/// the sandbox that keeps their changes inside the read-write folders
/// (Landlock); on Windows, which has no sandbox yet, only with Full access
/// (whose description there says they run unconfined).
pub fn commands_run_here(scope: &MachineScope) -> bool {
    if cfg!(windows) {
        scope.commands && preset_of(scope, home().as_deref()) == MachinePreset::FullAccess
    } else {
        crate::sandbox::confines_writes()
    }
}

/// Why commands don't run here (when [`commands_run_here`] says no).
pub fn commands_refused_why() -> String {
    if cfg!(windows) {
        "on Windows, commands run without a sandbox (Tessera has none there yet), so a machine runs them only with Full access, chosen on that computer".into()
    } else {
        format!(
            "commands need the sandbox with Landlock ABI 3 (Linux 6.2 or newer) on this machine, but it is {}",
            crate::sandbox::describe()
        )
    }
}

/// `raw` with a leading `~` meaning `home`.
pub fn expand(raw: &str, home: Option<&Path>) -> PathBuf {
    match (raw.strip_prefix('~'), home) {
        (Some(""), Some(h)) => h.to_path_buf(),
        (Some(rest), Some(h)) if rest.starts_with('/') => h.join(rest.trim_start_matches('/')),
        (Some(rest), Some(h)) if cfg!(windows) && rest.starts_with('\\') => {
            h.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(raw),
    }
}

/// Both homes: `$HOME` and the account's (usually the same).
fn homes(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = home.map(Path::to_path_buf).into_iter().collect();
    v.extend(account_home());
    v.sort();
    v.dedup();
    v
}

/// Tessera's own data and settings on this computer: this node's, the
/// default ones (a server or terminal here may use them) and the desktop
/// app's saved servers. Out of reach of every action, whatever the scope.
pub fn protected(paths: &Paths) -> Protected {
    let mut roots = vec![paths.config_dir.clone(), paths.data_dir.clone()];
    roots.extend(dirs::config_dir().map(|d| d.join("tessera")));
    roots.extend(dirs::data_dir().map(|d| d.join("tessera")));
    roots.extend(Paths::app_data_dir());
    // Windows: the desktop app's web storage (WebView2) is in LocalAppData
    // (elsewhere this is the folder above again).
    roots.extend(dirs::data_local_dir().map(|d| d.join("dev.tessera.app")));
    // The account's own, should `$HOME` point somewhere else.
    if let Some(h) = account_home() {
        roots.push(h.join(".config/tessera"));
        roots.push(h.join(".local/share/tessera"));
        roots.push(h.join(".local/share/dev.tessera.app"));
    }
    roots.sort();
    roots.dedup();
    Protected {
        roots,
        except: Vec::new(),
    }
}

/// Where other programs keep credentials (under the home folder). Out of
/// reach unless that folder itself (or one inside it) is allowed.
pub fn credential_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    const DIRS: &[&str] = &[
        ".ssh",
        ".gnupg",
        ".aws",
        ".azure",
        ".config/gcloud",
        ".kube",
        ".docker",
        ".password-store",
        ".config/gh",
        ".config/hub",
        ".config/op",
        ".config/Bitwarden",
        ".config/Bitwarden CLI",
        ".config/rclone",
        ".local/share/keyrings",
        ".local/share/kwalletd",
        ".pki",
        ".mozilla",
        ".thunderbird",
        ".config/google-chrome",
        ".config/chromium",
        ".config/BraveSoftware",
        ".config/vivaldi",
        ".config/microsoft-edge",
        ".codex",
        ".claude",
        ".terraform.d",
        // macOS: keychains, browser profiles and mail.
        "Library/Keychains",
        "Library/Cookies",
        "Library/Mail",
        "Library/Application Support/Google/Chrome",
        "Library/Application Support/Chromium",
        "Library/Application Support/BraveSoftware",
        "Library/Application Support/Firefox",
        "Library/Application Support/Microsoft Edge",
        "Library/Safari",
    ];
    let windows: &[&str] = if cfg!(windows) {
        WINDOWS_CREDENTIALS
    } else {
        &[]
    };
    homes(home)
        .iter()
        .flat_map(|h| DIRS.iter().chain(windows).map(move |d| h.join(d)))
        .collect()
}

/// Windows: the credential store, DPAPI keys, browser profiles and the
/// usual programs' sign-ins, under the user's profile folder.
const WINDOWS_CREDENTIALS: &[&str] = &[
    r"AppData\Roaming\Microsoft\Credentials",
    r"AppData\Local\Microsoft\Credentials",
    r"AppData\Roaming\Microsoft\Protect",
    r"AppData\Roaming\Microsoft\Crypto",
    r"AppData\Roaming\Microsoft\SystemCertificates",
    r"AppData\Local\Microsoft\Vault",
    r"AppData\Local\Google\Chrome\User Data",
    r"AppData\Local\Chromium\User Data",
    r"AppData\Local\Microsoft\Edge\User Data",
    r"AppData\Local\BraveSoftware",
    r"AppData\Local\Vivaldi",
    r"AppData\Roaming\Opera Software",
    r"AppData\Roaming\Mozilla",
    r"AppData\Roaming\Thunderbird",
    r"AppData\Roaming\gcloud",
    r"AppData\Roaming\GitHub CLI",
    r"AppData\Roaming\Bitwarden",
    r"AppData\Roaming\Bitwarden CLI",
    r"AppData\Roaming\1Password",
    r"AppData\Local\1Password",
    r"AppData\Roaming\rclone",
    r"AppData\Roaming\Claude",
];

/// Other programs' credentials kept as single files: commands don't see
/// them (they read as empty) unless their own folder was allowed, as
/// file actions refuse them ([`crate::tools::credential_file`]).
pub fn credential_files(home: Option<&Path>) -> Vec<PathBuf> {
    const FILES: &[&str] = &[
        ".netrc",
        ".git-credentials",
        ".pgpass",
        ".npmrc",
        ".pypirc",
        ".cargo/credentials.toml",
        ".cargo/credentials",
        ".Xauthority",
    ];
    // Windows' curl reads `_netrc`.
    let windows: &[&str] = if cfg!(windows) { &["_netrc"] } else { &[] };
    homes(home)
        .iter()
        .flat_map(|h| FILES.iter().chain(windows).map(move |f| h.join(f)))
        .collect()
}

/// Files and folders under the home folder that run programs later,
/// outside any sandbox: shell startup files, login services, autostart,
/// session and window manager settings. Machines change them only with
/// desktop control on (each is backed up first).
pub const STARTUP: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".zlogout",
    ".xprofile",
    ".xinitrc",
    ".xsession",
    ".xsessionrc",
    ".pam_environment",
    ".ssh/rc",
    ".ssh/environment",
    ".config/systemd",
    ".config/autostart",
    ".config/environment.d",
    ".config/fish",
    ".config/hypr",
    ".config/i3",
    ".config/sway",
    ".config/niri",
    ".config/river",
    ".config/labwc",
    ".config/uwsm",
    ".config/waybar",
    ".config/plasma-workspace",
    ".local/share/systemd",
    ".local/share/applications",
    // macOS login items.
    "Library/LaunchAgents",
];

/// The startup files and folders under `home` ([`STARTUP`]).
pub fn startup_paths(home: Option<&Path>) -> Vec<PathBuf> {
    let windows: &[&str] = if cfg!(windows) { WINDOWS_STARTUP } else { &[] };
    home.map(|h| STARTUP.iter().chain(windows).map(|d| h.join(d)).collect())
        .unwrap_or_default()
}

/// Deferred execution locations protected for ordinary server commands.
pub(crate) fn server_startup_paths(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = startup_paths(home);
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        v.extend(
            STARTUP
                .iter()
                .filter_map(|s| s.strip_prefix(".config/"))
                .map(|s| config.join(s)),
        );
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        v.extend([data.join("systemd"), data.join("applications")]);
    }
    #[cfg(unix)]
    v.extend(["/var/spool/cron", "/var/spool/at", "/var/spool/anacron"].map(PathBuf::from));
    v
}

/// Windows: the sign-in folder and PowerShell's profile scripts (which run
/// with every PowerShell, like `.bashrc`), in Documents or OneDrive's copy.
const WINDOWS_STARTUP: &[&str] = &[
    r"AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
    r"Documents\PowerShell",
    r"Documents\WindowsPowerShell",
    r"OneDrive\Documents\PowerShell",
    r"OneDrive\Documents\WindowsPowerShell",
];

/// The Tessera program this machine runs (and the desktop app beside it):
/// never changed by a machine, whatever it allows.
pub fn program_paths() -> Vec<PathBuf> {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|e| crate::platform::canonicalize(&e).ok())
    else {
        return Vec::new();
    };
    let mut v = vec![exe.clone()];
    // Windows: the whole folder (the app installs its programs and their
    // libraries there; a library dropped beside them would load when the
    // user approves running one raised, to install Tessera Guard).
    if cfg!(windows)
        && let Some(dir) = exe.parent()
    {
        v.push(dir.to_path_buf());
    }
    let names: &[&str] = if cfg!(windows) {
        &[
            "tessera.exe",
            "tessera-service.exe",
            "Tessera.exe",
            "tessera-app.exe",
        ]
    } else {
        &["tessera", "tessera-desktop"]
    };
    if let Some(dir) = exe.parent() {
        for name in names {
            let p = dir.join(name);
            if p.exists() && !v.contains(&p) {
                v.push(p);
            }
        }
    }
    v
}

/// Tessera's program and the service files that start it on a server
/// (`tessera service install` writes them under `home`'s
/// `.config/systemd`; `.config/environment.d` sets the service manager's
/// variables): commands and terminals in the server's sandbox may read them
/// but never change them, or one could swap them and run outside the
/// sandbox after the next restart. (A machine keeps all of its startup
/// files as they are; see [`startup_paths`].)
pub fn server_program_paths(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = program_paths();
    for h in homes(home) {
        v.push(h.join(".config/systemd"));
        v.push(h.join(".config/environment.d"));
        v.push(h.join(".local/share/systemd"));
    }
    v
}

/// `path` (absolute or `~/…`, as written) is a startup file or inside a
/// startup folder of `home`, or may be: a path that climbs with `..`
/// counts, to be safe (the machine resolves it for real).
pub fn may_be_startup(path: &str, home: Option<&Path>) -> bool {
    let p = expand(path.trim(), home);
    p.components().any(|c| c == std::path::Component::ParentDir)
        || startup_paths(home).iter().any(|s| p.starts_with(s))
}

/// A folder rule the user is adding here, checked and made canonical.
pub fn check_folder(
    paths: &Paths,
    raw: &str,
    write: bool,
    home: Option<&Path>,
) -> Result<FolderAccess, String> {
    let raw = raw.trim();
    let path = expand(raw, home);
    if !path.is_absolute() {
        return Err(format!(
            "`{raw}` is not a full path; start it with {}",
            if cfg!(windows) {
                r"a drive (C:\) or ~\"
            } else {
                "/ or ~/"
            }
        ));
    }
    let canon = crate::platform::canonicalize(&path)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| format!("there is no folder `{raw}`"))?;
    if canon.parent().is_none() {
        return Err("the whole disk can't be allowed; choose the folders agents may use".into());
    }
    let protected = protected(paths).canonical();
    if protected.roots.iter().any(|r| canon.starts_with(r)) {
        return Err(format!(
            "`{raw}` is inside Tessera's own data or settings, which agents never get"
        ));
    }
    if write
        && homes(home)
            .iter()
            .any(|h| h.starts_with(&canon) && *h != canon)
    {
        return Err(format!(
            "`{raw}` can only be read-only: it holds your whole home folder and more. Choose Full access for your home folder, or allow folders inside it read-write."
        ));
    }
    Ok(FolderAccess {
        path: canon.display().to_string(),
        write,
    })
}

/// What a Mac can't have yet. Its startup and credential locations are
/// only partly known to these checks, and its disks ignore letter case
/// (`~/library` is `~/Library`), so the whole home folder read-write (Full
/// access) and desktop control aren't offered there; chosen folders are.
fn mac_limits(folders: &[FolderAccess], desktop: bool, home: Option<&Path>) -> Result<(), String> {
    if desktop {
        return Err("desktop control isn't available on a Mac yet".into());
    }
    let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    let homes: Vec<PathBuf> = homes(home).iter().map(|h| lower(h)).collect();
    for f in folders.iter().filter(|f| f.write) {
        let p = lower(Path::new(&f.path));
        if homes
            .iter()
            .any(|h| h.starts_with(&p) || p.starts_with(h.join("library")))
        {
            return Err(format!(
                "on a Mac, `{}` can only be read-only for now: it holds your login items and app data, which Tessera can't keep safe there yet. Allow the folders agents need read-write instead (Work in folders).",
                f.path
            ));
        }
    }
    Ok(())
}

/// `scope` with every folder checked and made canonical (a later rule for
/// the same folder wins), the time limit within bounds and the bots tidy.
pub fn check_scope(
    paths: &Paths,
    scope: &MachineScope,
    home: Option<&Path>,
) -> Result<MachineScope, String> {
    check_scope_on(paths, scope, home, cfg!(target_os = "macos"))
}

fn check_scope_on(
    paths: &Paths,
    scope: &MachineScope,
    home: Option<&Path>,
    mac: bool,
) -> Result<MachineScope, String> {
    let mut folders: Vec<FolderAccess> = Vec::new();
    for f in &scope.folders {
        let f = check_folder(paths, &f.path, f.write, home)?;
        folders.retain(|o| o.path != f.path);
        folders.push(f);
    }
    if mac {
        mac_limits(&folders, scope.desktop, home)?;
    }
    if scope.desktop && !desktop_built(cfg!(target_os = "linux"), screen::AVAILABLE) {
        return Err("desktop control isn't available on this computer yet".into());
    }
    if cfg!(windows) && scope.terminal == TerminalAccess::Sandboxed {
        return Err("Windows terminals are not sandboxed; choose full access locally, or leave terminals off".into());
    }
    if scope.admin != AdminAccess::Off {
        if !cfg!(unix) {
            return Err("administrator commands aren't available on Windows yet".into());
        }
        if !scope.commands {
            return Err("administrator commands need commands on (Work in folders, Full access or Desktop control)".into());
        }
    }
    if folders.len() > MAX_FOLDERS {
        return Err(format!("at most {MAX_FOLDERS} folders"));
    }
    let bots = match &scope.bots {
        None => None,
        Some(list) => {
            let mut out: Vec<String> = Vec::new();
            for b in list {
                let b = b.trim().trim_start_matches('@');
                if b.is_empty() {
                    continue;
                }
                if b.len() > 64 || b.chars().any(char::is_control) {
                    return Err(format!("`{b}` isn't a bot's id or handle"));
                }
                if !out.iter().any(|o| o.eq_ignore_ascii_case(b)) {
                    out.push(b.to_string());
                }
            }
            if out.len() > MAX_BOTS {
                return Err(format!("at most {MAX_BOTS} bots"));
            }
            Some(out)
        }
    };
    Ok(MachineScope {
        folders,
        commands: scope.commands,
        ask: scope.ask,
        timeout_secs: clamp_timeout(scope.timeout_secs),
        desktop: scope.desktop,
        bots,
        webviews: scope.webviews,
        terminal: scope.terminal,
        admin: scope.admin,
        stopped: scope.stopped,
        stops: scope.stops,
    })
}

pub fn clamp_timeout(secs: u64) -> u64 {
    secs.clamp(MIN_TIMEOUT_SECS, tessera_proto::MACHINE_TIMEOUT_MAX)
}

// ---- presets -----------------------------------------------------------------

/// How much an "Ask first" choice lets through without asking (higher: less).
pub fn ask_rank(a: MachineAsk) -> u8 {
    match a {
        MachineAsk::Always => 0,
        MachineAsk::Risky => 1,
        MachineAsk::Chat => 2,
    }
}

/// "Ask first", in plain words.
pub fn ask_name(a: MachineAsk) -> &'static str {
    match a {
        MachineAsk::Always => "every action",
        MachineAsk::Risky => "only risky ones",
        MachineAsk::Chat => "never (the chat's own mode decides)",
    }
}

/// What a preset asks by default: desktop control asks before everything.
pub fn default_ask(p: MachinePreset) -> MachineAsk {
    match p {
        MachinePreset::DesktopControl | MachinePreset::Custom => MachineAsk::Always,
        _ => MachineAsk::Risky,
    }
}

pub fn preset_name(p: MachinePreset) -> &'static str {
    match p {
        MachinePreset::LookOnly => "Look only",
        MachinePreset::WorkInFolders => "Work in folders",
        MachinePreset::FullAccess => "Full access",
        MachinePreset::DesktopControl => "Desktop control",
        MachinePreset::Custom => "Advanced",
    }
}

/// One plain sentence: what the preset allows.
pub fn preset_sentence(p: MachinePreset) -> &'static str {
    preset_sentence_with_sandbox(p, std::env::consts::OS, crate::sandbox::confines_writes())
}

/// [`preset_sentence`] for a machine running `os` ([`os`]'s text).
pub fn preset_sentence_for(p: MachinePreset, os: &str) -> &'static str {
    preset_sentence_with_sandbox(p, os, os.starts_with("linux"))
}

/// Checked file actions do not need the command sandbox.
pub fn preset_sentence_with_sandbox(p: MachinePreset, os: &str, sandbox: bool) -> &'static str {
    if p == MachinePreset::WorkInFolders && !sandbox {
        return "Agents can read and change the folders you choose. Commands aren't available with this choice on this computer.";
    }
    if os.starts_with("windows") {
        return windows_preset_sentence(p);
    }
    match p {
        MachinePreset::LookOnly => "Agents can read the folders you choose, and change nothing.",
        MachinePreset::WorkInFolders => {
            "Agents can read and change the folders you choose, and run commands there. Commands can also read other files your account can read, except protected passwords and keys. They can change only the chosen folders."
        }
        MachinePreset::FullAccess => {
            "Agents can read and change everything in your home folder, run commands and open web views here. Startup files, passwords and keys stay protected."
        }
        MachinePreset::DesktopControl => {
            "Full access, plus your startup and window manager settings and your desktop session. This is close to giving the agent your account."
        }
        MachinePreset::Custom => "Your own folder rules.",
    }
}

/// Windows has no command sandbox yet: commands run only with Full access,
/// and then with the account's full rights. Said as plainly as that.
fn windows_preset_sentence(p: MachinePreset) -> &'static str {
    match p {
        MachinePreset::LookOnly => "Agents can read the folders you choose, and change nothing.",
        MachinePreset::WorkInFolders => {
            "Agents can read and change the folders you choose, and run commands there. Commands can also read other files your account can read, except protected passwords and keys. They can change only the chosen folders."
        }
        MachinePreset::FullAccess => {
            "Agents can read and change everything in your home folder and run commands here. On Windows commands run without a sandbox: they can reach everything your account can, including passwords and keys. This is close to giving the agent your account."
        }
        MachinePreset::DesktopControl => {
            "Full access, plus seeing your screens and using your mouse and keyboard while you're signed in. This is close to giving the agent your account."
        }
        MachinePreset::Custom => {
            "Your own folder rules. Commands need Full access on Windows (Tessera has no command sandbox there yet)."
        }
    }
}

/// The preset a scope amounts to (`home`: this computer's home folder, as
/// the folder paths spell it).
pub fn preset_of(scope: &MachineScope, home: Option<&Path>) -> MachinePreset {
    let home_rw = home.is_some_and(|h| {
        scope
            .folders
            .iter()
            .any(|f| f.write && Path::new(f.path.trim_end_matches('/')) == h)
    });
    let all_ro = scope.folders.iter().all(|f| !f.write);
    let all_rw = scope.folders.iter().all(|f| f.write);
    match (scope.desktop, home_rw, scope.commands) {
        (true, true, true) if scope.folders.len() == 1 => MachinePreset::DesktopControl,
        (true, _, _) => MachinePreset::Custom,
        (false, true, true) if scope.folders.len() == 1 => MachinePreset::FullAccess,
        (false, false, false) if all_ro && !scope.webviews => MachinePreset::LookOnly,
        (false, false, _) if all_rw && (!scope.folders.is_empty() || scope.commands) => {
            MachinePreset::WorkInFolders
        }
        _ => MachinePreset::Custom,
    }
}

/// The scope a preset gives, from what this computer has now. `folders`:
/// the folders for "Look only" and "Work in folders" (default: the ones
/// chosen before). Asking never gets looser by switching presets: only the
/// "Ask first" choice itself does that.
pub fn apply_preset(
    paths: &Paths,
    config: &NodeConfig,
    preset: MachinePreset,
    folders: Option<Vec<String>>,
    home: Option<&Path>,
) -> Result<MachineScope, String> {
    apply_preset_with_sandbox(
        paths,
        config,
        preset,
        folders,
        home,
        crate::sandbox::confines_writes(),
    )
}

fn apply_preset_with_sandbox(
    paths: &Paths,
    config: &NodeConfig,
    preset: MachinePreset,
    folders: Option<Vec<String>>,
    home: Option<&Path>,
    sandbox: bool,
) -> Result<MachineScope, String> {
    let current = &config.scope;
    let chosen = folders.unwrap_or_else(|| {
        if config.chosen.is_empty()
            && matches!(
                preset_of(current, home),
                MachinePreset::LookOnly | MachinePreset::WorkInFolders
            )
        {
            current.folders.iter().map(|f| f.path.clone()).collect()
        } else {
            config.chosen.clone()
        }
    });
    let mut scope = MachineScope {
        ask: if ask_rank(current.ask) <= ask_rank(default_ask(preset)) {
            current.ask
        } else {
            default_ask(preset)
        },
        timeout_secs: current.timeout_secs,
        bots: current.bots.clone(),
        // Included in Full access and Desktop control; optional with Work in
        // folders (kept only while staying there), never with Look only.
        webviews: match preset {
            MachinePreset::FullAccess | MachinePreset::DesktopControl => true,
            MachinePreset::WorkInFolders => {
                current.webviews && preset_of(current, home) == MachinePreset::WorkInFolders
            }
            _ => false,
        },
        // The user's own terminals aren't about agents: kept as they are.
        terminal: current.terminal,
        // Administrator stays as chosen where commands stay on (set below).
        admin: current.admin,
        ..MachineScope::default()
    };
    match preset {
        MachinePreset::LookOnly | MachinePreset::WorkInFolders => {
            let write = preset == MachinePreset::WorkInFolders;
            scope.folders = chosen
                .iter()
                .map(|p| FolderAccess {
                    path: p.clone(),
                    write,
                })
                .collect();
            scope.commands = write && sandbox;
        }
        MachinePreset::FullAccess | MachinePreset::DesktopControl => {
            let home = home.ok_or("this computer has no home folder to allow")?;
            scope.folders = vec![FolderAccess {
                path: home.display().to_string(),
                write: true,
            }];
            scope.commands = true;
            scope.desktop = preset == MachinePreset::DesktopControl;
        }
        MachinePreset::Custom => {
            return Err(
                "Advanced has no fixed settings: change folders, commands and the rest one by one (`tessera node scope`)"
                    .into(),
            );
        }
    }
    if !scope.commands {
        scope.admin = AdminAccess::Off;
    }
    check_scope(paths, &scope, home)
}

/// May `bot` (`(id, handle)`; none: a plain chat) use a machine with
/// `scope`? The same rule as for credentials ([`tessera_proto::bot_allowed`]);
/// a machine set up in its terminal may also name a bot by its handle.
/// `Err` says why not, in plain words.
pub fn may_use(scope: &MachineScope, bot: Option<(&str, &str)>) -> Result<(), String> {
    let allowed = scope.bots.as_deref();
    let by_handle = |(_, handle): (&str, &str)| {
        let handle = handle.trim_start_matches('@');
        !handle.is_empty()
            && allowed.is_some_and(|l| {
                l.iter()
                    .any(|a| a.trim_start_matches('@').eq_ignore_ascii_case(handle))
            })
    };
    if tessera_proto::bot_allowed(allowed, bot.map(|b| b.0)) || bot.is_some_and(by_handle) {
        return Ok(());
    }
    Err(match bot {
        Some(_) => "only certain bots may use it, and this one isn't among them".into(),
        None => "only certain bots may use it, not a plain chat".into(),
    })
}

/// Why an action counts as risky ("Ask first: only risky ones"): a command
/// the safety rules flag, or a change to a startup file (`startup` says
/// whether a path is one).
pub fn risky(action: &NodeAction, startup: impl Fn(&str) -> bool) -> Option<String> {
    risky_on(action, startup, cfg!(windows))
}

/// [`risky`] for a machine that runs Windows (`windows`) or not.
pub fn risky_on(
    action: &NodeAction,
    startup: impl Fn(&str) -> bool,
    windows: bool,
) -> Option<String> {
    match action {
        NodeAction::Run { command, .. } => {
            match crate::tools::policy::assess_command_for(command, windows) {
                crate::tools::Risk::Ask { reason, .. } => Some(reason),
                _ => None,
            }
        }
        NodeAction::Write { path, .. } | NodeAction::Edit { path, .. } => {
            startup(path).then(|| {
                "It changes a file that runs programs when you log in or open a terminal.".into()
            })
        }
        // Any mouse or keyboard input can do something: a click, a drag or
        // keys anything a person at that screen could, a scroll can change a
        // value under the pointer, a move can open what reacts to hovering.
        NodeAction::Desktop { desktop, look } if desktop.acts() => Some(if *look {
            "It uses the mouse or keyboard on that computer's screen as the person signed in there, then shows the agent what is on that computer's screen.".into()
        } else {
            "It uses the mouse or keyboard on that computer's screen as the person signed in there."
                .into()
        }),
        // What is on the screen may be private: a screenshot, or one taken
        // after acting (`look`).
        NodeAction::Desktop {
            desktop: DesktopOp::Screenshot { .. },
            ..
        }
        | NodeAction::Desktop { look: true, .. } => {
            Some("It shows the agent what is on that computer's screen.".into())
        }
        _ => None,
    }
}

/// How `new` gives agents more than `old`, one phrase per change (empty:
/// it only narrows or keeps things).
pub fn widening(old: &MachineScope, new: &MachineScope) -> Vec<String> {
    let mut out = Vec::new();
    for f in &new.folders {
        match old.folders.iter().find(|o| o.path == f.path) {
            None => out.push(format!(
                "allows {} ({})",
                f.path,
                if f.write { "read-write" } else { "read-only" }
            )),
            Some(o) if f.write && !o.write => out.push(format!("makes {} read-write", f.path)),
            _ => {}
        }
    }
    if new.commands && !old.commands {
        out.push("turns commands on".into());
    }
    if ask_rank(new.ask) > ask_rank(old.ask) {
        out.push(format!(
            "asks before {}",
            match new.ask {
                MachineAsk::Risky => "risky actions only".to_string(),
                _ => "nothing itself (each chat's own mode decides)".to_string(),
            }
        ));
    }
    if new.timeout_secs > old.timeout_secs {
        out.push(format!(
            "raises the time limit per action to {}",
            human_secs(new.timeout_secs)
        ));
    }
    if new.webviews && !old.webviews {
        out.push(
            "lets chats open web views here (they can reach this computer's local network and localhost services)"
                .into(),
        );
    }
    if new.desktop && !old.desktop {
        out.push(
            "turns on desktop control (startup files can change; commands get your desktop session)"
                .into(),
        );
    }
    match (old.terminal, new.terminal) {
        (TerminalAccess::Off, TerminalAccess::Sandboxed) => out.push(
            "lets your paired devices open a terminal here, in the sandbox (Tessera's own files stay out of reach, and your startup files can't be changed); this trusts your Tessera server with this account"
                .into(),
        ),
        (TerminalAccess::Off | TerminalAccess::Sandboxed, TerminalAccess::Full) => out.push(
            "lets your paired devices open a terminal here with your account's full rights; this trusts your Tessera server with everything this account can do".into(),
        ),
        _ => {}
    }
    match (old.admin, new.admin) {
        (AdminAccess::Off, AdminAccess::Ask) => out.push(
            "lets agents run commands as this computer's administrator (root, with sudo), outside the sandbox; each one asks you first, showing the exact command"
                .into(),
        ),
        (AdminAccess::Off | AdminAccess::Ask, AdminAccess::Chat) => out.push(
            "lets agents run commands as this computer's administrator (root, with sudo), outside the sandbox, without asking you in chats that don't ask (YOLO)"
                .into(),
        ),
        _ => {}
    }
    match (&old.bots, &new.bots) {
        (Some(_), None) => out.push("lets any chat use it".into()),
        (Some(o), Some(n)) => {
            let added: Vec<&str> = n
                .iter()
                .filter(|b| !o.iter().any(|x| x.eq_ignore_ascii_case(b)))
                .map(String::as_str)
                .collect();
            if !added.is_empty() {
                out.push(format!("lets {} use it", added.join(", ")));
            }
        }
        _ => {}
    }
    out
}

pub fn human_secs(s: u64) -> String {
    if s >= 3600 && s.is_multiple_of(3600) {
        format!("{} h", s / 3600)
    } else if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

/// "Any chat", or the bots by name (`name_of` turns an id or handle into a
/// name when it knows one).
pub fn who_can_use(scope: &MachineScope, name_of: impl Fn(&str) -> Option<String>) -> String {
    match &scope.bots {
        None => "any chat".into(),
        Some(b) if b.is_empty() => "nobody (no bot chosen yet)".into(),
        Some(b) => format!(
            "only {}",
            b.iter()
                .map(|x| name_of(x).unwrap_or_else(|| x.clone()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The scope in a few lines, for the terminal.
pub fn describe_scope(scope: &MachineScope, home: Option<&Path>) -> String {
    let preset = preset_of(scope, home);
    let tilde = |p: &str| match home.and_then(|h| Path::new(p).strip_prefix(h).ok()) {
        Some(r) if r.as_os_str().is_empty() => "~".to_string(),
        Some(r) => format!("~/{}", r.display()),
        None => p.to_string(),
    };
    let mut out = format!(
        "  allows:   {} · {}\n",
        preset_name(preset),
        preset_sentence(preset)
    );
    if scope.folders.is_empty() {
        out.push_str("  folders:  none yet (agents can't use any file here)\n");
    } else {
        out.push_str("  folders:\n");
        for f in &scope.folders {
            out.push_str(&format!(
                "    {}  {}\n",
                if f.write { "read-write" } else { "read-only " },
                tilde(&f.path)
            ));
        }
    }
    out.push_str(&format!(
        "  commands: {}\n",
        if scope.commands {
            "on (in an allowed folder; they change files only in read-write ones)"
        } else {
            "off"
        }
    ));
    if scope.desktop {
        out.push_str(if cfg!(windows) {
            "  desktop:  on (agents can see your screens and use your mouse and keyboard while you're signed in)\n"
        } else {
            "  desktop:  on (startup files can change, backed up first; commands get your desktop session)\n"
        });
    }
    if scope.webviews {
        out.push_str(
            "  web views: on (pages opened here can reach this computer's local network and localhost services)\n",
        );
    }
    match scope.terminal {
        TerminalAccess::Off => {}
        TerminalAccess::Sandboxed => out.push_str(
            "  terminal: on for your paired devices (sandboxed: Tessera's own files out of reach)\n",
        ),
        TerminalAccess::Full => out.push_str(
            "  terminal: on for your paired devices (your account's full rights)\n",
        ),
    }
    match scope.admin {
        AdminAccess::Off => {}
        AdminAccess::Ask => out.push_str(
            "  admin:    on (commands as administrator, outside the sandbox; each one asks first)\n",
        ),
        AdminAccess::Chat => out.push_str(
            "  admin:    on (commands as administrator, outside the sandbox; asking as each chat's mode says)\n",
        ),
    }
    out.push_str(&format!("  ask first: {}\n", ask_name(scope.ask)));
    out.push_str(&format!("  who:      {}\n", who_can_use(scope, |_| None)));
    out.push_str(&format!(
        "  limit:    {} per action",
        human_secs(scope.timeout_secs)
    ));
    out
}

/// The most specific folder rule holding `path` (all canonical).
pub(crate) fn rule_for<'a>(
    folders: &'a [(PathBuf, bool)],
    path: &Path,
) -> Option<&'a (PathBuf, bool)> {
    folders
        .iter()
        .filter(|(f, _)| path.starts_with(f))
        .max_by_key(|(f, _)| f.components().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_capabilities_follow_built_support_not_server_os() {
        assert!(
            desktop_built(true, false),
            "old Linux command desktop remains supported"
        );
        assert!(
            desktop_built(false, true),
            "Windows native desktop is supported"
        );
        assert!(
            !desktop_built(false, false),
            "an unsupported node stays refused"
        );
        let advertised = features();
        for name in ["terminal", "webviews"] {
            assert_eq!(
                advertised.iter().any(|s| s == name),
                cfg!(any(unix, windows))
            );
        }
        assert_eq!(advertised.iter().any(|s| s == "admin"), cfg!(unix));
        assert_eq!(
            advertised.iter().any(|s| s == "sandbox"),
            crate::sandbox::confines_writes()
        );
        assert_eq!(advertised.iter().any(|s| s == "desktop"), screen::AVAILABLE);
        assert_eq!(
            advertised.iter().any(|s| s == "desktop_session"),
            cfg!(target_os = "linux")
        );
        assert_eq!(
            screen::AVAILABLE || cfg!(target_os = "linux"),
            desktop_built(cfg!(target_os = "linux"), screen::AVAILABLE)
        );
    }

    fn rig() -> (tempfile::TempDir, Paths, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::at(root.join("tessera-home"));
        paths.ensure().unwrap();
        let home = root.join("me");
        for d in ["code", "docs", ".config/hypr"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        (dir, paths, home)
    }

    #[test]
    fn folders_are_checked_and_only_the_home_folder_itself_is_read_write() {
        let (dir, paths, home) = rig();
        let h = Some(home.as_path());
        let work = home.join("code");
        let ok = check_folder(&paths, work.to_str().unwrap(), true, h).unwrap();
        assert!(ok.write && ok.path == work.display().to_string());
        assert_eq!(
            check_folder(&paths, "~/code", false, h).unwrap().path,
            work.display().to_string()
        );
        assert!(check_folder(&paths, "relative/path", false, h).is_err());
        assert!(check_folder(&paths, "/", false, h).is_err());
        assert!(
            check_folder(
                &paths,
                &dir.path().join("nope").display().to_string(),
                false,
                h
            )
            .is_err()
        );
        let inside = paths.config_dir.join("skills");
        std::fs::create_dir_all(&inside).unwrap();
        let err = check_folder(&paths, inside.to_str().unwrap(), false, h).unwrap_err();
        assert!(err.contains("Tessera's own"), "{err}");
        // The home folder itself may be read-write (Full access); what holds
        // it never.
        assert!(check_folder(&paths, "~", true, h).is_ok());
        assert!(check_folder(&paths, home.parent().unwrap().to_str().unwrap(), true, h).is_err());
        assert!(check_folder(&paths, home.parent().unwrap().to_str().unwrap(), false, h).is_ok());
    }

    #[test]
    fn a_mac_gets_chosen_folders_not_the_whole_home_or_the_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let (project, library) = (home.join("code"), home.join("Library/LaunchAgents"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        let paths = Paths::at(home.join("tessera-node"));
        let folder = |p: &Path, write: bool| FolderAccess {
            path: p.display().to_string(),
            write,
        };
        let scope = |folders: Vec<FolderAccess>, desktop: bool| MachineScope {
            folders,
            commands: true,
            desktop,
            ..MachineScope::default()
        };
        let on_mac = |s: &MachineScope| check_scope_on(&paths, s, Some(&home), true);
        // Full access and desktop control: not on a Mac.
        let e = on_mac(&scope(vec![folder(&home, true)], false)).unwrap_err();
        assert!(e.contains("can only be read-only"), "{e}");
        assert!(on_mac(&scope(vec![folder(&library, true)], false)).is_err());
        let e = on_mac(&scope(vec![folder(&project, true)], true)).unwrap_err();
        assert!(e.contains("desktop control"), "{e}");
        // Chosen folders, and the home folder to read, are fine.
        assert!(
            on_mac(&scope(
                vec![folder(&project, true), folder(&home, false)],
                false
            ))
            .is_ok()
        );
        // Elsewhere nothing changes.
        assert!(
            check_scope_on(
                &paths,
                &scope(vec![folder(&home, true)], true),
                Some(&home),
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn work_without_a_sandbox_is_checked_folder_writes_only() {
        let (_dir, paths, home) = rig();
        let h = Some(home.as_path());
        for os in ["linux", "macos", "windows"] {
            let mut config = NodeConfig::default();
            let work = apply_preset_with_sandbox(
                &paths,
                &config,
                MachinePreset::WorkInFolders,
                Some(vec!["~/code".into()]),
                h,
                false,
            )
            .unwrap();
            assert!(
                !work.commands && !work.desktop && !work.webviews,
                "{os}: {work:?}"
            );
            assert_eq!(work.admin, AdminAccess::Off);
            assert_eq!(
                work.folders,
                vec![FolderAccess {
                    path: home.join("code").display().to_string(),
                    write: true
                }]
            );
            assert_eq!(preset_of(&work, h), MachinePreset::WorkInFolders);
            assert!(check_scope_on(&paths, &work, h, os == "macos").is_ok());
            config.set_scope(work.clone(), h);
            assert_eq!(config.chosen, vec![home.join("code").display().to_string()]);
            let look =
                apply_preset_with_sandbox(&paths, &config, MachinePreset::LookOnly, None, h, false)
                    .unwrap();
            assert!(!look.folders[0].write && !look.commands);
            config.set_scope(look, h);
            assert_eq!(
                apply_preset_with_sandbox(
                    &paths,
                    &config,
                    MachinePreset::WorkInFolders,
                    None,
                    h,
                    false
                )
                .unwrap(),
                work
            );
            assert!(
                preset_sentence_with_sandbox(MachinePreset::WorkInFolders, os, false)
                    .contains("Commands aren't available")
            );
        }
        let work = apply_preset_with_sandbox(
            &paths,
            &NodeConfig::default(),
            MachinePreset::WorkInFolders,
            Some(vec!["~/code".into()]),
            h,
            true,
        )
        .unwrap();
        assert!(work.commands && work.folders[0].write);
        assert!(
            preset_sentence_with_sandbox(MachinePreset::WorkInFolders, "linux", true)
                .contains("run commands there")
        );
    }

    #[test]
    fn presets_map_to_scopes_and_back() {
        let (_dir, paths, home) = rig();
        let h = Some(home.as_path());
        let code = home.join("code").display().to_string();
        let mut config = NodeConfig::default();
        assert_eq!(preset_of(&config.scope, h), MachinePreset::LookOnly);

        let look = apply_preset(
            &paths,
            &config,
            MachinePreset::LookOnly,
            Some(vec!["~/code".into()]),
            h,
        )
        .unwrap();
        assert_eq!(
            look.folders,
            vec![FolderAccess {
                path: code.clone(),
                write: false
            }]
        );
        assert!(!look.commands && !look.desktop);
        assert_eq!(
            look.ask,
            MachineAsk::Always,
            "switching never loosens asking"
        );
        assert_eq!(preset_of(&look, h), MachinePreset::LookOnly);
        config.set_scope(look, h);
        assert_eq!(config.chosen, vec![code.clone()]);

        let work = apply_preset(&paths, &config, MachinePreset::WorkInFolders, None, h).unwrap();
        assert_eq!(
            work.folders,
            vec![FolderAccess {
                path: code.clone(),
                write: true
            }]
        );
        assert_eq!(work.commands, crate::sandbox::confines_writes());
        assert!(!work.desktop);
        assert_eq!(preset_of(&work, h), MachinePreset::WorkInFolders);
        config.set_scope(work.clone(), h);

        assert!(
            !work.webviews,
            "web views are an extra with Work in folders"
        );
        let full = apply_preset(&paths, &config, MachinePreset::FullAccess, None, h).unwrap();
        assert_eq!(
            full.folders,
            vec![FolderAccess {
                path: home.display().to_string(),
                write: true
            }]
        );
        assert!(
            full.commands && !full.desktop && full.webviews,
            "Full access includes web views"
        );
        assert_eq!(preset_of(&full, h), MachinePreset::FullAccess);
        config.set_scope(full, h);
        assert_eq!(
            config.chosen,
            vec![code.clone()],
            "Full access keeps the chosen folders aside"
        );

        let desk = apply_preset(&paths, &config, MachinePreset::DesktopControl, None, h).unwrap();
        assert!(desk.desktop && desk.commands);
        assert_eq!(
            desk.ask,
            MachineAsk::Always,
            "desktop control asks before everything"
        );
        assert_eq!(preset_of(&desk, h), MachinePreset::DesktopControl);
        config.set_scope(desk, h);

        // Back to Work in folders: the folders chosen earlier come back
        // (and web views go, as they came with Full access).
        let back = apply_preset(&paths, &config, MachinePreset::WorkInFolders, None, h).unwrap();
        assert_eq!(back, work);
        // Turned on with Work in folders, they stay while it stays.
        let mut cw = config.clone();
        let mut ww = work.clone();
        ww.webviews = true;
        assert_eq!(preset_of(&ww, h), MachinePreset::WorkInFolders);
        cw.set_scope(ww, h);
        assert!(
            apply_preset(&paths, &cw, MachinePreset::WorkInFolders, None, h)
                .unwrap()
                .webviews
        );
        assert!(
            !apply_preset(&paths, &cw, MachinePreset::LookOnly, None, h)
                .unwrap()
                .webviews
        );
        let mut lw = apply_preset(&paths, &cw, MachinePreset::LookOnly, None, h).unwrap();
        lw.webviews = true;
        assert_eq!(
            preset_of(&lw, h),
            MachinePreset::Custom,
            "Look only never has web views"
        );
        assert!(apply_preset(&paths, &config, MachinePreset::Custom, None, h).is_err());

        // A mix is Advanced, and so is desktop control without the home folder.
        let mut mixed = work.clone();
        mixed.folders.push(FolderAccess {
            path: home.join("docs").display().to_string(),
            write: false,
        });
        assert_eq!(preset_of(&mixed, h), MachinePreset::Custom);
        let mut odd = work.clone();
        odd.desktop = true;
        assert_eq!(preset_of(&odd, h), MachinePreset::Custom);

        // Asking: a looser current choice is kept only up to the preset's default.
        let mut loose = config.clone();
        loose.scope.ask = MachineAsk::Chat;
        assert_eq!(
            apply_preset(&paths, &loose, MachinePreset::LookOnly, None, h)
                .unwrap()
                .ask,
            MachineAsk::Risky
        );
        assert_eq!(
            apply_preset(&paths, &loose, MachinePreset::DesktopControl, None, h)
                .unwrap()
                .ask,
            MachineAsk::Always
        );
    }

    #[test]
    fn who_can_use_matches_ids_and_handles() {
        let mut s = MachineScope::default();
        assert!(may_use(&s, None).is_ok() && may_use(&s, Some(("b1", "sweeper"))).is_ok());
        s.bots = Some(vec!["@Sweeper".into()]);
        assert!(may_use(&s, Some(("b1", "sweeper"))).is_ok());
        assert!(may_use(&s, Some(("b2", "writer"))).is_err());
        assert!(may_use(&s, None).unwrap_err().contains("plain chat"));
        s.bots = Some(vec!["b2".into()]);
        assert!(may_use(&s, Some(("b2", "writer"))).is_ok());
        s.bots = Some(vec![]);
        assert!(may_use(&s, Some(("b2", "writer"))).is_err());
    }

    #[test]
    fn widening_is_spelled_out_and_narrowing_is_not() {
        let old = MachineScope::default();
        let mut new = old.clone();
        assert!(widening(&old, &new).is_empty());
        new.folders.push(FolderAccess {
            path: "/w".into(),
            write: false,
        });
        new.commands = true;
        new.ask = MachineAsk::Chat;
        new.timeout_secs = 600;
        let w = widening(&old, &new);
        assert_eq!(w.len(), 4, "{w:?}");
        let mut rw = new.clone();
        rw.folders[0].write = true;
        assert_eq!(widening(&new, &rw), vec!["makes /w read-write".to_string()]);
        assert!(
            widening(&rw, &old).is_empty(),
            "taking things away never asks"
        );
        let mut desk = rw.clone();
        desk.desktop = true;
        assert!(widening(&rw, &desk)[0].contains("desktop control"));
        assert!(widening(&desk, &rw).is_empty());
        let mut only = rw.clone();
        only.bots = Some(vec!["sweeper".into()]);
        assert!(widening(&rw, &only).is_empty(), "limiting to bots narrows");
        assert_eq!(
            widening(&only, &rw),
            vec!["lets any chat use it".to_string()]
        );
        let mut more = only.clone();
        more.bots = Some(vec!["sweeper".into(), "writer".into()]);
        assert_eq!(
            widening(&only, &more),
            vec!["lets writer use it".to_string()]
        );
        let mut wv = rw.clone();
        wv.webviews = true;
        assert!(widening(&rw, &wv)[0].contains("web views"));
        assert!(widening(&wv, &rw).is_empty());
        let mut risky = old.clone();
        risky.ask = MachineAsk::Risky;
        assert_eq!(widening(&old, &risky).len(), 1);
        assert!(widening(&risky, &old).is_empty());
    }

    #[test]
    fn startup_files_are_recognised_and_risky_to_change() {
        let home = Path::new("/home/me");
        let h = Some(home);
        assert!(may_be_startup("~/.bashrc", h));
        assert!(may_be_startup("/home/me/.config/hypr/hyprland.conf", h));
        assert!(!may_be_startup("/home/me/.config/hyprx", h));
        assert!(!may_be_startup("~/code/.bashrc", h));
        assert!(
            may_be_startup("~/code/../.bashrc", h),
            "climbing out counts, to be safe"
        );
        let startup = |p: &str| may_be_startup(p, h);
        let write = |p: &str| NodeAction::Write {
            path: p.into(),
            content: String::new(),
        };
        assert!(risky(&write("~/.config/hypr/hyprland.conf"), startup).is_some());
        assert!(risky(&write("~/code/main.rs"), startup).is_none());
        let run = |c: &str| NodeAction::Run {
            command: c.into(),
            cwd: "/w".into(),
            timeout_secs: None,
            admin: false,
        };
        assert!(risky(&run("git push origin main"), startup).is_some());
        assert!(risky(&run("rm -rf build"), startup).is_none());
    }

    #[test]
    fn the_default_is_the_tightest() {
        let c = NodeConfig::default();
        assert!(!c.enabled);
        assert!(c.scope.folders.is_empty() && !c.scope.commands && !c.scope.desktop);
        assert!(c.scope.bots.is_none());
        assert_eq!(c.scope.ask, MachineAsk::Always);
        assert_eq!(c.scope.timeout_secs, tessera_proto::MACHINE_TIMEOUT_SECS);
        let parsed: NodeConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.scope, MachineScope::default());
        assert!(!parsed.enabled);
        // An older machine's settings read the same.
        let old: MachineScope = serde_json::from_str(
            r#"{"folders":[],"commands":true,"ask":"chat","timeout_secs":60}"#,
        )
        .unwrap();
        assert!(!old.desktop && old.bots.is_none() && old.ask == MachineAsk::Chat);
        // …and an unchanged scope is written as before (older servers read it).
        let text = serde_json::to_string(&MachineScope::default()).unwrap();
        assert!(
            !text.contains("desktop") && !text.contains("bots") && !text.contains("webviews"),
            "{text}"
        );
    }

    #[test]
    fn the_users_stop_holds_through_any_new_scope_and_refuses_everything() {
        let mut c = NodeConfig::default();
        c.scope.stopped = true;
        c.scope.stops = 3;
        // A preset, `scope --set`, the app's switches: none of them resume it.
        c.set_scope(
            MachineScope {
                commands: true,
                desktop: true,
                ..Default::default()
            },
            None,
        );
        assert!(c.scope.stopped && c.scope.stops == 3 && c.scope.commands);
        let paths = Paths::at(std::path::Path::new("/nonexistent-tessera-test"));
        for action in [
            NodeAction::Read {
                path: "/tmp/x".into(),
                offset: None,
                limit: None,
            },
            NodeAction::Desktop {
                desktop: DesktopOp::Screens,
                look: false,
            },
        ] {
            let why = exec::permitted(&paths, None, &c.scope, &action, None).unwrap_err();
            assert!(why.contains("stopped Tessera on this computer"), "{why}");
        }
        // What the Stop bar shows never includes typed text.
        let typing = NodeAction::Desktop {
            desktop: DesktopOp::Type {
                text: "my secret words".into(),
            },
            look: false,
        };
        assert!(!doing(&typing).contains("secret"), "{}", doing(&typing));
        // Nor a command's arguments.
        let run = NodeAction::Run {
            command: "curl -u admin:testpw2 https://x".into(),
            cwd: "~".into(),
            timeout_secs: None,
            admin: false,
        };
        assert_eq!(doing(&run), "a command");
    }

    #[test]
    fn a_stop_kept_by_the_previous_build_survives_the_update() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        paths.ensure().unwrap();
        // The previous build kept the Stop in node.json only.
        std::fs::write(
            config_file(&paths),
            r#"{"enabled": true, "name": "PC", "scope": {"stopped": true, "stops": 4}}"#,
        )
        .unwrap();
        let c = NodeConfig::load(&paths).unwrap();
        assert!(c.scope.stopped && c.scope.stops == 4);
        let l = StopLatch::load(&paths);
        assert!(l.stopped && l.stops == 4, "{l:?}");
        // Only a Resume undoes it, and the count stays.
        let l = StopLatch::set(&paths, false, 0).unwrap();
        assert!(!l.stopped && l.stops == 4);
        assert!(!NodeConfig::load(&paths).unwrap().scope.stopped);
        // A Stop never counts below what a machine already counted.
        let l = StopLatch::set(&paths, true, 9).unwrap();
        assert_eq!(l.stops, 10);
    }

    #[test]
    fn an_old_stop_holds_even_when_it_cant_be_moved_yet() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        paths.ensure().unwrap();
        std::fs::write(
            config_file(&paths),
            r#"{"enabled": true, "name": "PC", "scope": {"stopped": true, "stops": 2}}"#,
        )
        .unwrap();
        // Something else holds the Stop's lock: it can't be moved now.
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.config_dir.join("stop.lock"))
            .unwrap();
        assert!(try_lock_file(&held));
        let c = NodeConfig::load(&paths).unwrap();
        assert!(c.scope.stopped && c.scope.stops == 2);
        assert!(!stop_file(&paths).exists());
        // A settings change meanwhile keeps it too.
        let mut changed = c.clone();
        changed.scope.stopped = false;
        changed.save(&paths).unwrap();
        assert!(NodeConfig::load(&paths).unwrap().scope.stopped);
        drop(held);
        assert!(NodeConfig::load(&paths).unwrap().scope.stopped);
        assert!(StopLatch::load(&paths).stopped);
    }

    #[test]
    fn a_stop_is_kept_even_while_its_lock_is_held_and_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        paths.ensure().unwrap();
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.config_dir.join("stop.lock"))
            .unwrap();
        assert!(try_lock_file(&held));
        // Something else holds the lock: the Stop is kept beside it anyway.
        let l = StopLatch::set(&paths, true, 0).unwrap();
        assert!(l.stopped && l.stops == 1);
        assert!(!stop_file(&paths).exists());
        // Read afresh (as after a restart): still stopped.
        let again = StopLatch::load(&paths);
        assert!(again.stopped && again.stops == 1, "{again:?}");
        // A Resume needs the lock (one at a time) and is above that Stop.
        assert!(StopLatch::set(&paths, false, 0).is_err());
        drop(held);
        let r = StopLatch::set(&paths, false, 0).unwrap();
        assert!(!r.stopped && r.stops == 1);
        assert!(!StopLatch::load(&paths).stopped);
        // The next Stop counts on from there.
        let l = StopLatch::set(&paths, true, 0).unwrap();
        assert!(l.stopped && l.stops == 2);
        assert!(StopLatch::load(&paths).stopped);
    }

    #[test]
    fn a_resume_never_covers_a_stop_it_didnt_see() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        paths.ensure().unwrap();
        assert_eq!(StopLatch::set(&paths, true, 0).unwrap().stops, 1);
        // Stopped again while stopped (another device, the command line):
        // counted anew.
        assert_eq!(StopLatch::set(&paths, true, 0).unwrap().stops, 2);
        // A Resume decided on what it read before that second Stop (count 1)
        // lands after it: the second Stop holds.
        let stale = StopLatch {
            stopped: false,
            stops: 1,
            at: crate::util::now_ms() + 10,
        };
        std::fs::write(stop_file(&paths), serde_json::to_string(&stale).unwrap()).unwrap();
        let l = StopLatch::load(&paths);
        assert!(l.stopped && l.stops == 2, "{l:?}");
        // A Resume that saw it resumes.
        assert!(!StopLatch::set(&paths, false, 0).unwrap().stopped);
        assert!(!StopLatch::load(&paths).stopped);
    }

    #[test]
    fn a_stop_kept_without_the_lock_holds_until_a_resume_that_saw_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        paths.ensure().unwrap();
        let l = StopLatch {
            stopped: true,
            stops: 1,
            at: 1,
        };
        assert!(keep_own_stop(&paths, &l));
        assert!(StopLatch::load(&paths).stopped);
        assert!(!StopLatch::set(&paths, false, 0).unwrap().stopped);
        assert!(!StopLatch::load(&paths).stopped, "the Resume saw it");
        // Two such Stops at the same count each get their own file: one
        // made after a Resume looked is still there afterwards.
        assert!(keep_own_stop(&paths, &l));
        assert!(keep_own_stop(&paths, &l));
        assert_eq!(unacknowledged(&paths).unwrap().len(), 2);
        // A Resume removes only what it read.
        let seen = unacknowledged(&paths).unwrap();
        std::fs::remove_file(&seen[0].0).unwrap();
        assert!(StopLatch::load(&paths).stopped, "the other one still stops");
    }

    #[test]
    fn a_damaged_stop_record_stops_until_a_resume_mends_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        paths.ensure().unwrap();
        std::fs::write(pending_file(&paths), "{damaged").unwrap();
        assert!(StopLatch::load(&paths).stopped);
        let r = StopLatch::set(&paths, false, 0).unwrap();
        assert!(!r.stopped);
        assert!(!StopLatch::load(&paths).stopped, "mended by the Resume");
    }
}
