//! Running one action on this machine, within its scope.
//!
//! Paths: every action resolves its path here (`~` is this computer's
//! home), checks it against the folder rules, Tessera's own directories,
//! other programs' credential stores and startup files, then opens it and
//! checks again what was actually opened (`/proc/self/fd`), so a symlink
//! swapped in after the first check can't redirect it. Writes go through a
//! handle on the checked parent folder, never follow a final symlink and
//! refuse files hard-linked elsewhere.
//!
//! Commands: only with commands on, in an allowed folder, in the sandbox
//! with Tessera's data and credential stores hidden, files changeable only
//! in read-write folders (Landlock) and a private temporary folder, never
//! startup files (unless desktop control is on) or the Tessera program, and
//! an environment without credentials. With desktop control they also get
//! the graphical session ([`super::desktop`]). Stopping one kills its whole
//! process group and what it started.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tessera_proto::{MachineScope, NodeAction, NodeResult};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::config::Paths;
use crate::tools::Protected;

/// Largest file read (the view shown is smaller still).
const MAX_READ_BYTES: u64 = 8 << 20;
/// Largest file written or edited.
const MAX_WRITE_BYTES: usize = 16 << 20;
/// Command output kept (the end, where the errors are, wins).
const MAX_OUTPUT: usize = 1 << 20;
/// Entries shown when listing a folder.
const MAX_ENTRIES: usize = 500;

/// Live output of a running command.
pub type Progress = Arc<dyn Fn(&str, &'static str) + Send + Sync>;

pub fn refused(why: impl Into<String>) -> NodeResult {
    NodeResult {
        output: why.into(),
        ok: false,
        refused: true,
        exit_code: None,
        image: None,
    }
}

fn failed(why: impl Into<String>) -> NodeResult {
    NodeResult {
        output: why.into(),
        ok: false,
        refused: false,
        exit_code: None,
        image: None,
    }
}

fn done(text: impl Into<String>) -> NodeResult {
    NodeResult {
        output: text.into(),
        ok: true,
        refused: false,
        exit_code: None,
        image: None,
    }
}

/// Why an action can't touch a path.
#[derive(Debug)]
pub enum Denied {
    /// Outside the scope: nothing ran.
    Refused(String),
    /// Allowed, but it failed (missing file…).
    Failed(String),
}

impl From<Denied> for NodeResult {
    fn from(d: Denied) -> Self {
        match d {
            Denied::Refused(m) => refused(m),
            Denied::Failed(m) => failed(m),
        }
    }
}

/// What actions here may touch, from the scope (all paths canonical).
pub struct Access {
    folders: Vec<(PathBuf, bool)>,
    protected: Protected,
    credentials: Vec<PathBuf>,
    /// Startup files and folders, as named and as resolved (a dotfiles
    /// manager may link them elsewhere).
    startup: Vec<PathBuf>,
    program: Vec<PathBuf>,
    /// Desktop control: startup files in read-write folders may change.
    desktop: bool,
    home: Option<PathBuf>,
    /// A Mac: nothing in the whole home folder changes through a rule for
    /// all of it, nor anything in `~/Library` (see `node::mac_limits`).
    mac: bool,
}

const PROTECTED: &str =
    "that is Tessera's own data or settings on this machine, which agents never get";

/// Both the path and where it leads.
fn both(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = paths
        .iter()
        .flat_map(|p| [p.clone(), crate::tools::canonical(p)])
        .collect();
    v.sort();
    v.dedup();
    v
}

impl Access {
    pub fn new(paths: &Paths, scope: &MachineScope, home: Option<&Path>) -> Self {
        Self {
            folders: scope
                .folders
                .iter()
                .map(|f| {
                    (
                        crate::tools::canonical(&super::expand(&f.path, home)),
                        f.write,
                    )
                })
                .collect(),
            protected: super::protected(paths).canonical(),
            credentials: both(super::credential_dirs(home)),
            startup: both(super::startup_paths(home)),
            program: super::program_paths(),
            // On Windows desktop control is the screen, mouse and keyboard:
            // startup locations stay out of reach of file actions there.
            desktop: scope.desktop && cfg!(unix),
            home: home.map(Path::to_path_buf),
            mac: cfg!(target_os = "macos"),
        }
    }

    /// `raw` (as an action names it) is, once resolved, a startup file.
    pub fn changes_startup(&self, raw: &str) -> bool {
        let p = super::expand(raw.trim(), self.home.as_deref());
        if !p.is_absolute() {
            return false;
        }
        let canon = crate::tools::canonical(&p);
        self.startup
            .iter()
            .any(|s| canon.starts_with(s) || p.starts_with(s))
    }

    /// The startup files and folders (for backups).
    pub fn startup(&self) -> &[PathBuf] {
        &self.startup
    }

    fn allowed_list(&self) -> String {
        if self.folders.is_empty() {
            return "none yet".into();
        }
        self.folders
            .iter()
            .map(|(p, w)| {
                format!(
                    "{} ({})",
                    p.display(),
                    if *w { "read-write" } else { "read-only" }
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// May `p` (canonical) be read, or changed?
    pub fn check(&self, p: &Path, write: bool) -> Result<(), String> {
        if self.protected.covers(p) {
            return Err(PROTECTED.into());
        }
        let Some((folder, rw)) = super::rule_for(&self.folders, p) else {
            return Err(format!(
                "{} is outside the folders this machine allows ({})",
                p.display(),
                self.allowed_list()
            ));
        };
        if write && !rw {
            return Err(format!(
                "{} is in a read-only folder on this machine ({})",
                p.display(),
                folder.display()
            ));
        }
        // A Mac set up before Full access was held back there (its disks
        // ignore letter case, hence the lower case).
        if write && self.mac {
            let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
            if let Some(h) = self.home.as_deref().map(lower)
                && (h.starts_with(lower(folder)) || lower(p).starts_with(h.join("library")))
            {
                return Err(
                    "on a Mac, agents change only folders chosen for them, not anything through the whole home folder or in ~/Library: on that computer, choose Work in folders (`tessera node preset work`) and the folders they need".into()
                );
            }
        }
        if let Some(c) = self.credentials.iter().find(|c| p.starts_with(c))
            && !folder.starts_with(c)
        {
            return Err(format!(
                "{} holds another program's credentials; this machine keeps them out of reach unless that folder itself is allowed",
                c.display()
            ));
        }
        // A credentials file only where its own folder (not the home
        // folder or above) was allowed: ~/.netrc never.
        let own_folder = p.parent().is_some_and(|d| {
            folder.starts_with(d) && !self.home.as_ref().is_some_and(|h| h.starts_with(d))
        });
        if crate::tools::credential_file(p) && !own_folder {
            return Err(format!(
                "{} is another program's credentials file; this machine never hands it out",
                p.display()
            ));
        }
        if write && self.program.iter().any(|s| p.starts_with(s)) {
            return Err(format!(
                "{} is the Tessera program on this machine; machines never change it",
                p.display()
            ));
        }
        if write && !self.desktop && self.startup.iter().any(|s| p.starts_with(s)) {
            return Err(format!(
                "{} runs programs when you log in or open a terminal; machines change it only with desktop control, which is turned on at this computer",
                p.display()
            ));
        }
        Ok(())
    }

    /// `raw` (absolute or `~/…`) resolved and checked.
    pub fn resolve(&self, raw: &str, write: bool) -> Result<PathBuf, String> {
        let raw = raw.trim();
        if raw.is_empty() || raw.contains('\0') {
            return Err("give a path on this machine".into());
        }
        let path = super::expand(raw, self.home.as_deref());
        if !path.is_absolute() {
            return Err(format!(
                "`{raw}` isn't a full path on this machine (start it with / or ~/)"
            ));
        }
        if self.protected.covers(&path) {
            return Err(PROTECTED.into());
        }
        let canon = crate::tools::canonical(&path);
        self.check(&canon, write)?;
        Ok(canon)
    }

    /// Where commands may change files: the read-write folders, opened up
    /// around what must stay as it is (startup files unless desktop
    /// control is on, and the Tessera program). A folder holding one of
    /// those isn't writable itself; each of its entries is, except that
    /// one (and links, which could lead anywhere). So nothing new can
    /// appear beside a startup file either, such as a `.bash_login` that
    /// wasn't there.
    fn writable(&self) -> Vec<PathBuf> {
        let mut keep: Vec<PathBuf> = self.program.clone();
        if !self.desktop {
            keep.extend(self.startup.iter().cloned());
        }
        let mut out = Vec::new();
        let mut budget = 20_000usize;
        for (f, _) in self.folders.iter().filter(|(_, w)| *w) {
            open_up(f, &keep, &mut out, &mut budget, 0);
        }
        out
    }
}

/// `dir` writable as a whole, or (holding something in `keep`) entry by
/// entry; `budget` bounds the entries looked at (past it, less is writable).
fn open_up(dir: &Path, keep: &[PathBuf], out: &mut Vec<PathBuf>, budget: &mut usize, depth: u32) {
    if keep.iter().any(|k| dir.starts_with(k)) {
        return;
    }
    if !keep.iter().any(|k| k.starts_with(dir)) {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    if depth > 8 {
        return;
    }
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_symlink() {
            continue;
        }
        if t.is_dir() {
            open_up(&p, keep, out, budget, depth + 1);
        } else if !keep.iter().any(|k| p.starts_with(k)) {
            out.push(p);
        }
    }
}

/// The desktop session's variables to use instead of the running one's.
pub type SessionEnv = Option<Arc<std::collections::HashMap<String, String>>>;

/// Run `action` (`home`: this computer's home folder); `timeout` bounds it,
/// `cancel` stops it.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    paths: &Paths,
    home: Option<&Path>,
    scope: &MachineScope,
    action: NodeAction,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Option<Progress>,
    session_env: SessionEnv,
    secrets: &tessera_proto::SecretEnv,
) -> Outcome {
    let access = Arc::new(Access::new(paths, scope, home));
    // Stopped before it began (the user's Stop, the chat): nothing starts.
    if cancel.is_cancelled() {
        return Outcome::Cancelled(failed("stopped before it began"));
    }
    match action {
        NodeAction::Run {
            command,
            cwd,
            admin,
            ..
        } => {
            command_action(
                paths,
                &access,
                scope,
                &command,
                &cwd,
                admin,
                timeout,
                cancel,
                progress,
                session_env,
                secrets,
            )
            .await
        }
        NodeAction::Desktop { desktop, look } => {
            if let Err(why) = desktop_allowed(scope) {
                return Outcome::Finished(refused(why));
            }
            let stop = super::screen::Stop::default();
            let worker = stop.clone();
            run_desktop(
                move || super::screen::act(&desktop, look, &worker),
                stop,
                timeout,
                cancel,
            )
            .await
        }
        other => {
            // The worker looks at the stop right before it changes anything
            // (a folder, the file); stopped or out of time, the answer waits
            // for it: nothing is changed after that answer.
            let fence = cancel.child_token();
            let worker = fence.clone();
            let mut task =
                tokio::task::spawn_blocking(move || file_action(&access, other, &worker));
            let ended = |r: Result<NodeResult, tokio::task::JoinError>| {
                r.unwrap_or_else(|e| failed(format!("the action failed: {e}")))
            };
            let why = tokio::select! {
                r = &mut task => return Outcome::Finished(ended(r)),
                _ = tokio::time::sleep(timeout) => "stopped: the time limit ran out",
                _ = cancel.cancelled() => "stopped before it finished",
            };
            fence.cancel();
            let r = match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                Ok(r) => {
                    if ended(r).ok {
                        failed(format!(
                            "{why}, but it had already finished (what it did stays done)"
                        ))
                    } else {
                        failed(why)
                    }
                }
                Err(_) => failed(format!(
                    "{why}; it didn't stop within 10 seconds and may still change the file"
                )),
            };
            if cancel.is_cancelled() {
                Outcome::Cancelled(r)
            } else {
                Outcome::TimedOut(r)
            }
        }
    }
}

/// A desktop action on its own thread. Stopping it (the chat, the time
/// limit, the machine paused or narrowed) tells it to stop and waits until
/// it has let go of the mouse and keys before saying so: nothing it does
/// comes after the answer.
pub(super) async fn run_desktop(
    work: impl FnOnce() -> NodeResult + Send + 'static,
    stop: super::screen::Stop,
    timeout: Duration,
    cancel: CancellationToken,
) -> Outcome {
    let mut task = tokio::task::spawn_blocking(work);
    let ended = |r: Result<NodeResult, tokio::task::JoinError>| {
        r.unwrap_or_else(|e| failed(format!("the desktop action failed: {e}")))
    };
    let why = tokio::select! {
        r = &mut task => return Outcome::Finished(ended(r)),
        _ = tokio::time::sleep(timeout) => "stopped: the time limit ran out",
        _ = cancel.cancelled() => "stopped before it finished",
    };
    stop.stop();
    // From now on, what it still sends is refused where it arrives (it may
    // be past its last look, about to send), until it has ended.
    let fence = super::screen::fence();
    // What it holds right now, let go without waiting for it (it may be
    // stuck in Windows' call, and so may this).
    std::thread::spawn(super::screen::release_live);
    // Each step looks for the stop within a fraction of a second.
    let r = match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
        Ok(r) => {
            drop(fence);
            ended(r)
        }
        Err(_) => {
            // Not over: no desktop action runs here until it is, and then
            // what Windows still holds from it is let go.
            let unresolved = super::screen::quarantine();
            tokio::spawn(async move {
                let _ = task.await;
                let _ = tokio::task::spawn_blocking(move || {
                    super::screen::let_go(super::screen::begin_let_go());
                    drop(fence);
                    drop(unresolved);
                })
                .await;
            });
            failed(
                "it didn't stop within 10 seconds and may still be acting; no desktop action runs on this computer until it has ended and let go of what it held",
            )
        }
    };
    let r = NodeResult {
        output: if r.ok {
            why.to_string()
        } else {
            format!("{why}: {}", r.output)
        },
        ok: false,
        ..r
    };
    if cancel.is_cancelled() {
        Outcome::Cancelled(r)
    } else {
        Outcome::TimedOut(r)
    }
}

/// How an action ended.
pub enum Outcome {
    Finished(NodeResult),
    TimedOut(NodeResult),
    Cancelled(NodeResult),
}

impl Outcome {
    pub fn result(self) -> NodeResult {
        match self {
            Outcome::Finished(r) | Outcome::TimedOut(r) | Outcome::Cancelled(r) => r,
        }
    }
}

/// Whether `action`, asked by `bot`, is within `scope` (who may use it,
/// paths and switches; used to stop running actions a narrowed scope no
/// longer allows).
pub fn permitted(
    paths: &Paths,
    home: Option<&Path>,
    scope: &MachineScope,
    action: &NodeAction,
    bot: Option<(&str, &str)>,
) -> Result<(), String> {
    if scope.stopped {
        return Err(super::STOPPED_HERE.into());
    }
    super::may_use(scope, bot)?;
    let access = Access::new(paths, scope, home);
    match action {
        NodeAction::Run { cwd, admin, .. } => {
            if !scope.commands {
                return Err("commands were turned off on this machine".into());
            }
            if *admin && scope.admin == tessera_proto::AdminAccess::Off {
                return Err("administrator commands were turned off on this machine".into());
            }
            access.resolve(cwd, false).map(|_| ())
        }
        NodeAction::Desktop { .. } => desktop_allowed(scope),
        a => access.resolve(&a.target(), a.changes()).map(|_| ()),
    }
}

/// Desktop actions need desktop control on, and a computer that can do them.
fn desktop_allowed(scope: &MachineScope) -> Result<(), String> {
    if !scope.desktop {
        return Err("desktop control is off on this machine; only this computer can turn it on (Settings → Devices → This computer → Desktop control, or `tessera node preset desktop`)".into());
    }
    if !super::screen::AVAILABLE {
        return Err("desktop actions aren't available on this computer yet; with desktop control on, commands get the desktop session and can use its own tools".into());
    }
    Ok(())
}

// ---- files -------------------------------------------------------------------

fn file_action(access: &Access, action: NodeAction, stop: &CancellationToken) -> NodeResult {
    let r = match action {
        NodeAction::Read {
            path,
            offset,
            limit,
        } => read(access, &path, offset, limit),
        NodeAction::List { path } => list(access, &path),
        NodeAction::Write { path, content } => write(access, &path, &content, stop),
        NodeAction::Edit {
            path,
            old,
            new,
            all,
        } => edit(access, &path, &old, &new, all, stop),
        NodeAction::Run { .. } | NodeAction::Desktop { .. } => {
            Err(Denied::Failed("not a file action".into()))
        }
    };
    r.unwrap_or_else(Into::into)
}

/// Files a change is still being made to (an action's worker, until it
/// really ends, even past a stopped or timed-out answer): no other change
/// to the same file starts meanwhile.
static BUSY: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

struct Busy(PathBuf);

impl Busy {
    fn claim(path: &Path) -> Result<Self, Denied> {
        let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if busy.iter().any(|p| p == path) {
            return Err(Denied::Failed(format!(
                "an earlier change to {} is still finishing; try again in a moment",
                path.display()
            )));
        }
        busy.push(path.to_path_buf());
        Ok(Self(path.to_path_buf()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| p != &self.0);
    }
}

/// Right before a change: not stopped meanwhile.
fn not_stopped(stop: &CancellationToken) -> Result<(), Denied> {
    if stop.is_cancelled() {
        Err(Denied::Failed("stopped before it changed anything".into()))
    } else {
        Ok(())
    }
}

fn io(path: &Path, e: std::io::Error) -> Denied {
    let why = match e.kind() {
        std::io::ErrorKind::NotFound => "doesn't exist".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ if e.raw_os_error() == Some(40) => {
            "changed into a link while being opened; try again".to_string()
        }
        _ => e.to_string(),
    };
    Denied::Failed(format!("{}: {why}", path.display()))
}

/// The real location of an open file.
fn real_path(f: &std::fs::File) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd())).ok()
    }
    #[cfg(windows)]
    {
        crate::platform::windows::real_path(f)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = f;
        None
    }
}

/// Open `canon` (already checked) without following a final symlink, and
/// check again what was opened.
fn open_checked(
    access: &Access,
    canon: &Path,
    write: bool,
    dir: bool,
) -> Result<std::fs::File, Denied> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Non-blocking: opening a named pipe to read would otherwise wait
        // for a writer forever (regular files don't mind).
        let mut flags = libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        if dir {
            flags |= libc::O_DIRECTORY;
        }
        opts.custom_flags(flags);
    }
    // Windows opens a folder only with backup semantics, and a final link
    // itself (not what it points to) with the reparse-point flag.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
        if dir {
            flags |= FILE_FLAG_BACKUP_SEMANTICS;
        }
        opts.custom_flags(flags);
    }
    let _ = dir;
    let f = opts.open(canon).map_err(|e| io(canon, e))?;
    if let Some(real) = real_path(&f) {
        access.check(&real, write).map_err(Denied::Refused)?;
    }
    Ok(f)
}

fn read(
    access: &Access,
    raw: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<NodeResult, Denied> {
    use std::io::Read;
    let canon = access.resolve(raw, false).map_err(Denied::Refused)?;
    if canon.is_dir() {
        return Err(Denied::Failed(format!(
            "{} is a folder; list it instead",
            canon.display()
        )));
    }
    let f = open_checked(access, &canon, false, false)?;
    let meta = f.metadata().map_err(|e| io(&canon, e))?;
    if !meta.is_file() {
        return Err(Denied::Failed(format!(
            "{} is not a regular file",
            canon.display()
        )));
    }
    let size = meta.len();
    let mut bytes = Vec::new();
    f.take(MAX_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| io(&canon, e))?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Ok(done(format!(
            "{} is a binary file ({}); not shown.",
            canon.display(),
            crate::tools::files::human_size(size)
        )));
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = offset.unwrap_or(1).max(1) as usize;
    let limit = limit.unwrap_or(1000).clamp(1, 5000) as usize;
    let mut out =
        crate::tools::files::render_lines(&canon.display().to_string(), &text, offset, limit);
    if size > MAX_READ_BYTES {
        out.push_str(&format!(
            "\n[Only the first {} of {} were read.]",
            crate::tools::files::human_size(MAX_READ_BYTES),
            crate::tools::files::human_size(size)
        ));
    }
    Ok(done(out))
}

fn list(access: &Access, raw: &str) -> Result<NodeResult, Denied> {
    let canon = access.resolve(raw, false).map_err(Denied::Refused)?;
    let f = open_checked(access, &canon, false, true)?;
    #[cfg(target_os = "linux")]
    let at = {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd()))
    };
    #[cfg(not(target_os = "linux"))]
    let at = canon.clone();
    let rd = std::fs::read_dir(&at).map_err(|e| io(&canon, e))?;
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type() {
            Ok(t) if t.is_dir() => dirs.push(format!("{name}/")),
            Ok(t) if t.is_symlink() => files.push(format!("{name}@")),
            _ => {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                files.push(format!(
                    "{name}  ({})",
                    crate::tools::files::human_size(size)
                ));
            }
        }
    }
    drop(f);
    dirs.sort();
    files.sort();
    let total = dirs.len() + files.len();
    let mut lines: Vec<String> = dirs.into_iter().chain(files).collect();
    let more = lines.len().saturating_sub(MAX_ENTRIES);
    lines.truncate(MAX_ENTRIES);
    let mut out = format!(
        "{} ({total} entries)\n{}",
        canon.display(),
        lines.join("\n")
    );
    if more > 0 {
        out.push_str(&format!("\n[… {more} more entries]"));
    }
    Ok(done(out))
}

/// Open `canon` (checked for writing) to change it: through a handle on its
/// checked parent folder, never following a final symlink, refusing a file
/// that is hard-linked elsewhere. Creates it (and its folders) if `create`.
fn open_for_change(access: &Access, canon: &Path, create: bool) -> Result<std::fs::File, Denied> {
    let parent = canon
        .parent()
        .ok_or_else(|| Denied::Failed("not a file path".into()))?;
    let name = canon
        .file_name()
        .ok_or_else(|| Denied::Failed("not a file path".into()))?;
    let dir = if create && !parent.exists() {
        make_folders(access, parent)?
    } else {
        open_checked(access, parent, true, true)?
    };
    #[cfg(unix)]
    let f = {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let cname = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| Denied::Failed("the name contains a NUL byte".into()))?;
        let mut flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if create {
            flags |= libc::O_CREAT;
        }
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                cname.as_ptr(),
                flags,
                0o644 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io(canon, std::io::Error::last_os_error()));
        }
        unsafe { std::fs::File::from_raw_fd(fd) }
    };
    #[cfg(not(unix))]
    let f = {
        let _ = (dir, name);
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .open(canon)
            .map_err(|e| io(canon, e))?
    };
    let meta = f.metadata().map_err(|e| io(canon, e))?;
    if !meta.is_file() {
        return Err(Denied::Failed(format!(
            "{} is not a regular file",
            canon.display()
        )));
    }
    #[cfg(unix)]
    let linked = {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    };
    // The same on Windows (NTFS hard links): one file under two names
    // could be changed by two actions at once, past the busy check.
    #[cfg(windows)]
    let linked = crate::platform::windows::link_count(&f).is_none_or(|n| n > 1);
    #[cfg(not(any(unix, windows)))]
    let linked = false;
    if linked {
        return Err(Denied::Refused(format!(
            "{} is hard-linked to another place, which this machine doesn't change through it",
            canon.display()
        )));
    }
    if let Some(real) = real_path(&f) {
        access.check(&real, true).map_err(Denied::Refused)?;
    }
    Ok(f)
}

/// Make the missing folders of `dir` (checked for writing), one at a time
/// from the nearest one that exists (checked again where it really is),
/// never through a link: one swapped in since the check leads nowhere.
fn make_folders(access: &Access, dir: &Path) -> Result<std::fs::File, Denied> {
    let mut base = dir;
    while !base.exists() {
        base = base
            .parent()
            .ok_or_else(|| Denied::Failed("not a file path".into()))?;
    }
    let start = open_checked(access, base, true, true)?;
    #[cfg(unix)]
    {
        let rest = dir.strip_prefix(base).unwrap_or(Path::new(""));
        let made = crate::util::beneath::dir(&start, rest, true).map_err(|e| io(dir, e))?;
        if let Some(real) = real_path(&made) {
            access.check(&real, true).map_err(Denied::Refused)?;
        }
        Ok(made)
    }
    #[cfg(not(unix))]
    {
        drop(start);
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        open_checked(access, dir, true, true)
    }
}

fn replace_contents(f: &mut std::fs::File, canon: &Path, data: &[u8]) -> Result<(), Denied> {
    use std::io::{Seek, Write};
    f.set_len(0).map_err(|e| io(canon, e))?;
    f.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| io(canon, e))?;
    f.write_all(data).map_err(|e| io(canon, e))?;
    f.sync_all().map_err(|e| io(canon, e))
}

fn write(
    access: &Access,
    raw: &str,
    content: &str,
    stop: &CancellationToken,
) -> Result<NodeResult, Denied> {
    use std::io::Read;
    if content.len() > MAX_WRITE_BYTES {
        return Err(Denied::Failed(format!(
            "that is more than {} to write at once",
            crate::tools::files::human_size(MAX_WRITE_BYTES as u64)
        )));
    }
    let canon = access.resolve(raw, true).map_err(Denied::Refused)?;
    if canon.is_dir() {
        return Err(Denied::Failed(format!("{} is a folder", canon.display())));
    }
    let existed = canon.exists();
    let _busy = Busy::claim(&canon)?;
    // Before anything is made (its folders, the file).
    not_stopped(stop)?;
    let mut f = open_for_change(access, &canon, true)?;
    let mut old = Vec::new();
    let _ = (&mut f).take(MAX_WRITE_BYTES as u64).read_to_end(&mut old);
    not_stopped(stop)?;
    replace_contents(&mut f, &canon, content.as_bytes())?;
    let lines = content.lines().count();
    let mut out = format!(
        "{} {} ({lines} lines, {})",
        if existed { "Overwrote" } else { "Created" },
        canon.display(),
        crate::tools::files::human_size(content.len() as u64)
    );
    if existed && let Ok(old) = String::from_utf8(old) {
        let (added, removed, _) = crate::tools::files::diff_stats(&old, content, "");
        out.push_str(&format!(": +{added} −{removed} lines"));
    }
    Ok(done(out))
}

fn edit(
    access: &Access,
    raw: &str,
    old: &str,
    new: &str,
    all: bool,
    stop: &CancellationToken,
) -> Result<NodeResult, Denied> {
    use std::io::Read;
    if old.is_empty() {
        return Err(Denied::Failed(
            "`old` is empty: give the exact text to replace".into(),
        ));
    }
    let canon = access.resolve(raw, true).map_err(Denied::Refused)?;
    let _busy = Busy::claim(&canon)?;
    let mut f = open_for_change(access, &canon, false)?;
    let mut bytes = Vec::new();
    (&mut f)
        .take(MAX_WRITE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(&canon, e))?;
    if bytes.len() > MAX_WRITE_BYTES {
        return Err(Denied::Failed(format!(
            "{} is too large to edit here",
            canon.display()
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Denied::Failed(format!("{} isn't UTF-8 text", canon.display())))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err(Denied::Failed(format!(
            "`old` was not found in {}; read it again and copy the text exactly",
            canon.display()
        )));
    }
    if count > 1 && !all {
        return Err(Denied::Failed(format!(
            "`old` appears {count} times in {}; include more surrounding text, or set all=true",
            canon.display()
        )));
    }
    let updated = if all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    not_stopped(stop)?;
    replace_contents(&mut f, &canon, updated.as_bytes())?;
    let name = canon.display().to_string();
    let (added, removed, diff) = crate::tools::files::diff_stats(&text, &updated, &name);
    Ok(done(format!(
        "Edited {name}: {count} replacement{} (+{added} −{removed} lines)\n{}",
        if count == 1 { "" } else { "s" },
        crate::util::truncate(&diff, 4000)
    )))
}

// ---- commands ----------------------------------------------------------------

/// Environment variables a command here never gets: credentials (by name),
/// the desktop session, agent sockets and Tessera's own settings.
fn withheld(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "GPG_AGENT_INFO",
        "DBUS_SESSION_BUS_ADDRESS",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "SWAYSOCK",
        "I3SOCK",
        "NIRI_SOCKET",
        "TMUX",
        "TMUX_PANE",
        "STY",
        "KRB5CCNAME",
        "BASH_ENV",
        "ENV",
        "PROMPT_COMMAND",
        "TMPDIR",
    ];
    const WORDS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTH",
        "COOKIE",
        "SESSION",
    ];
    let upper = name.to_ascii_uppercase();
    EXACT.contains(&upper.as_str())
        || upper.starts_with("TESSERA")
        || WORDS.iter().any(|w| upper.contains(w))
}

type Env = Vec<(std::ffi::OsString, std::ffi::OsString)>;

/// A command's environment: this process's, without credentials and the
/// desktop session; with desktop control, the session's own variables
/// (`session`) and the filtered session bus (`bus`, a socket path).
fn command_env(tmp: &Path, session: Option<&super::desktop::Session>, bus: Option<&Path>) -> Env {
    let mut env: Env = crate::tools::bash::agent_environment(std::env::vars_os(), &[])
        .into_iter()
        .filter(|(k, _)| !withheld(&k.to_string_lossy()))
        .collect();
    if let Some(s) = session {
        for (k, v) in &s.vars {
            env.push((k.into(), v.into()));
        }
        if let Some(b) = bus {
            let mut addr = std::ffi::OsString::from("unix:path=");
            addr.push(b.as_os_str());
            env.push(("DBUS_SESSION_BUS_ADDRESS".into(), addr));
        }
    }
    for (k, v) in [
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("DEBIAN_FRONTEND", "noninteractive"),
        ("TESSERA", "1"),
    ] {
        env.push((k.into(), v.into()));
    }
    env.push(("TMPDIR".into(), tmp.as_os_str().to_owned()));
    env
}

/// The sandbox for a command here: Tessera's data, the session's control
/// sockets and other programs' credential stores (unless allowed) hidden;
/// files changeable only in the read-write folders (opened up around
/// startup files and the Tessera program) and the command's own temporary
/// folder. With desktop control, the graphical session's sockets
/// (`session`) stay visible inside the hidden runtime folder.
#[cfg_attr(windows, allow(dead_code))]
fn command_spec(
    paths: &Paths,
    access: &Access,
    tmp: &Path,
    session: Option<&super::desktop::Session>,
) -> crate::sandbox::Spec {
    let allowed = |c: &PathBuf| access.folders.iter().any(|(f, _)| f.starts_with(c));
    let mut hide: Vec<PathBuf> = super::protected(paths).roots;
    hide.extend(crate::sandbox::session_hides());
    // Where older Hyprland keeps its control sockets.
    hide.push(PathBuf::from("/tmp/hypr"));
    // The session's own runtime folder, if elsewhere: only what it keeps shows.
    if let Some(rt) = session.and_then(|s| s.vars.iter().find(|(k, _)| k == "XDG_RUNTIME_DIR")) {
        hide.push(PathBuf::from(&rt.1));
    }
    hide.extend(access.credentials.iter().filter(|c| !allowed(c)).cloned());
    // Single credential files, unless their own folder (never the home
    // folder) was allowed.
    let own_folder = |c: &PathBuf| {
        c.parent().is_some_and(|d| {
            access.home.as_deref() != Some(d) && access.folders.iter().any(|(f, _)| f == d)
        })
    };
    hide.extend(
        super::credential_files(access.home.as_deref())
            .into_iter()
            .filter(|c| !own_folder(c)),
    );
    let mut folders = access.folders.clone();
    if !access.desktop {
        folders = crate::sandbox::protect_startup(folders, access.startup.clone());
    }
    let mut writable = access.writable();
    writable.push(tmp.to_path_buf());
    crate::sandbox::Spec {
        hide,
        keep: session.map(|s| s.keep.clone()).unwrap_or_default(),
        folders,
        writable: Some(writable),
        group: String::new(),
    }
}

/// The sandbox for a terminal opened here from one of the user's devices
/// ("Sandboxed"): everything the account can change stays changeable,
/// except what no command here ever changes — Tessera's program and the
/// startup files and folders (so nothing done in it runs outside it later,
/// such as a changed `.bashrc` or service file at the next login). As for
/// commands, nothing new can appear beside one of those either (a folder
/// holding one is writable entry by entry). Tessera's data and the
/// session's control sockets are hidden. Unlike an agent's command, other
/// programs' credentials stay where they are: it is the user's own command
/// line. Needs Landlock ABI 3+, like commands here.
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn terminal_spec(
    paths: &Paths,
    home: Option<&Path>,
    group: String,
) -> crate::sandbox::Spec {
    let access = Access {
        folders: vec![(PathBuf::from("/"), true)],
        protected: super::protected(paths).canonical(),
        credentials: Vec::new(),
        startup: both(super::startup_paths(home)),
        program: super::program_paths(),
        desktop: false,
        home: home.map(Path::to_path_buf),
        mac: cfg!(target_os = "macos"),
    };
    let mut hide: Vec<PathBuf> = super::protected(paths).roots;
    hide.extend(crate::sandbox::session_hides());
    // Where older Hyprland keeps its control sockets.
    hide.push(PathBuf::from("/tmp/hypr"));
    // Belt and braces: existing startup folders are read-only mounts too.
    let folders = access
        .startup
        .iter()
        .filter(|p| p.is_dir())
        .map(|p| (p.clone(), false))
        .collect();
    crate::sandbox::Spec {
        hide,
        keep: crate::sandbox::session_keeps(),
        folders,
        writable: Some(access.writable()),
        group,
    }
}

#[allow(clippy::too_many_arguments)]
async fn command_action(
    paths: &Paths,
    access: &Access,
    scope: &MachineScope,
    command: &str,
    cwd: &str,
    admin: bool,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Option<Progress>,
    session_env: SessionEnv,
    secrets: &tessera_proto::SecretEnv,
) -> Outcome {
    if !scope.commands {
        return Outcome::Finished(refused(
            "commands are off on this machine (only that computer can turn them on: Work in folders or Full access)",
        ));
    }
    if command.trim().is_empty() {
        return Outcome::Finished(failed("give a command"));
    }
    let cwd = match access.resolve(cwd, false) {
        Ok(c) if c.is_dir() => c,
        Ok(c) => {
            return Outcome::Finished(failed(format!("{} is not a folder", c.display())));
        }
        Err(e) => return Outcome::Finished(refused(e)),
    };
    // A private temporary folder, removed afterwards.
    let tmp =
        std::env::temp_dir().join(format!("tessera-machine-{}", crate::util::random_token(8)));
    if let Err(e) = make_private_dir(&tmp) {
        return Outcome::Finished(failed(format!("couldn't make a temporary folder: {e}")));
    }
    // Desktop control: the graphical session, read now (it may have
    // started, restarted or moved since the machine did), and the session
    // bus only through a filter.
    let session = match (scope.desktop, session_env) {
        (false, _) => None,
        (true, Some(env)) => super::desktop::from_env(&env),
        (true, None) => tokio::task::spawn_blocking(super::desktop::discover)
            .await
            .ok()
            .flatten(),
    };
    let filter = match session.as_ref().and_then(|s| s.bus.as_deref()) {
        Some(bus) => super::desktop::BusFilter::start(bus, &tmp.join("bus"))
            .map_err(|e| tracing::warn!("no session bus for this command: {e}"))
            .ok(),
        None => None,
    };
    let mut env = command_env(&tmp, session.as_ref(), filter.as_ref().map(|f| f.socket()));
    let redactions = crate::auth::env_forms(&secrets.0);
    // The stored secrets the chat may use here, by the names it gave
    // (never one that changes how programs run).
    for (k, v) in &secrets.0 {
        if crate::auth::valid_secret_name(k).is_ok() {
            env.retain(|(name, _)| name.to_str() != Some(k.as_str()));
            env.push((k.into(), v.into()));
        }
    }
    // As administrator (allowed here, approved as this machine asks):
    // outside the sandbox, where `sudo` works, with the password saved here
    // handed to it on a pipe (see `admin.rs`).
    if admin {
        drop(filter);
        let outcome = admin_command(
            paths, scope, command, &cwd, env, timeout, cancel, progress, redactions,
        )
        .await;
        let _ = std::fs::remove_dir_all(&tmp);
        return outcome;
    }
    // Windows has no command sandbox (yet): commands run there only with
    // Full access, which the person at that computer chose knowing that.
    #[cfg(windows)]
    {
        drop(filter);
        let outcome = if super::commands_run_here(scope) {
            env.push(("TEMP".into(), tmp.as_os_str().to_owned()));
            env.push(("TMP".into(), tmp.as_os_str().to_owned()));
            spawn(
                command, &cwd, env, None, timeout, cancel, progress, redactions,
            )
            .await
        } else {
            Outcome::Finished(refused(super::commands_refused_why()))
        };
        let _ = (paths, std::fs::remove_dir_all(&tmp));
        outcome
    }
    #[cfg(not(windows))]
    let spec = command_spec(paths, access, &tmp, session.as_ref());
    #[cfg(not(windows))]
    let outcome = match crate::sandbox::prepare(Some(&(spec, true)), &cwd) {
        Ok(Some(sandbox)) => {
            spawn(
                command,
                &cwd,
                env,
                Some(sandbox),
                timeout,
                cancel,
                progress,
                redactions,
            )
            .await
        }
        Ok(None) => Outcome::Finished(refused(
            "commands need the sandbox, which isn't available here",
        )),
        Err(e) => Outcome::Finished(refused(e)),
    };
    #[cfg(not(windows))]
    {
        drop(filter);
        let _ = std::fs::remove_dir_all(&tmp);
        outcome
    }
}

/// Run `command` outside the sandbox through [`super::admin::script`]:
/// with `password`, it reaches `sudo` (at `sudo`) on descriptor 3 only.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn admin_spawn(
    command: &str,
    cwd: &Path,
    env: Env,
    sudo: Option<&Path>,
    password: Option<&str>,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Option<Progress>,
    mut redactions: Vec<String>,
) -> Outcome {
    if let Some(password) = password {
        redactions.extend(crate::auth::env_forms(&std::collections::BTreeMap::from([
            ("SUDO_PASSWORD".into(), password.into()),
        ])));
    }
    let fd = match (sudo, password) {
        (Some(_), Some(p)) => match super::admin::password_pipe(p) {
            Ok(fd) => Some(fd),
            Err(e) => {
                return Outcome::Finished(failed(format!(
                    "couldn't hand the password to sudo: {e}"
                )));
            }
        },
        _ => None,
    };
    let script = super::admin::script(command, fd.as_ref().and(sudo));
    spawn_with(
        &script, cwd, env, None, timeout, cancel, progress, redactions, fd,
    )
    .await
}

/// A command as this computer's administrator (`admin.rs`).
#[allow(clippy::too_many_arguments)]
async fn admin_command(
    paths: &Paths,
    scope: &MachineScope,
    command: &str,
    cwd: &Path,
    env: Env,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Option<Progress>,
    redactions: Vec<String>,
) -> Outcome {
    if scope.admin == tessera_proto::AdminAccess::Off {
        return Outcome::Finished(refused(
            "administrator commands are off on this machine (only this computer can turn them on: Settings → Devices → This computer → Administrator, or `tessera node admin on`)",
        ));
    }
    #[cfg(unix)]
    {
        let password = super::admin::password(paths);
        let sudo = match &password {
            Some(_) => match super::admin::system_sudo() {
                Some(s) => Some(s),
                None => return Outcome::Finished(failed("sudo isn't installed on this computer")),
            },
            None => None,
        };
        let mut outcome = admin_spawn(
            command,
            cwd,
            env,
            sudo.as_deref(),
            password.as_deref(),
            timeout,
            cancel,
            progress,
            redactions,
        )
        .await;
        // Never the password itself, whatever the command printed.
        if let Some(p) = &password {
            let scrub = crate::auth::value_forms([p]);
            let (Outcome::Finished(r) | Outcome::TimedOut(r) | Outcome::Cancelled(r)) =
                &mut outcome;
            r.output = crate::tools::redact_known(&r.output, &scrub);
        }
        outcome
    }
    #[cfg(not(unix))]
    {
        let _ = (
            paths, command, cwd, env, timeout, cancel, progress, redactions,
        );
        Outcome::Finished(refused(
            "administrator commands aren't available on Windows yet",
        ))
    }
}

fn make_private_dir(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(p)
    }
    #[cfg(not(unix))]
    std::fs::create_dir(p)
}

#[allow(clippy::too_many_arguments)]
async fn spawn(
    command: &str,
    cwd: &Path,
    env: Env,
    sandbox: Option<crate::sandbox::Prepared>,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Option<Progress>,
    redactions: Vec<String>,
) -> Outcome {
    #[cfg(unix)]
    {
        spawn_with(
            command, cwd, env, sandbox, timeout, cancel, progress, redactions, None,
        )
        .await
    }
    #[cfg(not(unix))]
    {
        spawn_with(
            command, cwd, env, sandbox, timeout, cancel, progress, redactions,
        )
        .await
    }
}

/// [`spawn`], with `fd3` (if any) as the command's file descriptor 3 (an
/// admin command's password for `sudo`, `admin.rs`).
#[allow(clippy::too_many_arguments)]
async fn spawn_with(
    command: &str,
    cwd: &Path,
    env: Env,
    sandbox: Option<crate::sandbox::Prepared>,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Option<Progress>,
    redactions: Vec<String>,
    #[cfg(unix)] fd3: Option<std::os::fd::OwnedFd>,
) -> Outcome {
    let started = Instant::now();
    let mut cmd = crate::tools::bash::shell_command(command, sandbox.as_ref());
    cmd.env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    // Git Bash reads the command from a variable (`platform::shell`), which
    // the clean environment above dropped.
    #[cfg(windows)]
    cmd.envs(crate::platform::shell().command_line(command).1);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            // Establish the session before protected anchors are forked.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if let Some(s) = &sandbox
        && let Err(e) = s.protect_environment(cmd.as_std_mut())
    {
        return Outcome::Finished(failed(format!("couldn't prepare the command: {e}")));
    }
    #[cfg(unix)]
    let fd3_raw = {
        use std::os::fd::AsRawFd;
        fd3.as_ref().map(|f| f.as_raw_fd())
    };
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            // The admin command's password pipe as descriptor 3 (kept open
            // across exec; the script closes it after `sudo -v`).
            if let Some(fd) = fd3_raw {
                if fd == 3 {
                    let flags = libc::fcntl(3, libc::F_GETFD);
                    if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                } else if libc::dup2(fd, 3) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            // Below the machine's own work and the node connection.
            crate::tools::bash::yield_to_server();
            match &sandbox {
                Some(s) => s.enter(),
                None => Ok(()),
            }
        });
    }
    #[cfg(not(unix))]
    let _ = sandbox;
    // Stopped while it was being prepared (a backup first, say): it never starts.
    if cancel.is_cancelled() {
        return Outcome::Cancelled(failed("stopped before it began"));
    }
    let mut child = match crate::platform::spawn(&mut cmd, crate::platform::Start::Agent) {
        Ok(c) => c,
        Err(e) => return Outcome::Finished(failed(format!("couldn't start the command: {e}"))),
    };
    // The child has its copy; ours goes now.
    #[cfg(unix)]
    drop(fd3);
    let pid = child.id().unwrap_or(0);
    // Its job, kept by this command for its whole life: ended through it,
    // never by process id (which may name another process once it ended).
    #[cfg(windows)]
    let job = crate::platform::windows::take_job(pid);
    let output = Arc::new(Mutex::new([String::new(), String::new()]));
    let mut readers = Vec::new();
    let drain = CancellationToken::new();
    for (index, stream) in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    .enumerate()
    {
        let output = output.clone();
        let progress = progress.clone();
        let mut redactor = crate::tools::redaction::Stream::terminal(redactions.clone());
        let drain = drain.clone();
        readers.push(tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = vec![0u8; 16 * 1024];
            let publish = |text: String| {
                let text = crate::tools::bash::strip_ansi(&text);
                if text.is_empty() {
                    return;
                }
                {
                    let mut streams = output.lock();
                    let out = &mut streams[index];
                    out.push_str(&text);
                    if out.len() > MAX_OUTPUT {
                        let cut = crate::util::ceil_char(out, out.len() - MAX_OUTPUT / 2);
                        *out = format!("[… earlier output dropped …]\n{}", &out[cut..]);
                    }
                }
                if let Some(p) = &progress {
                    p(
                        &crate::tools::scrub_patterns(&text),
                        if index == 0 { "stdout" } else { "stderr" },
                    );
                }
            };
            loop {
                let read = tokio::select! {
                    read = stream.read(&mut buf) => read,
                    _ = drain.cancelled() => break,
                };
                let n = match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                publish(redactor.push(&buf[..n]));
            }
            publish(redactor.finish());
        }));
    }
    enum End {
        Exit(i32),
        TimedOut,
        Cancelled,
    }
    let end = tokio::select! {
        s = child.wait() => End::Exit(s.ok().and_then(|s| s.code()).unwrap_or(-1)),
        _ = tokio::time::sleep(timeout) => End::TimedOut,
        _ = cancel.cancelled() => End::Cancelled,
    };
    if !matches!(end, End::Exit(_)) {
        #[cfg(windows)]
        match &job {
            Some(job) => job.end(),
            None => stop_tree(pid),
        }
        #[cfg(not(windows))]
        stop_tree(pid);
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    }
    // Done: what it left running in its job ends too, as on Linux (where
    // the sandbox ends with the command); a Stop couldn't reach it once the
    // command's record is gone. Through its job only (never by id now).
    #[cfg(windows)]
    if let Some(job) = &job {
        job.end();
    }
    // A grandchild may hold the pipes open: don't wait for it forever.
    for mut r in readers {
        if tokio::time::timeout(Duration::from_secs(2), &mut r)
            .await
            .is_err()
        {
            drain.cancel();
            let _ = r.await;
        }
    }
    let combined = output.lock().join("");
    let body = crate::tools::scrub_patterns(combined.trim_end());
    let secs = started.elapsed().as_secs_f32();
    let status = match &end {
        End::Exit(c) => format!("[exit {c} · {secs:.1}s]"),
        End::TimedOut => format!(
            "[stopped: the time limit ({}) ran out · {secs:.1}s]",
            super::human_secs(timeout.as_secs())
        ),
        End::Cancelled => format!("[stopped · {secs:.1}s]"),
    };
    let text = if body.is_empty() {
        format!("(no output) {status}")
    } else {
        format!("{body}\n{status}")
    };
    let result = |ok: bool, code: Option<i32>| NodeResult {
        output: text.clone(),
        ok,
        refused: false,
        exit_code: code,
        image: None,
    };
    match end {
        End::Exit(c) => Outcome::Finished(result(c == 0, Some(c))),
        End::TimedOut => Outcome::TimedOut(result(false, None)),
        End::Cancelled => Outcome::Cancelled(result(false, None)),
    }
}

/// Stop a command: its process group, and every process it started that
/// is still its descendant (some leave the group), politely then for good.
pub fn stop_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        let mut all = descendants(pid);
        all.push(pid);
        for p in &all {
            unsafe {
                libc::kill(*p as i32, libc::SIGTERM);
            }
        }
        crate::tools::bash::kill_group(pid);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            for p in all {
                unsafe {
                    libc::kill(p as i32, libc::SIGKILL);
                }
            }
        });
    }
    #[cfg(not(unix))]
    crate::tools::bash::kill_group(pid);
}

/// Processes descended from `pid` (from `/proc`).
#[cfg(unix)]
fn descendants(pid: u32) -> Vec<u32> {
    let mut parent: Vec<(u32, u32)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Some(p) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat")) else {
                continue;
            };
            // `pid (comm) state ppid …`; comm may hold spaces and parens.
            let ppid = stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u32>().ok());
            if let Some(pp) = ppid
                && !crate::sandbox::supervisor_auxiliary(p)
            {
                parent.push((p, pp));
            }
        }
    }
    let mut out = Vec::new();
    let mut frontier = vec![pid];
    while let Some(p) = frontier.pop() {
        for (child, _) in parent.iter().filter(|(_, pp)| *pp == p) {
            if !out.contains(child) {
                out.push(*child);
                frontier.push(*child);
            }
        }
    }
    out
}

#[cfg(all(test, unix))]
pub(crate) fn has_descendant_for_tests(root: u32, child: u32) -> bool {
    descendants(root).contains(&child)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop action stopped halfway (the chat stopped it, or its time
    /// ran out) is answered only once it has let go: nothing it does comes
    /// after the answer.
    #[tokio::test]
    async fn a_stopped_desktop_action_is_answered_only_after_it_let_go() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for by_time in [false, true] {
            let stop = super::super::screen::Stop::default();
            let worker = stop.clone();
            let let_go = Arc::new(AtomicBool::new(false));
            let flag = let_go.clone();
            let cancel = CancellationToken::new();
            if !by_time {
                let c = cancel.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    c.cancel();
                });
            }
            let out = run_desktop(
                move || {
                    while !worker.stopped() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    // Letting go of the keys takes a moment.
                    std::thread::sleep(Duration::from_millis(150));
                    flag.store(true, Ordering::SeqCst);
                    done("typed 3 of 40 characters")
                },
                stop,
                Duration::from_millis(if by_time { 100 } else { 30_000 }),
                cancel,
            )
            .await;
            assert!(let_go.load(Ordering::SeqCst), "answered before it let go");
            match out {
                Outcome::Cancelled(r) if !by_time => {
                    assert!(
                        !r.ok && r.output.starts_with("stopped before it finished"),
                        "{r:?}"
                    )
                }
                Outcome::TimedOut(r) if by_time => {
                    assert!(!r.ok && r.output.contains("time limit"), "{r:?}")
                }
                _ => panic!("ended the wrong way (by time: {by_time})"),
            }
        }
        // One that finishes on its own is just its answer.
        let out = run_desktop(
            || done("Clicked at 1,1."),
            super::super::screen::Stop::default(),
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(out, Outcome::Finished(r) if r.ok && r.output == "Clicked at 1,1."));
    }
    use tessera_proto::FolderAccess;

    #[cfg(unix)]
    #[tokio::test]
    async fn node_command_streams_redact_each_pipe_before_progress_and_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let value = "fixture-value-123456";
        std::fs::write(
            dir.path().join("output.py"),
            r#"import os, time
value = os.environ['LAB_VALUE'].encode()
for split in range(len(value) + 1):
    for fd in (1, 2):
        os.write(fd, b'before ' + value[:split])
        time.sleep(.01)
        os.write(fd, value[split:] + b' after\n')
"#,
        )
        .unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let progress: Progress = {
            let seen = seen.clone();
            Arc::new(move |text, _| seen.lock().push_str(text))
        };
        let result = spawn(
            "python3 output.py",
            dir.path(),
            vec![
                ("LAB_VALUE".into(), value.into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
            None,
            Duration::from_secs(10),
            CancellationToken::new(),
            Some(progress),
            vec![value.into()],
        )
        .await
        .result();
        assert!(result.ok);
        assert!(!result.output.contains(value));
        assert!(!seen.lock().contains(value));
        assert_eq!(
            result.output.matches("[REDACTED]").count(),
            (value.len() + 1) * 2
        );
        assert_eq!(
            seen.lock().matches("[REDACTED]").count(),
            (value.len() + 1) * 2
        );
    }
    /// An administrator command (`admin.rs`): the saved password reaches
    /// `sudo` (here a stand-in at its full path) on a pipe, and the command
    /// itself never gets it: not on descriptor 3, not in its environment.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_sudo_password_reaches_sudo_and_nothing_else() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let got = dir.path().join("got");
        let fake = dir.path().join("sudo");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\ncase \"$1\" in -S) IFS= read -r pw; printf '%s' \"$pw\" > '{}'; exit 0;; -k) exit 0;; esac\nexit 1\n",
                got.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir(&tmp).unwrap();
        let out = admin_spawn(
            "echo ran; (cat <&3) 2>/dev/null && echo fd3-open || echo fd3-closed; echo in-env:$(env | grep -c pw-1234-secret)",
            dir.path(),
            command_env(&tmp, None, None),
            Some(&fake),
            Some("pw-1234-secret"),
            Duration::from_secs(20),
            CancellationToken::new(),
            None,
            vec![],
        )
        .await
        .result();
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "pw-1234-secret");
        assert!(out.ok, "{}", out.output);
        for want in ["ran", "fd3-closed", "in-env:0"] {
            assert!(out.output.contains(want), "{want}: {}", out.output);
        }
        assert!(!out.output.contains("pw-1234-secret"), "{}", out.output);
        // A password sudo refuses: the command doesn't run, and it says why.
        std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
        let out = admin_spawn(
            "echo should-not-run",
            dir.path(),
            command_env(&tmp, None, None),
            Some(&fake),
            Some("pw-1234-secret"),
            Duration::from_secs(20),
            CancellationToken::new(),
            None,
            vec![],
        )
        .await
        .result();
        assert!(
            !out.ok
                && !out.output.contains("should-not-run")
                && out.output.contains("did not accept the password"),
            "{}",
            out.output
        );
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        /// A stand-in home folder (never the real one).
        home: PathBuf,
        ro: PathBuf,
        rw: PathBuf,
        outside: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::at(root.join("tessera-home"));
        paths.ensure().unwrap();
        std::fs::write(paths.config_dir.join("node.json"), "{}").unwrap();
        let home = root.join("me");
        let (ro, rw, outside) = (home.join("docs"), home.join("code"), root.join("private"));
        for d in [
            &ro,
            &rw,
            &outside,
            &home.join(".config/hypr"),
            &home.join(".config/app"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(ro.join("notes.txt"), "line one\nline two\n").unwrap();
        std::fs::write(outside.join("diary.txt"), "dear diary").unwrap();
        std::fs::write(home.join(".bashrc"), "# shell\n").unwrap();
        std::fs::write(
            home.join(".config/hypr/hyprland.conf"),
            "monitor=,preferred,auto,1\n",
        )
        .unwrap();
        Fixture {
            _dir: dir,
            paths,
            home,
            ro,
            rw,
            outside,
        }
    }

    fn scope(f: &Fixture) -> MachineScope {
        MachineScope {
            folders: vec![
                FolderAccess {
                    path: f.ro.display().to_string(),
                    write: false,
                },
                FolderAccess {
                    path: f.rw.display().to_string(),
                    write: true,
                },
            ],
            ..Default::default()
        }
    }

    fn home_rw(f: &Fixture, desktop: bool) -> MachineScope {
        MachineScope {
            folders: vec![FolderAccess {
                path: f.home.display().to_string(),
                write: true,
            }],
            commands: true,
            desktop,
            ..Default::default()
        }
    }

    fn access(f: &Fixture, s: &MachineScope) -> Access {
        Access::new(&f.paths, s, Some(&f.home))
    }

    fn file(access: &Access, action: NodeAction) -> NodeResult {
        file_action(access, action, &CancellationToken::new())
    }

    #[test]
    fn a_stopped_file_action_changes_nothing() {
        let f = fixture();
        let a = access(&f, &home_rw(&f, false));
        let path = f.home.join("notes.txt");
        std::fs::write(&path, "before").unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        let r = file_action(
            &a,
            NodeAction::Write {
                path: path.display().to_string(),
                content: "after".into(),
            },
            &stop,
        );
        assert!(!r.ok && r.output.contains("stopped"), "{r:?}");
        let r = file_action(
            &a,
            NodeAction::Edit {
                path: path.display().to_string(),
                old: "before".into(),
                new: "after".into(),
                all: false,
            },
            &stop,
        );
        assert!(!r.ok && r.output.contains("stopped"), "{r:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
        // Nor does it make anything (a folder, the file).
        let fresh = f.home.join("new-folder").join("new.txt");
        let r = file_action(
            &a,
            NodeAction::Write {
                path: fresh.display().to_string(),
                content: "x".into(),
            },
            &stop,
        );
        assert!(!r.ok && !fresh.parent().unwrap().exists(), "{r:?}");
    }

    #[test]
    fn files_stay_inside_the_allowed_folders() {
        let f = fixture();
        let access = access(&f, &scope(&f));
        let p = |d: &Path, n: &str| d.join(n).display().to_string();
        let r = file(
            &access,
            NodeAction::Read {
                path: p(&f.ro, "notes.txt"),
                offset: None,
                limit: None,
            },
        );
        assert!(r.ok && r.output.contains("2:line two"), "{}", r.output);
        // `~` is this machine's home folder.
        let r = file(
            &access,
            NodeAction::Read {
                path: "~/docs/notes.txt".into(),
                offset: None,
                limit: None,
            },
        );
        assert!(r.ok, "{}", r.output);
        let r = file(
            &access,
            NodeAction::Read {
                path: p(&f.outside, "diary.txt"),
                offset: None,
                limit: None,
            },
        );
        assert!(
            r.refused && !r.output.contains("dear diary"),
            "{}",
            r.output
        );
        let r = file(
            &access,
            NodeAction::Write {
                path: p(&f.ro, "new.txt"),
                content: "x".into(),
            },
        );
        assert!(r.refused, "read-only folders refuse writes: {}", r.output);
        let r = file(
            &access,
            NodeAction::Write {
                path: p(&f.rw, "src/main.rs"),
                content: "fn main() {}\n".into(),
            },
        );
        assert!(r.ok, "{}", r.output);
        assert_eq!(
            std::fs::read_to_string(f.rw.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let r = file(
            &access,
            NodeAction::Edit {
                path: p(&f.rw, "src/main.rs"),
                old: "{}".into(),
                new: "{ println!(\"hi\"); }".into(),
                all: false,
            },
        );
        assert!(r.ok && r.output.contains("+1 −1"), "{}", r.output);
        let r = file(
            &access,
            NodeAction::List {
                path: f.rw.display().to_string(),
            },
        );
        assert!(r.ok && r.output.contains("src/"), "{}", r.output);
        // `..` can't climb out.
        let r = file(
            &access,
            NodeAction::Read {
                path: format!("{}/../../private/diary.txt", f.rw.display()),
                offset: None,
                limit: None,
            },
        );
        assert!(r.refused, "{}", r.output);
        // Tessera's own settings, even through an allowed parent.
        let wide = MachineScope {
            folders: vec![FolderAccess {
                path: f.paths.config_dir.parent().unwrap().display().to_string(),
                write: false,
            }],
            ..Default::default()
        };
        let access_wide = Access::new(&f.paths, &wide, Some(&f.home));
        let r = file(
            &access_wide,
            NodeAction::Read {
                path: p(&f.paths.config_dir, "node.json"),
                offset: None,
                limit: None,
            },
        );
        assert!(
            r.refused && r.output.contains("Tessera's own"),
            "{}",
            r.output
        );
        // Relative paths mean nothing on another computer.
        let r = file(
            &access,
            NodeAction::Read {
                path: "notes.txt".into(),
                offset: None,
                limit: None,
            },
        );
        assert!(r.refused, "{}", r.output);
    }

    #[cfg(unix)]
    #[test]
    fn links_cant_lead_out() {
        let f = fixture();
        let access = access(&f, &scope(&f));
        std::os::unix::fs::symlink(&f.outside, f.rw.join("door")).unwrap();
        std::os::unix::fs::symlink(f.outside.join("diary.txt"), f.rw.join("diary")).unwrap();
        for path in [f.rw.join("door/diary.txt"), f.rw.join("diary")] {
            let r = file(
                &access,
                NodeAction::Read {
                    path: path.display().to_string(),
                    offset: None,
                    limit: None,
                },
            );
            assert!(
                r.refused && !r.output.contains("dear diary"),
                "{}",
                r.output
            );
            let r = file(
                &access,
                NodeAction::Write {
                    path: path.display().to_string(),
                    content: "overwritten".into(),
                },
            );
            assert!(r.refused, "{}", r.output);
        }
        assert_eq!(
            std::fs::read_to_string(f.outside.join("diary.txt")).unwrap(),
            "dear diary"
        );
        // A hard link to a file elsewhere is not changed through.
        if std::fs::hard_link(f.outside.join("diary.txt"), f.rw.join("hard")).is_ok() {
            let r = file(
                &access,
                NodeAction::Write {
                    path: f.rw.join("hard").display().to_string(),
                    content: "x".into(),
                },
            );
            assert!(r.refused, "{}", r.output);
            assert_eq!(
                std::fs::read_to_string(f.outside.join("diary.txt")).unwrap(),
                "dear diary"
            );
        }
    }

    /// A link put in place after a path was checked (here: a path checked
    /// before `door` became one) leads nowhere: missing folders are made
    /// one at a time from the checked folder, never through a link.
    #[cfg(unix)]
    #[test]
    fn new_folders_are_never_made_through_a_link() {
        let f = fixture();
        let access = access(&f, &scope(&f));
        std::os::unix::fs::symlink(&f.outside, f.rw.join("door")).unwrap();
        let stale = f.rw.join("door/new/notes.txt");
        assert!(open_for_change(&access, &stale, true).is_err());
        assert!(!f.outside.join("new").exists(), "nothing made outside");
        // Ordinary nested folders are still made.
        let fresh = f.rw.join("a/b/c.txt");
        let mut file = open_for_change(&access, &fresh, true).unwrap();
        replace_contents(&mut file, &fresh, b"hi").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "hi");
    }

    /// Reading a named pipe answers at once instead of waiting for a writer.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_not_read() {
        let f = fixture();
        let access = access(&f, &scope(&f));
        let pipe = f.rw.join("pipe");
        let c = std::ffi::CString::new(pipe.display().to_string()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let path = pipe.display().to_string();
        std::thread::spawn(move || {
            let r = file(
                &access,
                NodeAction::Read {
                    path,
                    offset: None,
                    limit: None,
                },
            );
            let _ = tx.send(r);
        });
        let r = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the read answered");
        assert!(r.output.contains("not a regular file"), "{}", r.output);
    }

    #[test]
    fn startup_files_and_credentials_stay_out_of_reach() {
        let f = fixture();
        let home = &f.home;
        let ro = MachineScope {
            folders: vec![FolderAccess {
                path: home.display().to_string(),
                write: false,
            }],
            ..Default::default()
        };
        let a = access(&f, &ro);
        assert!(a.check(&home.join(".ssh/id_ed25519"), false).is_err());
        assert!(a.check(&home.join(".aws/credentials"), false).is_err());
        assert!(a.check(&home.join(".netrc"), false).is_err());
        assert!(a.check(&home.join("README"), false).is_ok());
        // Full access: the home folder read-write, startup files still not.
        let full = access(&f, &home_rw(&f, false));
        assert!(
            full.check(&home.join(".config/systemd/user/x.service"), true)
                .is_err()
        );
        assert!(
            full.check(&home.join(".config/autostart/x.desktop"), true)
                .is_err()
        );
        assert!(full.check(&home.join(".bashrc"), true).is_err());
        assert!(
            full.check(&home.join(".bash_login"), true).is_err(),
            "not even new ones"
        );
        assert!(
            full.check(&home.join(".config/app/settings.toml"), true)
                .is_ok()
        );
        assert!(full.check(&home.join("code/x.rs"), true).is_ok());
        assert!(
            full.check(&f.paths.config_dir.join("node.json"), false)
                .is_err()
        );
        // Desktop control: startup files may change; Tessera's own data
        // and the credential stores stay out of reach.
        let desk = access(&f, &home_rw(&f, true));
        assert!(
            desk.check(&home.join(".config/hypr/hyprland.conf"), true)
                .is_ok()
        );
        assert!(desk.check(&home.join(".bashrc"), true).is_ok());
        assert!(desk.check(&home.join(".ssh/config"), false).is_err());
        assert!(
            desk.check(&f.paths.config_dir.join("node.json"), true)
                .is_err()
        );
        assert!(desk.changes_startup("~/.config/hypr/hyprland.conf"));
        assert!(desk.changes_startup("~/code/../.bashrc"));
        assert!(!desk.changes_startup("~/code/main.rs"));
        // The Tessera program itself, whatever is allowed.
        if let Some(exe) = super::super::program_paths().first() {
            let wide = MachineScope {
                folders: vec![FolderAccess {
                    path: exe.parent().unwrap().display().to_string(),
                    write: true,
                }],
                desktop: true,
                ..Default::default()
            };
            assert!(
                Access::new(&f.paths, &wide, Some(home))
                    .check(exe, true)
                    .is_err()
            );
        }
    }

    /// A Mac set up with Full access before it was held back there: writes
    /// through the whole home folder and into ~/Library (in any letter
    /// case) are refused; chosen folders still work.
    #[test]
    fn a_mac_changes_only_chosen_folders() {
        let f = fixture();
        let home = &f.home;
        let mut full = access(&f, &home_rw(&f, false));
        full.mac = true;
        assert!(full.check(&home.join("code/x.rs"), true).is_err());
        assert!(full.check(&home.join("code/x.rs"), false).is_ok());
        let code = home.join("code");
        let mut chosen = access(
            &f,
            &MachineScope {
                folders: vec![
                    FolderAccess {
                        path: code.display().to_string(),
                        write: true,
                    },
                    FolderAccess {
                        path: home.display().to_string(),
                        write: false,
                    },
                ],
                ..Default::default()
            },
        );
        chosen.mac = true;
        assert!(chosen.check(&code.join("x.rs"), true).is_ok());
        let mut lib = access(
            &f,
            &MachineScope {
                folders: vec![FolderAccess {
                    path: home.join("library").display().to_string(),
                    write: true,
                }],
                ..Default::default()
            },
        );
        lib.mac = true;
        assert!(
            lib.check(&home.join("library/LaunchAgents/x.plist"), true)
                .is_err()
        );
    }

    #[test]
    fn commands_may_change_the_home_folder_around_startup_files() {
        let f = fixture();
        let w = access(&f, &home_rw(&f, false)).writable();
        let has = |p: &Path| w.iter().any(|x| x == p);
        assert!(
            !has(&f.home),
            "the home folder itself isn't writable: {w:?}"
        );
        assert!(has(&f.rw) && has(&f.ro), "its folders are: {w:?}");
        assert!(has(&f.home.join(".config/app")), "{w:?}");
        assert!(
            !w.iter().any(|x| x.starts_with(f.home.join(".config/hypr"))),
            "{w:?}"
        );
        assert!(
            !has(&f.home.join(".config")),
            "a folder holding a startup folder isn't: {w:?}"
        );
        assert!(!has(&f.home.join(".bashrc")), "{w:?}");
        // With desktop control, the home folder is writable as a whole.
        let w = access(&f, &home_rw(&f, true)).writable();
        assert!(w.contains(&f.home), "{w:?}");
        // Links are never opened up (they could lead anywhere).
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&f.outside, f.home.join("elsewhere")).unwrap();
            let w = access(&f, &home_rw(&f, false)).writable();
            assert!(!w.iter().any(|x| x.ends_with("elsewhere")), "{w:?}");
        }
    }

    #[test]
    fn commands_lose_credentials_from_their_environment() {
        for name in [
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "SSH_AUTH_SOCK",
            "TESSERA_HOME",
            "OPENAI_API_KEY",
            "DBUS_SESSION_BUS_ADDRESS",
            "MY_PASSWORD",
            "WAYLAND_DISPLAY",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "SWAYSOCK",
        ] {
            assert!(withheld(name), "{name}");
        }
        for name in ["PATH", "HOME", "CARGO_HOME", "LANG", "RUSTUP_HOME", "USER"] {
            assert!(!withheld(name), "{name}");
        }
    }

    #[test]
    fn the_desktop_session_is_passed_only_with_desktop_control() {
        let f = fixture();
        let tmp = f.home.join("tmp");
        let plain = command_env(&tmp, None, None);
        let names = |e: &Env| {
            e.iter()
                .map(|(k, _)| k.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        };
        for v in [
            "WAYLAND_DISPLAY",
            "DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "XDG_RUNTIME_DIR",
        ] {
            assert!(
                !names(&plain).contains(&v.to_string()),
                "{v} without desktop control"
            );
        }
        let session = super::super::desktop::Session {
            vars: vec![
                ("WAYLAND_DISPLAY".into(), "wayland-9".into()),
                ("XDG_RUNTIME_DIR".into(), "/run/user/1".into()),
            ],
            keep: vec![PathBuf::from("/run/user/1/wayland-9")],
            bus: Some("unix:path=/run/user/1/bus".into()),
            label: "Test".into(),
        };
        let env = command_env(&tmp, Some(&session), Some(&tmp.join("bus")));
        let get = |k: &str| {
            env.iter()
                .rev()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.to_string_lossy().to_string())
        };
        assert_eq!(get("WAYLAND_DISPLAY").as_deref(), Some("wayland-9"));
        assert_eq!(
            get("DBUS_SESSION_BUS_ADDRESS"),
            Some(format!("unix:path={}", tmp.join("bus").display())),
            "only the filtered bus"
        );
        let spec = command_spec(
            &f.paths,
            &access(&f, &home_rw(&f, true)),
            &tmp,
            Some(&session),
        );
        assert_eq!(spec.keep, session.keep);
        assert!(
            spec.hide.iter().any(|h| h.ends_with(".X11-unix")),
            "the session's sockets stay hidden otherwise"
        );
        let spec = command_spec(&f.paths, &access(&f, &home_rw(&f, false)), &tmp, None);
        assert!(spec.keep.is_empty());
    }

    /// Other programs' credentials in single files read as empty in
    /// commands, as file actions refuse them, unless their own folder was
    /// allowed (never the home folder).
    #[test]
    fn single_credential_files_are_hidden_from_commands() {
        let f = fixture();
        let tmp = f.home.join("tmp");
        let spec = command_spec(&f.paths, &access(&f, &home_rw(&f, false)), &tmp, None);
        for name in [
            ".netrc",
            ".git-credentials",
            ".pgpass",
            ".npmrc",
            ".pypirc",
            ".Xauthority",
        ] {
            assert!(spec.hide.contains(&f.home.join(name)), "{name}");
        }
        assert!(spec.hide.contains(&f.home.join(".cargo/credentials.toml")));
        let cargo = f.home.join(".cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        let own = MachineScope {
            folders: vec![FolderAccess {
                path: cargo.display().to_string(),
                write: true,
            }],
            commands: true,
            ..Default::default()
        };
        let spec = command_spec(&f.paths, &access(&f, &own), &tmp, None);
        assert!(!spec.hide.contains(&cargo.join("credentials.toml")));
        assert!(spec.hide.contains(&f.home.join(".netrc")));
    }
}
