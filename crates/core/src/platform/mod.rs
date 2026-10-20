//! What differs on Windows, in one place: the shell for commands, starting
//! programs without a console window, stopping a whole tree of processes,
//! asking a server to stop, plain paths. Linux and macOS behaviour stays
//! where it always was; the helpers here do nothing there unless they say
//! otherwise.

#[cfg(any(windows, test))]
pub(crate) mod atomic_write;
#[cfg(windows)]
pub mod private_file;
pub mod shell;
#[cfg(windows)]
pub mod windows;

use std::path::{Path, PathBuf};

/// The shell agent commands run in on this Windows computer, chosen once.
#[cfg(windows)]
pub fn shell() -> &'static shell::Shell {
    static S: std::sync::OnceLock<shell::Shell> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let s = shell::choose(
            &|k| std::env::var(k).ok().filter(|v| !v.is_empty()),
            &|p| Path::new(p).is_file(),
            &|name| find_program(name).map(|p| p.display().to_string()),
        );
        tracing::info!("commands run in {} ({})", s.name(), s.program);
        s
    })
}

/// What the model is told about the shell, on Windows only (the base
/// prompt elsewhere stays as it is).
pub fn shell_hint() -> &'static str {
    #[cfg(windows)]
    {
        shell().hint()
    }
    #[cfg(not(windows))]
    {
        ""
    }
}

/// Start a program without a console window of its own. A server started
/// at sign-in has no console, and every console program it starts would
/// otherwise open a window on the user's desktop. Nothing elsewhere.
pub fn no_window(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    cmd.creation_flags(windows::CREATE_NO_WINDOW);
    cmd
}

/// [`no_window`] for `std::process::Command`.
pub fn no_window_std(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows::CREATE_NO_WINDOW);
    }
    cmd
}

/// Agent work on Windows: no window, a process group of its own and below
/// normal priority, so it can't starve the server (what `setsid` and
/// `nice` do on Linux, where the callers' `pre_exec` does it instead).
pub fn agent_process(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    cmd.creation_flags(
        windows::CREATE_NO_WINDOW
            | windows::CREATE_NEW_PROCESS_GROUP
            | windows::BELOW_NORMAL_PRIORITY_CLASS,
    );
    cmd
}

/// How a child process starts on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Agent work: as [`agent_process`] says.
    Agent,
    /// A helper (a browser, cloudflared): without a window, and it ends
    /// with this process however this process ends, so a crashed or stuck
    /// server leaves nothing behind that would get in the next one's way.
    Quiet,
}

/// Whether a Windows computer is kept awake for a server running in the
/// background, by the power line status Windows reports: 0 on battery, 1
/// plugged in, 255 unknown (some desktops; they have no battery to save).
pub fn stays_awake(ac_line_status: u8) -> bool {
    ac_line_status != 0
}

/// Retry a failed keep-awake request on the next poll. A failure never
/// changes the last state Windows successfully accepted.
pub fn update_awake(
    current: &mut Option<bool>,
    want: bool,
    request: impl FnOnce() -> bool,
) -> Option<bool> {
    if *current == Some(want) {
        return None;
    }
    let ok = request();
    if ok {
        *current = Some(want);
    }
    Some(ok)
}

/// Start `cmd` so that stopping it later reaches everything it started.
/// On Windows: suspended, put in a Job Object of its own, then let run, so
/// nothing it starts can slip out before the job holds it (`kill_group`
/// then ends the job). Elsewhere just `spawn`: process groups do that job.
pub fn spawn(
    cmd: &mut tokio::process::Command,
    start: Start,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(windows)]
    {
        let flags = match start {
            Start::Agent => {
                windows::CREATE_NO_WINDOW
                    | windows::CREATE_NEW_PROCESS_GROUP
                    | windows::BELOW_NORMAL_PRIORITY_CLASS
            }
            Start::Quiet => windows::CREATE_NO_WINDOW,
        };
        let job = match start {
            Start::Agent => windows::Job::Agent,
            Start::Quiet => windows::Job::Helper,
        };
        windows::spawn_in_job(cmd, flags, job)
    }
    #[cfg(not(windows))]
    {
        let _ = start;
        cmd.spawn()
    }
}

/// Interpret LockFileEx without mistaking permission or disk errors for
/// another server. ERROR_LOCK_VIOLATION is the only contention result.
pub fn windows_lock_result(result: std::io::Result<()>) -> std::io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(e) if e.raw_os_error() == Some(33) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Compare Windows policy paths across slash and extended-length spellings.
/// Never use this result for filesystem operations: stripping the namespace
/// can change DOS names and long-path behavior. Case folding is conservative
/// for protected roots; callers must not fold allowed exceptions.
pub fn windows_policy_path(path: &str, fold_case: bool) -> String {
    let mut path = path.replace('\\', "/");
    if path
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("//?/UNC/") || p.eq_ignore_ascii_case("//./UNC/"))
    {
        path = format!("//{}", &path[8..]);
    } else if path.starts_with("//?/") || path.starts_with("//./") {
        path = path[4..].to_string();
    }
    if fold_case { path.to_lowercase() } else { path }
}

/// Explicit program paths use either separator on Windows. A drive-relative
/// path (`C:tools\ls.exe`) is still a path, not a name to search on PATH.
pub fn program_is_path(name: &str, windows: bool) -> bool {
    name.contains('/') || (windows && name.contains(['\\', ':']))
}

/// A program by name the way Windows finds it (`.exe`, `.cmd`, `.bat`… per
/// `PATHEXT`, through `PATH`): `npx` is `npx.cmd` there, which starting a
/// bare `npx` would miss. Elsewhere: the first match on `PATH`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if program_is_path(name, cfg!(windows)) || p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        windows_extensions(name, &pathext)
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|c| c.is_file())
    })
}

/// The suffixes to try for `name` on Windows: none if it already has one
/// of `PATHEXT`'s, else each of them (lowercase; Windows doesn't mind).
pub fn windows_extensions(name: &str, pathext: &str) -> Vec<String> {
    let exts: Vec<String> = pathext
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .collect();
    let lower = name.to_ascii_lowercase();
    if exts.iter().any(|e| lower.ends_with(e.as_str())) {
        vec![String::new()]
    } else {
        exts
    }
}

/// A Windows path without the `\\?\` prefix `canonicalize` adds, when the
/// plain form means the same file: programs (cmd above all) and people read
/// `C:\Users\…` but not `\\?\C:\Users\…`. Other paths stay as they are.
pub fn plain_windows_path(p: &str) -> Option<String> {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        let plain = format!(r"\\{rest}");
        return safe_plain(&plain[2..]).then_some(plain);
    }
    let rest = p.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    (drive && safe_plain(&rest[3..])).then(|| rest.to_string())
}

/// Whether the parts of a path keep their meaning without `\\?\`: short
/// enough, no device names, no names ending in a dot or space (which Win32
/// would trim), no `.` or `..`.
fn safe_plain(rest: &str) -> bool {
    const DEVICES: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "conin$",
        "conout$",
    ];
    rest.len() < 250
        && rest.split('\\').filter(|c| !c.is_empty()).all(|c| {
            let stem = c
                .split('.')
                .next()
                .unwrap_or(c)
                .trim_end()
                .to_ascii_lowercase();
            c != "."
                && c != ".."
                && !c.ends_with('.')
                && !c.ends_with(' ')
                && !DEVICES.contains(&stem.as_str())
        })
}

/// Git Bash's spelling of a Windows path (`/c/Users/ann`) as Windows
/// writes it (`C:\Users\ann`); `None` for anything else.
pub fn git_bash_path(p: &str) -> Option<String> {
    let rest = p.strip_prefix('/')?;
    let mut chars = rest.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    let tail = chars.as_str();
    if !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    Some(format!(
        "{}:\\{}",
        drive.to_ascii_uppercase(),
        tail.trim_start_matches('/').replace('/', "\\")
    ))
}

/// `std::fs::canonicalize`, giving plain `C:\…` paths on Windows.
pub fn canonicalize(p: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    #[cfg(windows)]
    if let Some(plain) = c.to_str().and_then(plain_windows_path) {
        return Ok(PathBuf::from(plain));
    }
    Ok(c)
}

/// What follows the network share `remote` (`\\server\share`) in `path`,
/// compared without case, or `None` when `path` isn't on that share.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn unc_rest<'a>(path: &'a str, remote: &str) -> Option<&'a str> {
    let remote = remote.trim_end_matches('\\');
    if !remote.starts_with(r"\\") || remote.len() <= 2 {
        return None;
    }
    let head = path.get(..remote.len())?;
    let rest = &path[remote.len()..];
    (head.eq_ignore_ascii_case(remote) && (rest.is_empty() || rest.starts_with('\\')))
        .then(|| rest.trim_start_matches('\\'))
}

/// Quote one argument for a Windows command line the way the Microsoft C
/// runtime (and every program that parses its command line like it) reads
/// it back: unchanged unless it has spaces, tabs or quotes.
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
                continue;
            }
            _ => {}
        }
        if c != '\\' {
            out.extend(std::iter::repeat_n('\\', slashes));
            slashes = 0;
            out.push(c);
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

/// Split a Windows command line into arguments the way the Microsoft C
/// runtime does (for reading back the background entry Tessera wrote).
pub fn split_windows_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut have = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let start = i;
            while i < chars.len() && chars[i] == '\\' {
                i += 1;
            }
            let n = i - start;
            if i < chars.len() && chars[i] == '"' {
                cur.extend(std::iter::repeat_n('\\', n / 2));
                if n % 2 == 1 {
                    cur.push('"');
                    i += 1;
                }
            } else {
                cur.extend(std::iter::repeat_n('\\', n));
            }
            have = true;
            continue;
        }
        if c == '"' {
            // `""` inside quotes is a literal quote.
            if in_quotes && chars.get(i + 1) == Some(&'"') {
                cur.push('"');
                i += 2;
                continue;
            }
            in_quotes = !in_quotes;
            have = true;
            i += 1;
            continue;
        }
        if (c == ' ' || c == '\t') && !in_quotes {
            if have {
                args.push(std::mem::take(&mut cur));
                have = false;
            }
            i += 1;
            continue;
        }
        cur.push(c);
        have = true;
        i += 1;
    }
    if have {
        args.push(cur);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder on a network share maps onto a drive whose share it is
    /// (Windows' mapped drives), without regard to case, and only at a
    /// folder boundary.
    #[test]
    fn a_share_path_maps_to_the_rest_below_its_share() {
        let share = r"\\nas\Projects";
        assert_eq!(unc_rest(r"\\nas\Projects\app\src", share), Some(r"app\src"));
        assert_eq!(unc_rest(r"\\NAS\projects\app", share), Some("app"));
        assert_eq!(unc_rest(r"\\nas\Projects", share), Some(""));
        assert_eq!(
            unc_rest(r"\\nas\Projects\app", r"\\nas\Projects\"),
            Some("app")
        );
        assert_eq!(unc_rest(r"\\nas\ProjectsOld\app", share), None);
        assert_eq!(unc_rest(r"\\other\Projects\app", share), None);
        assert_eq!(unc_rest(r"C:\Projects\app", share), None);
        assert_eq!(unc_rest(r"\\nas\Projects\app", "Z:"), None);
        assert_eq!(unc_rest(r"\\n", share), None);
    }

    #[test]
    fn windows_program_paths_are_not_searched_as_bare_names() {
        for name in [
            r".\tools\ls.exe",
            r"C:\Tools\ls.exe",
            r"~\tools\ls.exe",
            "C:ls.exe",
            "tools/ls",
        ] {
            assert!(program_is_path(name, true), "{name}");
        }
        assert!(!program_is_path("rust-analyzer", true));
        assert!(!program_is_path(r"tools\ls", false));
        assert!(!program_is_path("my:tool", false));
        assert!(program_is_path("./tools/ls", false));
    }

    #[test]
    fn failed_keep_awake_requests_retry_without_claiming_success() {
        let mut state = None;
        assert_eq!(update_awake(&mut state, true, || false), Some(false));
        assert_eq!(state, None);
        assert_eq!(update_awake(&mut state, true, || true), Some(true));
        assert_eq!(state, Some(true));
        assert_eq!(
            update_awake(&mut state, true, || panic!("already set")),
            None
        );
        assert_eq!(update_awake(&mut state, false, || false), Some(false));
        assert_eq!(state, Some(true));
        assert_eq!(update_awake(&mut state, false, || true), Some(true));
        assert_eq!(state, Some(false));
    }

    #[test]
    fn windows_lock_errors_are_not_contention() {
        assert!(windows_lock_result(Ok(())).unwrap());
        assert!(!windows_lock_result(Err(std::io::Error::from_raw_os_error(33))).unwrap());
        for code in [5, 6, 50, 112, 1117] {
            assert_eq!(
                windows_lock_result(Err(std::io::Error::from_raw_os_error(code)))
                    .unwrap_err()
                    .raw_os_error(),
                Some(code)
            );
        }
        assert!(windows_lock_result(Err(std::io::Error::other("lock failed"))).is_err());
    }

    #[test]
    fn a_server_keeps_windows_awake_unless_on_battery() {
        assert!(!stays_awake(0), "on battery: Windows sleeps as usual");
        assert!(stays_awake(1), "plugged in");
        assert!(stays_awake(255), "unknown (a desktop without a battery)");
    }

    #[test]
    fn verbatim_prefixes_come_off_only_when_safe() {
        assert_eq!(
            plain_windows_path(r"\\?\C:\Users\Ann Lee\project").as_deref(),
            Some(r"C:\Users\Ann Lee\project")
        );
        assert_eq!(
            plain_windows_path(r"\\?\UNC\nas\share\docs").as_deref(),
            Some(r"\\nas\share\docs")
        );
        assert_eq!(plain_windows_path(r"C:\already\plain"), None);
        // Names Win32 would read differently keep the prefix.
        assert_eq!(plain_windows_path(r"\\?\C:\x\nul.txt"), None);
        assert_eq!(plain_windows_path(r"\\?\C:\x\trailing."), None);
        assert_eq!(plain_windows_path(r"\\?\C:\x\space "), None);
        assert_eq!(
            plain_windows_path(&format!(r"\\?\C:\{}", "a".repeat(300))),
            None
        );
        // Volume GUID paths aren't drive paths.
        assert_eq!(plain_windows_path(r"\\?\Volume{0a1b}\folder"), None);
    }

    #[test]
    fn git_bash_paths_become_windows_paths() {
        assert_eq!(
            git_bash_path("/c/Users/Ann Lee/x.txt").as_deref(),
            Some(r"C:\Users\Ann Lee\x.txt")
        );
        assert_eq!(git_bash_path("/d").as_deref(), Some(r"D:\"));
        assert_eq!(git_bash_path("/d/").as_deref(), Some(r"D:\"));
        assert_eq!(git_bash_path("/home/ann"), None);
        assert_eq!(git_bash_path("/tmp"), None);
        assert_eq!(git_bash_path("c/x"), None);
        assert_eq!(git_bash_path(r"C:\x"), None);
    }

    #[test]
    fn extensions_follow_pathext() {
        let pathext = ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.PS1";
        assert_eq!(
            windows_extensions("npx", pathext),
            vec![".com", ".exe", ".bat", ".cmd", ".vbs", ".js", ".ps1"]
        );
        assert_eq!(windows_extensions("npx.cmd", pathext), vec![""]);
        assert_eq!(windows_extensions("Claude.EXE", pathext), vec![""]);
        assert_eq!(windows_extensions("x", ";;bad;.EXE"), vec![".exe"]);
    }

    #[test]
    fn windows_arguments_survive_a_round_trip() {
        let cases = [
            "plain",
            "",
            r"C:\Users\Ann Lee\AppData\Local\Programs\Tessera\tessera.exe",
            r#"say "hi""#,
            r"trailing\",
            r"two\\",
            r#"quote\"inside"#,
            "tab\there",
            r"\\server\share\x y",
        ];
        for c in cases {
            let line = format!("prog {} end", quote_windows_arg(c));
            let back = split_windows_args(&line);
            assert_eq!(back, vec!["prog", c, "end"], "{c:?} → {line}");
        }
        assert_eq!(quote_windows_arg("serve"), "serve");
        assert_eq!(
            quote_windows_arg(r"C:\Program Files\x"),
            r#""C:\Program Files\x""#
        );
        assert_eq!(quote_windows_arg(r"a b\"), r#""a b\\""#);
    }

    #[test]
    fn command_lines_split_like_windows_does() {
        assert_eq!(
            split_windows_args(
                r#""C:\Program Files\T\tessera-service.exe" --log "C:\a b\server.log" serve --bind 127.0.0.1:7878"#
            ),
            vec![
                r"C:\Program Files\T\tessera-service.exe",
                "--log",
                r"C:\a b\server.log",
                "serve",
                "--bind",
                "127.0.0.1:7878"
            ]
        );
        assert_eq!(split_windows_args(r#"a "" b"#), vec!["a", "", "b"]);
        assert_eq!(split_windows_args(r#""a""b""#), vec![r#"a"b"#]);
    }
}
