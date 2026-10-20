//! Finding and launching the browser: an installed Chromium-family browser,
//! driven only through a pipe (`--remote-debugging-pipe`), never a port.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::cdp::{Cdp, CdpEvent};

/// Browsers tried in order when `[browser] path` is not set.
#[cfg(not(windows))]
const CANDIDATES: &[&str] = &[
    // Arch's `/usr/bin/chromium` is a launcher that adds the user's
    // `chromium-flags.conf`; the real binary takes only our flags.
    "/usr/lib/chromium/chromium",
    "chromium",
    "chromium-browser",
    "google-chrome-stable",
    "google-chrome",
    "brave-browser",
    "brave",
    "microsoft-edge-stable",
    "microsoft-edge",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

/// Windows: where Chrome, Edge (on every Windows 10 and 11), Brave and
/// Chromium install, per machine or per user (relative to the folder named
/// by the variable).
#[cfg(any(windows, test))]
const WINDOWS_CANDIDATES: &[(&str, &str)] = &[
    ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
    ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
    ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
    (
        "ProgramFiles(x86)",
        r"Microsoft\Edge\Application\msedge.exe",
    ),
    ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
    (
        "ProgramFiles",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
    ),
    (
        "LOCALAPPDATA",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
    ),
    ("LOCALAPPDATA", r"Chromium\Application\chrome.exe"),
];

/// The Windows browsers that exist, in order (`env` reads a variable).
#[cfg(any(windows, test))]
fn windows_candidates(env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    WINDOWS_CANDIDATES
        .iter()
        .filter_map(|(var, rel)| env(var).map(|base| PathBuf::from(format!("{base}\\{rel}"))))
        .collect()
}

/// The browser to run: the configured one, else the first installed, else
/// the one `scripts/install.sh` downloads when there is none.
pub fn find(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        return which(p);
    }
    #[cfg(windows)]
    {
        windows_candidates(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
            .into_iter()
            .find(|p| p.is_file())
            .or_else(|| ["chrome.exe", "msedge.exe"].iter().find_map(|c| which(c)))
    }
    #[cfg(not(windows))]
    CANDIDATES
        .iter()
        .filter_map(|c| which(c))
        .find(|p| !is_snap(p))
        .or_else(|| downloaded().filter(|p| p.is_file()))
}

/// A snap can't open the profile Tessera keeps in a hidden folder (Ubuntu's
/// `chromium-browser` is a script that starts one).
#[cfg_attr(windows, allow(dead_code))]
fn is_snap(p: &Path) -> bool {
    if std::fs::canonicalize(p).is_ok_and(|c| c.starts_with("/snap/")) {
        return true;
    }
    let mut head = [0u8; 4096];
    let n = std::fs::File::open(p)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    head[..n].starts_with(b"#!") && head[..n].windows(6).any(|w| w == b"/snap/")
}

/// Where `scripts/install.sh` puts Chrome for Testing (no sudo needed): a
/// link to the current version's binary.
#[cfg_attr(windows, allow(dead_code))]
pub fn downloaded() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("tessera-browser").join("chrome"))
}

/// "Chromium", "Google Chrome"… from the program's file name.
pub fn display_name(bin: &Path) -> &'static str {
    let n = bin
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if n.contains("chrome") {
        "Google Chrome"
    } else if n.contains("brave") {
        "Brave"
    } else if n.contains("edge") {
        "Microsoft Edge"
    } else {
        "Chromium"
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|c| c.is_file())
    })
}

/// The flags every launch uses. `--remote-debugging-pipe` is the only way
/// in; no flag may ever open a debugging port. `scale`: one render scale
/// for every tab (otherwise each tab's window gets a screen of its own
/// scale, see `render.rs`).
pub fn args(profile: &Path, headless: bool, scale: Option<f64>) -> Vec<String> {
    let mut a = vec![
        "--remote-debugging-pipe".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // Cookies are encrypted with a fixed key instead of the desktop
        // keyring, so a server never pops up a keyring prompt; the profile
        // directory itself is private (0700).
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        // A page's sound would play on the server's speakers.
        "--mute-audio".into(),
        // Nobody can answer a permission prompt in a remote view.
        "--deny-permission-prompts".into(),
        "--disable-notifications".into(),
        "--disable-sync".into(),
        // Built-in extensions with background pages cost memory for nothing.
        "--disable-component-extensions-with-background-pages".into(),
        "--hide-crash-restore-bubble".into(),
        "--noerrdialogs".into(),
        "--force-color-profile=srgb".into(),
        // Besides services nobody uses here: the browser's own toolbar and
        // address-bar pop-ups drawn as web pages, which nobody sees in a web
        // view (only pages are shown) and which took about 100 MB in two
        // renderers of their own (observed with Chrome 155); Chrome draws them
        // natively instead. Pages see no difference.
        "--disable-features=Translate,MediaRouter,DialMediaRouteProvider,OptimizationHints,WebUIOmniboxPopup,WebUIOmniboxAimPopup,WebUIReloadButton".into(),
        // No security keys or passkeys (WebAuth): nobody can plug a key into
        // a web view, so sites see a browser without them and offer their
        // other ways to sign in instead of waiting for a key
        // (`tab::NO_KEYS_JS` turns down the requests of sites that ask
        // without looking first). Tessera doesn't hide from sites that its
        // web views are automated: a site's verification check is completed
        // by the user in a visible browser ("Show on <machine>'s screen").
        "--disable-blink-features=WebAuth".into(),
        "--window-size=1280,800".into(),
    ];
    if headless {
        // Linux runs ordinary Chrome on Ozone's private, in-memory display,
        // not Chrome's headless browser mode. Its identity is Chrome's own;
        // no user display, clipboard or display server is needed.
        #[cfg(target_os = "linux")]
        a.push("--ozone-platform=headless".into());
        // That display has no OpenGL of its own: without this the graphics
        // process fails to start twice and pages get no WebGL at all (maps,
        // charts and some sign-in checks need it). Chrome's software
        // renderer for WebGL only, as its headless mode uses; pages are
        // still composited in software (fast enough without a GPU).
        #[cfg(target_os = "linux")]
        a.push("--use-angle=swiftshader-webgl".into());
        #[cfg(not(target_os = "linux"))]
        a.push("--headless".into());
        if let Some(s) = scale {
            a.push(format!("--force-device-scale-factor={s}"));
        }
    }
    // Covered windows still paint for the devices watching them.
    a.push("--disable-backgrounding-occluded-windows".into());
    a.push("--disable-renderer-backgrounding".into());
    a.push("--disable-background-timer-throttling".into());
    a.push("about:blank".into());
    a
}

/// A running browser process.
pub struct Process {
    pub cdp: Arc<Cdp>,
    pub child: Option<tokio::process::Child>,
    /// The last lines it printed (for errors when it fails to start or dies).
    pub log: Arc<Mutex<std::collections::VecDeque<String>>>,
    /// When it started.
    pub started: std::time::Instant,
    /// Moves into the detached reaper; never released on a wait timeout.
    pub(crate) lease: Option<ExitLease>,
    /// Keep the owned group handle even after Windows reaps the leader.
    #[cfg(windows)]
    job: Option<crate::platform::windows::TreeJob>,
}

impl Process {
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(tokio::process::Child::id)
    }

    /// Stop it: ask nicely, then kill its process group.
    pub async fn stop(self) {
        // Own the process in a task before any await, so caller cancellation
        // cannot drop its profile lease before confirmed exit.
        let _ = tokio::spawn(async move {
            let _ = self
                .cdp
                .call_timeout(
                    None,
                    "Browser.close",
                    serde_json::json!({}),
                    std::time::Duration::from_secs(2),
                )
                .await;
            self.reap(std::time::Duration::from_secs(3)).await
        })
        .await;
    }

    /// Its pipe closed without being asked to stop: how it ended. A browser
    /// that still runs after `grace` (only its pipe went) is killed, and
    /// says so.
    pub async fn reap(self, grace: std::time::Duration) -> String {
        tokio::spawn(async move { self.reap_inner(grace).await })
            .await
            .unwrap_or_else(|_| "its cleanup task stopped; exit not confirmed".into())
    }

    async fn reap_inner(mut self, grace: std::time::Duration) -> String {
        let exited = wait_exit(self.child.as_mut().expect("running child"), grace).await;
        self.kill();
        match self.exit_bundle().wait(std::time::Duration::from_secs(2)).await {
            Some(status) if exited => describe_exit(&status),
            Some(_) => "its connection closed while it still ran, so Tessera stopped it".into(),
            None => "Tessera killed its process group; exit confirmation timed out (profile still reserved)".into(),
        }
    }

    fn exit_bundle(
        &mut self,
    ) -> ExitBundle<
        impl std::future::Future<Output = std::process::ExitStatus> + Send + 'static + use<>,
        Option<ExitLease>,
    > {
        let child = self.child.take().expect("running child");
        let lease = self.lease.take();
        #[cfg(windows)]
        let job = self.job.take().expect("owned browser Job");
        ExitBundle::new(
            async move {
                let status = confirmed_exit(child).await;
                #[cfg(windows)]
                job.wait_empty().await;
                status
            },
            lease,
        )
    }

    pub fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid() {
            // SAFETY: plain syscall on our own child's process group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        // Windows: the browser's helper processes are in its Job Object.
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.end();
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.kill();
            drop(self.exit_bundle());
        }
    }
}

/// Owned exit bundle used by normal, failed-start and Drop reapers. Both
/// the exit future (child + optional TreeJob) and lease detach together.
struct ExitBundle<F, L>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
    L: Send + 'static,
{
    exit: Option<F>,
    lease: Option<L>,
}
impl<F, L> ExitBundle<F, L>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
    L: Send + 'static,
{
    fn new(exit: F, lease: L) -> Self {
        Self {
            exit: Some(exit),
            lease: Some(lease),
        }
    }
    async fn wait(mut self, bound: std::time::Duration) -> Option<F::Output> {
        let exit = self.exit.take().expect("exit bundle");
        let lease = self.lease.take().expect("exit lease");
        hold_until_exit(exit, lease, bound).await
    }
}
impl<F, L> Drop for ExitBundle<F, L>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
    L: Send + 'static,
{
    fn drop(&mut self) {
        if let Some(exit) = self.exit.take() {
            let lease = self.lease.take();
            tokio::spawn(async move {
                let _lease = lease;
                exit.await;
            });
        }
    }
}

/// A browser profile is reserved until its leader AND owned tree exit is confirmed.
/// Completion is signalled by the reaper, never by a timeout's caller.
pub(crate) struct ExitLease {
    _profile: Option<tokio::sync::OwnedMutexGuard<()>>,
    pub done: tokio_util::sync::CancellationToken,
}

impl ExitLease {
    pub(crate) fn new(
        profile: Option<tokio::sync::OwnedMutexGuard<()>>,
        done: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            _profile: profile,
            done,
        }
    }
}
impl Drop for ExitLease {
    fn drop(&mut self) {
        self.done.cancel();
    }
}

pub(crate) const STILL_CLOSING: &str = "the browser is still closing; its profile is reserved until exit is confirmed. Try again in a moment";
const ADMISSION_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// One admission bound for server, visible node and Windows/headless node.
/// Never spawn this acquisition: timeout or caller cancellation drops the
/// pending future, so it cannot later acquire a lease and launch a browser.
pub(crate) async fn bounded_admission<F: std::future::Future>(
    pending: F,
) -> Result<F::Output, String> {
    tokio::time::timeout(ADMISSION_WAIT, pending)
        .await
        .map_err(|_| STILL_CLOSING.into())
}
/// On timeout the entire waiter (including its lease) moves into a task.
/// `exit` must resolve only on confirmed process exit, not an I/O error.
pub(crate) async fn hold_until_exit<F, T>(
    exit: F,
    lease: T,
    wait: std::time::Duration,
) -> Option<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
    T: Send + 'static,
{
    let pending = tokio::spawn(async move {
        let _lease = lease;
        exit.await
    });
    match tokio::time::timeout(wait, pending).await {
        Ok(Ok(status)) => Some(status),
        // Dropping JoinHandle detaches, preserving both waiter and lease.
        _ => None,
    }
}

/// Relay EOF is not Chrome exit. Wait for confirmation through a grace,
/// then kill the owned tree and bound the post-kill wait. The detached
/// waiter retains the supplied profile/Job lease until exit AND kill are
/// confirmed. Channels make the exit-before-kill handshake explicit.
#[cfg(any(windows, test))]
pub(crate) async fn relay_shutdown<F, P, K, L>(exit: F, pipes: P, kill: K, lease: L) -> bool
where
    F: std::future::Future<Output = ()> + Send + 'static,
    P: std::future::Future<Output = ()>,
    K: FnOnce(),
    L: Send + 'static,
{
    let (exited, mut notice) = tokio::sync::oneshot::channel();
    let (cleanup, killed) = tokio::sync::oneshot::channel();
    let pending = tokio::spawn(async move {
        let _lease = lease;
        exit.await;
        let _ = exited.send(());
        let _ = killed.await;
    });
    let exited_first = tokio::select! {
        result = &mut notice => result.is_ok(),
        _ = pipes => false,
    };
    if !exited_first {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut notice).await;
    }
    kill();
    let _ = cleanup.send(());
    tokio::time::timeout(std::time::Duration::from_secs(2), pending)
        .await
        .is_ok_and(|result| result.is_ok())
    // Timeout drops only the JoinHandle, leaving child, lease and Job owned.
}
pub(crate) async fn confirmed_exit(mut child: tokio::process::Child) -> std::process::ExitStatus {
    loop {
        if let Ok(status) = child.wait().await {
            return status;
        }
        // Errors aren't confirmation. Fail closed and keep the profile held.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Observe a Unix child's exit without reaping: its zombie reserves the
/// process/group id until whole-group cleanup. Never signal a recycled id.
pub(crate) async fn wait_exit(
    child: &mut tokio::process::Child,
    grace: std::time::Duration,
) -> bool {
    #[cfg(unix)]
    {
        let until = std::time::Instant::now() + grace;
        loop {
            let Some(pid) = child.id() else { return true };
            // SAFETY: waitid writes a siginfo for our unreaped child only.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            #[cfg(target_os = "linux")]
            let exited_pid = unsafe { info.si_pid() };
            #[cfg(not(target_os = "linux"))]
            let exited_pid = info.si_pid;
            if rc == 0 && exited_pid != 0 {
                return true;
            }
            if std::time::Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    #[cfg(not(unix))]
    {
        // Windows helpers are in an owned Job Object, not a numeric group.
        tokio::time::timeout(grace, child.wait())
            .await
            .is_ok_and(|status| status.is_ok())
    }
}

/// The pipe ends of a browser started with `--remote-debugging-pipe`:
/// what it writes (fd 4) and what it reads (fd 3).
#[cfg(unix)]
pub(crate) type Pipes = (
    tokio::net::unix::pipe::Receiver,
    tokio::net::unix::pipe::Sender,
);

/// Start `cmd` with the debugging pipe on its fds 3 and 4, in a process
/// group of its own that dies with this process. `confine` runs in the
/// child just before the program (async-signal-safe calls only).
#[cfg(unix)]
pub(crate) fn spawn_piped(
    cmd: &mut tokio::process::Command,
    confine: Option<Box<dyn FnMut() -> std::io::Result<()> + Send + Sync>>,
) -> Result<(tokio::process::Child, Pipes), String> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// A pipe whose ends sit at fd 10 or above (so the child's `dup2` onto
    /// 3 and 4 never clobbers them) and close on exec.
    fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut out = Vec::new();
        for fd in fds {
            // SAFETY: fcntl on a descriptor we just created; the original
            // is closed right after.
            let high = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
            unsafe { libc::close(fd) };
            if high < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `high` is a fresh descriptor we own.
            out.push(unsafe { OwnedFd::from_raw_fd(high) });
        }
        let w = out.pop().expect("two ends");
        let r = out.pop().expect("two ends");
        Ok((r, w))
    }

    let (child_in, ours_out) = pipe().map_err(|e| format!("can't create a pipe: {e}"))?;
    let (ours_in, child_out) = pipe().map_err(|e| format!("can't create a pipe: {e}"))?;
    // Finish all fallible parent-side pipe setup before any child exists.
    // Otherwise a setup error could drop the child's profile lease before
    // the automatic kill/reaper has confirmed its exit.
    let reader = tokio::net::unix::pipe::Receiver::from_owned_fd(ours_in)
        .map_err(|e| format!("pipe: {e}"))?;
    let writer = tokio::net::unix::pipe::Sender::from_owned_fd(ours_out)
        .map_err(|e| format!("pipe: {e}"))?;
    let (cin, cout) = (child_in.as_raw_fd(), child_out.as_raw_fd());
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut confine = confine;
    // SAFETY: only async-signal-safe calls between fork and exec (and in
    // `confine`, by its contract).
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(cin, 3) < 0 || libc::dup2(cout, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Die with the server rather than linger with the user's logins.
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            if let Some(f) = confine.as_mut() {
                f()?;
            }
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("can't start the browser: {e}"))?;
    drop(child_in);
    drop(child_out);
    Ok((child, (reader, writer)))
}

/// Windows: the pipe ends are two named pipes (ours, asynchronous) whose
/// other ends the browser inherits and is told about with
/// `--remote-debugging-io-pipes=<it reads>,<it writes>`. Still no port.
#[cfg(windows)]
pub(crate) type Pipes = (
    tokio::net::windows::named_pipe::NamedPipeServer,
    tokio::net::windows::named_pipe::NamedPipeServer,
);

/// Start `cmd` with the debugging pipes, in a Job Object of its own and
/// without a window. Nothing can confine it on Windows (`confine` must be
/// `None`).
#[cfg(windows)]
pub(crate) fn spawn_piped(
    cmd: &mut tokio::process::Command,
    confine: Option<Box<dyn FnMut() -> std::io::Result<()> + Send + Sync>>,
) -> Result<(tokio::process::Child, Pipes), String> {
    use std::os::windows::io::AsRawHandle;
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};
    if confine.is_some() {
        return Err("the browser can't be confined on Windows".into());
    }
    let pipe_err = |e: std::io::Error| format!("can't create a pipe: {e}");
    // Random names, created here first and closed to other computers; one
    // instance each, so nobody else can connect once the browser's ends have.
    let id = crate::util::random_token(16);
    let to_browser = format!(r"\\.\pipe\tessera-cdp-{id}-in");
    let from_browser = format!(r"\\.\pipe\tessera-cdp-{id}-out");
    let writer = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .access_inbound(false)
        .access_outbound(true)
        .pipe_mode(PipeMode::Byte)
        .max_instances(1)
        .create(&to_browser)
        .map_err(pipe_err)?;
    let reader = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .access_inbound(true)
        .access_outbound(false)
        .pipe_mode(PipeMode::Byte)
        .max_instances(1)
        .create(&from_browser)
        .map_err(pipe_err)?;
    let child_in = std::fs::OpenOptions::new()
        .read(true)
        .open(&to_browser)
        .map_err(pipe_err)?;
    let child_out = std::fs::OpenOptions::new()
        .write(true)
        .open(&from_browser)
        .map_err(pipe_err)?;
    for h in [&child_in, &child_out] {
        crate::platform::windows::set_inheritable(h, true).map_err(pipe_err)?;
    }
    cmd.arg(format!(
        "--remote-debugging-io-pipes={},{}",
        child_in.as_raw_handle() as usize,
        child_out.as_raw_handle() as usize
    ))
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    let child = crate::platform::spawn(cmd, crate::platform::Start::Quiet)
        .map_err(|e| format!("can't start the browser: {e}"))?;
    // Our copies of its ends go; it holds its own.
    drop(child_in);
    drop(child_out);
    Ok((child, (reader, writer)))
}

/// Stderr is untrusted and can contain credentials unknown to the vault.
/// Keep a byte-bounded private tail; only constant error classes ever leave it.
const LOG_LINE: usize = 1024;
const LOG_TOTAL: usize = 16 * 1024;

pub(crate) fn keep_log(
    child: &mut tokio::process::Child,
) -> Arc<Mutex<std::collections::VecDeque<String>>> {
    let log = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(read_log(stderr, log.clone()));
    }
    log
}

async fn read_log(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    log: Arc<Mutex<std::collections::VecDeque<String>>>,
) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0; 2048];
    let mut line = Vec::with_capacity(LOG_LINE);
    let keep = |line: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(line).trim_end().to_string();
        line.clear();
        let mut tail = log.lock();
        // Lossy UTF-8 expands invalid bytes: enforce the byte budget after
        // decoding too, not just the input budget.
        let text = text[..crate::util::floor_char(&text, LOG_LINE.min(text.len()))].to_string();
        while !tail.is_empty()
            && (tail.len() >= 20
                || tail.iter().map(String::len).sum::<usize>() + text.len() > LOG_TOTAL)
        {
            tail.pop_front();
        }
        tail.push_back(text);
    };
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &chunk[..n] {
            if b == b'\n' {
                keep(&mut line);
            } else if line.len() < LOG_LINE {
                line.push(b);
            }
            // Excess bytes are drained without growing the allocation.
        }
    }
    if !line.is_empty() {
        keep(&mut line);
    }
}

/// Never emit arbitrary stderr, even after a general-purpose redactor:
/// OAuth codes, cookie values and signed/credential URLs may be unknown.
fn error_class(line: &str) -> &'static str {
    if line.contains("GPU process isn't usable") {
        "Chrome's GPU process was unusable"
    } else if line.to_ascii_lowercase().contains("out of memory") {
        "Chrome reported an out-of-memory error"
    } else if line.contains("No usable sandbox") {
        "Chrome had no usable sandbox"
    } else if line.contains("SingletonLock") || line.contains("ProcessSingleton") {
        "Chrome's profile was already in use"
    } else if line.contains("Received signal") {
        "Chrome reported a fatal signal"
    } else if line.contains("FATAL") || line.contains("Check failed") {
        "Chrome reported a fatal error (details withheld)"
    } else {
        "Chrome reported an error (details withheld)"
    }
}

pub(crate) fn why_failed(log: &Mutex<std::collections::VecDeque<String>>) -> Option<String> {
    log.lock()
        .iter()
        .rev()
        .find(|l| l.contains("ERROR") || l.contains("FATAL"))
        .map(|l| error_class(l).to_string())
}

pub(crate) fn last_words(log: &Mutex<std::collections::VecDeque<String>>) -> Option<String> {
    let log = log.lock();
    let line = log
        .iter()
        .rev()
        .find(|l| {
            [
                "FATAL",
                "Check failed",
                "Received signal",
                "Goodbye",
                "out of memory",
            ]
            .iter()
            .any(|w| l.contains(w))
        })
        .or_else(|| log.back())
        .filter(|l| !l.trim().is_empty())?;
    Some(error_class(line).to_string())
}

/// How a browser process ended, in plain words: "it crashed (signal 11,
/// SIGSEGV)", "it was killed (signal 9, SIGKILL)", "it exited with code 1".
pub(crate) fn describe_exit(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let (what, name) = match sig {
                libc::SIGKILL => ("was killed", "SIGKILL"),
                libc::SIGTERM => ("was told to stop", "SIGTERM"),
                libc::SIGINT => ("was told to stop", "SIGINT"),
                libc::SIGHUP => ("was told to stop", "SIGHUP"),
                libc::SIGSEGV => ("crashed", "SIGSEGV"),
                libc::SIGBUS => ("crashed", "SIGBUS"),
                libc::SIGILL => ("crashed", "SIGILL"),
                libc::SIGFPE => ("crashed", "SIGFPE"),
                libc::SIGABRT => ("crashed", "SIGABRT"),
                libc::SIGTRAP => ("crashed", "SIGTRAP"),
                _ => ("ended on a signal", ""),
            };
            let core = if status.core_dumped() {
                ", core dumped"
            } else {
                ""
            };
            return if name.is_empty() {
                format!("it {what} (signal {sig}{core})")
            } else {
                format!("it {what} (signal {sig}, {name}{core})")
            };
        }
    }
    match status.code() {
        Some(0) => "it exited by itself (code 0)".into(),
        Some(c) => format!("it exited with code {c}"),
        None => "it ended".into(),
    }
}

/// Start `bin` on `profile` and connect to it.
#[cfg(any(unix, windows))]
pub async fn launch(
    bin: &Path,
    profile: &Path,
    headless: bool,
    scale: Option<f64>,
    lease: ExitLease,
) -> Result<(Process, mpsc::UnboundedReceiver<CdpEvent>), String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args(profile, headless, scale));
    let (mut child, (reader, writer)) = spawn_piped(&mut cmd, None)
        .map_err(|e| e.replace("the browser", &bin.display().to_string()))?;
    #[cfg(windows)]
    let job = match child.id().and_then(crate::platform::windows::take_job) {
        Some(job) => job,
        None => {
            let _ = child.start_kill();
            let _ = hold_until_exit(
                async move {
                    confirmed_exit(child).await;
                    // Without an owned Job there is no way to confirm the
                    // helpers are gone. Fail closed; never recycle this profile.
                    std::future::pending::<()>().await;
                },
                lease,
                std::time::Duration::from_secs(2),
            )
            .await;
            return Err(
                "Chrome couldn't start in its owned process group; its profile is still reserved"
                    .into(),
            );
        }
    };
    let log = keep_log(&mut child);
    let (cdp, events) = Cdp::start(reader, writer);
    let mut process = Process {
        cdp,
        child: Some(child),
        log,
        started: std::time::Instant::now(),
        lease: Some(lease),
        #[cfg(windows)]
        job: Some(job),
    };
    // It answers within a second or two when it started fine.
    let probe = process
        .cdp
        .call_timeout(
            None,
            "Browser.getVersion",
            serde_json::json!({}),
            std::time::Duration::from_secs(20),
        )
        .await;
    if probe.is_err() {
        let why = why_failed(&process.log)
            .unwrap_or_else(|| "Chrome didn't answer the startup probe".into());
        process.kill();
        process.reap(std::time::Duration::ZERO).await;
        return Err(format!("{} didn't start: {why}", bin.display()));
    }
    Ok((process, events))
}

#[cfg(not(any(unix, windows)))]
pub async fn launch(
    _bin: &Path,
    _profile: &Path,
    _headless: bool,
    _scale: Option<f64>,
    _lease: ExitLease,
) -> Result<(Process, mpsc::UnboundedReceiver<CdpEvent>), String> {
    Err("the web view browser runs on Linux and macOS servers".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_looks_for_chrome_then_edge() {
        let env = |v: &str| match v {
            "ProgramFiles" => Some(r"C:\Program Files".to_string()),
            "ProgramFiles(x86)" => Some(r"C:\Program Files (x86)".to_string()),
            "LOCALAPPDATA" => Some(r"C:\Users\Ann\AppData\Local".to_string()),
            _ => None,
        };
        let all: Vec<String> = super::windows_candidates(env)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            all[0],
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        );
        let edge = all
            .iter()
            .position(|p| p.ends_with(r"\msedge.exe"))
            .unwrap();
        assert!(edge > 2, "Chrome (machine and per-user) comes before Edge");
        assert_eq!(
            all[edge],
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
        );
        assert_eq!(
            super::display_name(std::path::Path::new(&all[edge])),
            "Microsoft Edge"
        );
    }

    #[test]
    fn snaps_are_passed_over() {
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("chromium-browser");
        std::fs::write(&script, "#!/bin/sh\nexec /snap/bin/chromium \"$@\"\n").unwrap();
        assert!(super::is_snap(&script));
        let real = d.path().join("chrome");
        std::fs::write(&real, b"\x7fELF\x02\x01\x01 not a script /snap/").unwrap();
        assert!(!super::is_snap(&real));
        let wrapper = d.path().join("google-chrome");
        std::fs::write(
            &wrapper,
            "#!/bin/bash\nexec /opt/google/chrome/chrome \"$@\"\n",
        )
        .unwrap();
        assert!(!super::is_snap(&wrapper));
    }

    use super::*;

    #[test]
    fn launches_never_open_a_debugging_port() {
        for headless in [true, false] {
            let a = args(Path::new("/p"), headless, Some(2.0));
            assert!(a.contains(&"--remote-debugging-pipe".to_string()));
            assert!(!a.iter().any(|f| f.contains("remote-debugging-port")
                || f.contains("remote-debugging-address")
                || f == "--no-sandbox"));
            assert!(a.contains(&"--user-data-dir=/p".to_string()));
            assert_eq!(
                a.contains(&"--headless".to_string()),
                headless && !cfg!(target_os = "linux")
            );
            assert_eq!(
                a.contains(&"--ozone-platform=headless".to_string()),
                headless && cfg!(target_os = "linux")
            );
            assert!(!a.iter().any(|f| f.starts_with("--user-agent=")
                || f.contains("AutomationControlled")
                || f.contains("disable-web-security")
                || f.contains("ThirdPartyCookies")));
            // WebGL in software on Linux's private display, never the GPU
            // compositing that costs a weak server several cores.
            assert_eq!(
                a.contains(&"--use-angle=swiftshader-webgl".to_string()),
                headless && cfg!(target_os = "linux")
            );
            assert!(!a.iter().any(|f| f == "--use-angle=swiftshader"
                || f == "--enable-unsafe-swiftshader"
                || f == "--disable-gpu"));
        }
    }

    #[test]
    fn exits_are_told_apart() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let raw = |sig: i32| std::process::ExitStatus::from_raw(sig);
            assert_eq!(
                describe_exit(&raw(libc::SIGKILL)),
                "it was killed (signal 9, SIGKILL)"
            );
            assert_eq!(
                describe_exit(&raw(libc::SIGSEGV)),
                "it crashed (signal 11, SIGSEGV)"
            );
            assert_eq!(
                describe_exit(&raw(libc::SIGTRAP | 0x80)),
                "it crashed (signal 5, SIGTRAP, core dumped)"
            );
            assert_eq!(describe_exit(&raw(1 << 8)), "it exited with code 1");
            assert_eq!(describe_exit(&raw(0)), "it exited by itself (code 0)");
        }
        let log = Mutex::new(std::collections::VecDeque::from(
            [
                "[1:1:ERROR:gl_display.cc] eglInitialize failed",
                "[1:2:FATAL:gpu_data_manager_impl_private.cc] GPU process isn't usable. Goodbye.",
                "[1:3:ERROR:registration_request.cc] Registration response error message",
            ]
            .map(String::from),
        ));
        assert_eq!(
            last_words(&log).as_deref(),
            Some("Chrome's GPU process was unusable")
        );
        let secret = "unknown-never-stored-credential";
        for text in [
            format!(
                "[1:2:ERROR:oauth.cc] callback https://user:{secret}@x.invalid/{secret}?code={secret}&state={secret}#{secret}"
            ),
            format!("[1:2:FATAL:download.cc] https://s.invalid/{secret}?X-Amz-Signature={secret}"),
            format!("[1:2:ERROR:cookie.cc] Cookie: session={secret}; auth={secret}"),
            format!("[1:2:FATAL:http.cc] Set-Cookie: {secret}; Authorization: Bearer {secret}"),
            format!("[1:2:FATAL:gpu.cc] GPU process isn't usable. Goodbye. {secret}"),
        ] {
            let log = Mutex::new(std::collections::VecDeque::from([text]));
            for safe in [last_words(&log), why_failed(&log)].into_iter().flatten() {
                assert!(
                    !safe.contains(secret) && !safe.contains("https:") && !safe.contains("Cookie:"),
                    "{safe}"
                );
            }
        }
        assert_eq!(last_words(&Mutex::new(Default::default())), None);
    }

    #[tokio::test]
    async fn stderr_is_byte_bounded_even_without_newlines() {
        let log = Arc::new(Mutex::new(Default::default()));
        let raw = vec![b'x'; 2 * 1024 * 1024];
        read_log(raw.as_slice(), log.clone()).await;
        assert!(log.lock().iter().all(|l| l.len() <= LOG_LINE));
        assert_eq!(log.lock().len(), 1);
        let raw = format!("{}\n", "\u{fffd}".repeat(4096)).repeat(100);
        read_log(raw.as_bytes(), log.clone()).await;
        assert!(log.lock().iter().all(|l| l.len() <= LOG_LINE));
        assert!(log.lock().iter().map(String::len).sum::<usize>() <= LOG_TOTAL);
        assert!(log.lock().len() <= 20);
    }

    #[test]
    fn web_views_offer_no_security_keys() {
        let a = args(Path::new("/p"), true, None);
        // Chromium reads only the last of a repeated switch: one list.
        let blink: Vec<&String> = a
            .iter()
            .filter(|f| f.starts_with("--disable-blink-features="))
            .collect();
        assert_eq!(blink.len(), 1, "{blink:?}");
        assert!(blink[0].split(['=', ',']).any(|f| f == "WebAuth"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reaping_a_leader_also_kills_its_surviving_helper_and_stalled_exit_is_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("helper.pid");
        let child = tokio::process::Command::new("/bin/sh")
            .args([
                "-c",
                &format!("sleep 60 & echo $! > '{}'; exit 0", pidfile.display()),
            ])
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (cdp, _, _) = Cdp::channel();
        let p = Process {
            cdp,
            child: Some(child),
            log: Arc::new(Mutex::new(Default::default())),
            started: std::time::Instant::now(),
            lease: None,
        };
        assert!(
            p.reap(std::time::Duration::from_secs(1))
                .await
                .contains("code 0")
        );
        let pid = std::fs::read_to_string(pidfile).unwrap();
        for _ in 0..20 {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).ok();
            if stat.as_ref().is_none_or(|s| {
                s.rsplit_once(')')
                    .is_some_and(|(_, s)| s.trim_start().starts_with('Z'))
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).ok();
        assert!(
            stat.as_ref().is_none_or(|s| s
                .rsplit_once(')')
                .is_some_and(|(_, s)| s.trim_start().starts_with('Z'))),
            "helper must not survive leader"
        );
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (cdp, _, _) = Cdp::channel();
        let p = Process {
            cdp,
            child: Some(child),
            log: Arc::new(Mutex::new(Default::default())),
            started: std::time::Instant::now(),
            lease: None,
        };
        let how = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            p.reap(std::time::Duration::from_millis(20)),
        )
        .await
        .unwrap();
        assert!(how.contains("Tessera stopped it"), "{how}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_reaper_keeps_the_profile_and_completion_until_confirmed_exit() {
        // Mock an uninterruptible post-kill wait: no dangerous D-state or
        // FUSE/ptrace changes on the user's machine. The real child is owned
        // by the delayed reaper, and SIGKILL has already been requested.
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let done = tokio_util::sync::CancellationToken::new();
        let lease = ExitLease::new(Some(gate.clone().lock_owned().await), done.clone());
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        child.start_kill().unwrap();
        let (confirm, wait) = tokio::sync::oneshot::channel();
        let result = hold_until_exit(
            async move {
                let _ = wait.await;
                confirmed_exit(child).await
            },
            lease,
            std::time::Duration::from_millis(20),
        )
        .await;
        assert!(result.is_none());
        assert!(
            !done.is_cancelled(),
            "timeout cannot clear cleanup completion"
        );
        assert!(gate.try_lock().is_err(), "profile reserved after timeout");
        let g = gate.clone();
        let mut second_launch = tokio::spawn(async move {
            let _guard = g.lock_owned().await;
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second_launch)
                .await
                .is_err()
        );
        confirm.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), second_launch)
            .await
            .unwrap()
            .unwrap();
        assert!(done.is_cancelled() && gate.try_lock().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn shared_relay_eof_waits_for_cookie_write_and_exit_before_kill() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let done = tokio_util::sync::CancellationToken::new();
        let lease = ExitLease::new(Some(gate.clone().lock_owned().await), done.clone());
        let (eof, pipe) = tokio::sync::oneshot::channel();
        let (seen, eof_seen) = tokio::sync::oneshot::channel();
        let (confirm, exit) = tokio::sync::oneshot::channel();
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let kill = killed.clone();
        let relay = tokio::spawn(relay_shutdown(
            async move { exit.await.unwrap() },
            async move {
                pipe.await.unwrap();
                seen.send(()).unwrap();
            },
            move || kill.store(true, std::sync::atomic::Ordering::SeqCst),
            lease,
        ));
        eof.send(()).unwrap();
        eof_seen.await.unwrap(); // explicit EOF-before-exit handshake, no sleep
        assert!(!killed.load(std::sync::atomic::Ordering::SeqCst) && !done.is_cancelled());
        let tmp = tempfile::tempdir().unwrap();
        let cookie = tmp.path().join("persistent-cookie.fixture");
        std::fs::write(&cookie, "flushed after EOF").unwrap();
        confirm.send(()).unwrap();
        assert!(relay.await.unwrap());
        assert_eq!(
            std::fs::read_to_string(cookie).unwrap(),
            "flushed after EOF"
        );
        assert!(killed.load(std::sync::atomic::Ordering::SeqCst) && done.is_cancelled());
        assert!(gate.try_lock().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn shared_relay_postkill_timeout_detaches_but_keeps_profile_and_job_lease() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let done = tokio_util::sync::CancellationToken::new();
        let lease = ExitLease::new(Some(gate.clone().lock_owned().await), done.clone());
        let (eof, pipe) = tokio::sync::oneshot::channel();
        let (confirm, exit) = tokio::sync::oneshot::channel();
        let (kill, killed) = tokio::sync::oneshot::channel();
        struct Job(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Job {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let job_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let relay = tokio::spawn(relay_shutdown(
            async move { exit.await.unwrap() },
            async move { pipe.await.unwrap() },
            move || {
                kill.send(()).unwrap();
            },
            (lease, Job(job_gone.clone())),
        ));
        eof.send(()).unwrap();
        killed.await.unwrap(); // paused clock advances the5s grace, no real sleep
        assert!(!relay.await.unwrap(), "bounded foreground post-kill wait");
        assert!(
            !done.is_cancelled()
                && gate.try_lock().is_err()
                && !job_gone.load(std::sync::atomic::Ordering::SeqCst)
        );
        assert_eq!(
            bounded_admission(gate.clone().lock_owned())
                .await
                .err()
                .unwrap(),
            STILL_CLOSING
        );
        confirm.send(()).unwrap();
        done.cancelled().await;
        assert!(gate.try_lock().is_ok() && job_gone.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn admission_timeout_and_cancellation_leave_no_late_launch_or_released_lease() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let done = tokio_util::sync::CancellationToken::new();
        let lease = ExitLease::new(Some(gate.clone().lock_owned().await), done.clone());
        let (confirm, exit) = tokio::sync::oneshot::channel::<()>();
        assert!(
            hold_until_exit(
                async move { exit.await.unwrap() },
                lease,
                std::time::Duration::from_secs(1)
            )
            .await
            .is_none()
        );
        let launched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pending = || {
            let gate = gate.clone();
            let launched = launched.clone();
            async move {
                let guard = gate.lock_owned().await;
                launched.store(true, std::sync::atomic::Ordering::SeqCst);
                guard
            }
        };
        assert_eq!(
            bounded_admission(pending()).await.err().unwrap(),
            STILL_CLOSING
        );
        assert!(!done.is_cancelled() && gate.try_lock().is_err());
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let g = gate.clone();
        let ran = launched.clone();
        let abandoned = tokio::spawn(async move {
            bounded_admission(async move {
                entered.send(()).unwrap();
                let guard = g.lock_owned().await;
                ran.store(true, std::sync::atomic::Ordering::SeqCst);
                guard
            })
            .await
        });
        waiting.await.unwrap();
        abandoned.abort();
        assert!(abandoned.await.unwrap_err().is_cancelled());
        confirm.send(()).unwrap();
        done.cancelled().await;
        assert!(gate.try_lock().is_ok());
        assert!(
            !launched.load(std::sync::atomic::Ordering::SeqCst),
            "dropped acquisitions never run later"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn whole_exit_bundle_keeps_mock_job_and_lease_on_timeout_and_drop() {
        for dropped in [false, true] {
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            let done = tokio_util::sync::CancellationToken::new();
            let lease = ExitLease::new(Some(gate.clone().lock_owned().await), done.clone());
            let (leader_exit, leader) = tokio::sync::oneshot::channel();
            let (observed, leader_seen) = tokio::sync::oneshot::channel();
            let (queries, mut query) = tokio::sync::mpsc::unbounded_channel::<Option<u32>>();
            let (seen, mut samples) = tokio::sync::mpsc::unbounded_channel();
            let job_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
            struct MockJob(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for MockJob {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let job = MockJob(job_gone.clone());
            let bundle = ExitBundle::new(
                async move {
                    leader.await.unwrap();
                    observed.send(()).unwrap();
                    while let Some(active) = query.recv().await {
                        seen.send(active).unwrap();
                        if active == Some(0) {
                            break;
                        }
                    }
                    drop(job);
                },
                lease,
            );
            let wait = if dropped {
                drop(bundle);
                None
            } else {
                Some(tokio::spawn(bundle.wait(std::time::Duration::from_secs(2))))
            };
            leader_exit.send(()).unwrap();
            leader_seen.await.unwrap();
            for active in [Some(1), None] {
                queries.send(active).unwrap();
                assert_eq!(samples.recv().await.unwrap(), active);
                assert!(
                    !done.is_cancelled()
                        && gate.try_lock().is_err()
                        && !job_gone.load(std::sync::atomic::Ordering::SeqCst),
                    "leader exit or failed Job query cannot release ownership"
                );
            }
            if let Some(wait) = wait {
                assert!(wait.await.unwrap().is_none(), "bounded foreground wait");
            }
            assert_eq!(
                bounded_admission(gate.clone().lock_owned())
                    .await
                    .err()
                    .unwrap(),
                STILL_CLOSING
            );
            queries.send(Some(0)).unwrap();
            done.cancelled().await;
            assert!(gate.try_lock().is_ok() && job_gone.load(std::sync::atomic::Ordering::SeqCst));
        }
    }
}
