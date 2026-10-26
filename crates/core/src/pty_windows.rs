//! Windows terminal shell policy, shared with Linux-run unit tests.
//! ConPTY itself lives in `pty_windows/windows.rs` (no terminal dependency).
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(windows)]
#[path = "pty_windows/windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::{Account, Child, Pty, Spawned, Start, account, spawn};

pub fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(2, 1000), rows.clamp(1, 500))
}

/// PowerShell cmdlets may run entirely inside the shell process. ConPTY has
/// no trustworthy prompt signal, so a process count cannot establish idleness.
fn prompt_state() -> Option<bool> {
    None
}

/// Prefer PowerShell 7 on PATH, then Windows PowerShell, then cmd.
fn choose_shell(mut find: impl FnMut(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    ["pwsh.exe", "powershell.exe", "cmd.exe"]
        .into_iter()
        .find_map(&mut find)
}

fn shell_start(shell: &Path) -> (OsString, Vec<OsString>) {
    let name = shell
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let args: &[&str] = if name == "cmd.exe" {
        &["/d", "/k", "chcp 65001 >nul"]
    } else {
        &[
            "-NoLogo",
            "-NoExit",
            "-Command",
            "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding",
        ]
    };
    (
        shell.as_os_str().to_owned(),
        args.iter().map(OsString::from).collect(),
    )
}

fn withheld(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("TESSERA")
        || [
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
        ]
        .iter()
        .any(|word| upper.contains(word))
}

/// Keep the account's ordinary environment, never the service's credentials.
fn environment_from(
    vars: impl Iterator<Item = (OsString, OsString)>,
    home: &Path,
    shell: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<_> = vars
        .filter(|(k, _)| {
            let k = k.to_string_lossy().to_ascii_uppercase();
            !withheld(&k)
                && [
                    "PATH",
                    "PATHEXT",
                    "SYSTEMROOT",
                    "WINDIR",
                    "TEMP",
                    "TMP",
                    "APPDATA",
                    "LOCALAPPDATA",
                    "HOMEDRIVE",
                    "HOMEPATH",
                    "PROGRAMDATA",
                    "PROGRAMFILES",
                    "PROGRAMFILES(X86)",
                    "PROGRAMW6432",
                    "COMSPEC",
                    "PSMODULEPATH",
                    "USERNAME",
                    "USERDOMAIN",
                    "NUMBER_OF_PROCESSORS",
                    "PROCESSOR_ARCHITECTURE",
                    "OS",
                    "LANG",
                    "LC_ALL",
                ]
                .contains(&k.as_str())
        })
        .collect();
    for (key, value) in [
        ("HOME", home.as_os_str()),
        ("USERPROFILE", home.as_os_str()),
        ("SHELL", shell.as_os_str()),
        ("TERM", "xterm-256color".as_ref()),
        ("COLORTERM", "truecolor".as_ref()),
    ] {
        env.push((key.into(), value.to_owned()));
    }
    env.sort_by_key(|(k, _)| k.to_string_lossy().to_ascii_uppercase());
    env.dedup_by(|(a, _), (b, _)| {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    });
    env
}

/// CRT quoting without losing non-Unicode Windows path characters.
fn quote_wide(arg: &[u16]) -> Vec<u16> {
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for &c in arg {
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            b'\\' as u16,
            if c == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_process_cmdlets_cannot_be_mistaken_for_an_idle_prompt() {
        assert_eq!(prompt_state(), None);
    }

    #[test]
    fn shell_preference_is_fixed_and_does_not_use_comspec() {
        for (available, expected) in [
            (
                vec!["pwsh.exe", "powershell.exe", "cmd.exe"],
                Some("pwsh.exe"),
            ),
            (vec!["powershell.exe", "cmd.exe"], Some("powershell.exe")),
            (vec!["cmd.exe"], Some("cmd.exe")),
            (vec![], None),
        ] {
            let got = choose_shell(|name| available.contains(&name).then(|| PathBuf::from(name)));
            assert_eq!(got, expected.map(PathBuf::from));
        }
    }

    #[test]
    fn shell_initialization_is_interactive_and_utf8() {
        for shell in ["pwsh.exe", "powershell.exe"] {
            let (program, args) = shell_start(Path::new(shell));
            assert_eq!(program, shell);
            assert!(args.contains(&OsString::from("-NoExit")));
            assert!(!args.contains(&OsString::from("-NoProfile")));
            assert!(!args.contains(&OsString::from("-NonInteractive")));
            assert!(
                args.last()
                    .unwrap()
                    .to_string_lossy()
                    .contains("UTF8Encoding")
            );
        }
        assert_eq!(
            shell_start(Path::new("cmd.exe")).1,
            ["/d", "/k", "chcp 65001 >nul"].map(OsString::from)
        );
    }

    #[test]
    fn sizes_have_the_same_bounds_as_unix() {
        assert_eq!(clamp_size(0, 0), (2, 1));
        assert_eq!(clamp_size(u16::MAX, u16::MAX), (1000, 500));
        assert_eq!(clamp_size(120, 40), (120, 40));
    }

    #[test]
    fn service_secrets_are_never_in_the_shell_environment() {
        let vars = [
            "Path",
            "SystemRoot",
            "temp",
            "tessera_home",
            "api_key",
            "Cloud_Token",
            "Password",
            "SERVER_SUDO",
            "DESKTOP_SUDO",
            "UNNAMED_CUSTOM_ENV",
            "HOME",
            "USERPROFILE",
            "TERM",
        ]
        .into_iter()
        .map(|s| (s.into(), OsString::from("parent")));
        let env = environment_from(vars, Path::new("home"), Path::new("pwsh.exe"));
        for (k, v) in &env {
            assert!(!withheld(&k.to_string_lossy()), "{k:?}");
            if ["HOME", "USERPROFILE"].contains(&k.to_string_lossy().as_ref()) {
                assert_eq!(v, "home");
            }
        }
        assert!(env.iter().all(|(k, _)| {
            !["SERVER_SUDO", "DESKTOP_SUDO", "UNNAMED_CUSTOM_ENV"]
                .contains(&k.to_string_lossy().as_ref())
        }));
        assert!(env.contains(&("Path".into(), "parent".into())));
        assert!(env.contains(&("TERM".into(), "xterm-256color".into())));
        assert!(env.contains(&("COLORTERM".into(), "truecolor".into())));
        assert!(env.contains(&("SHELL".into(), "pwsh.exe".into())));
    }

    #[test]
    fn command_line_arguments_round_trip_even_with_quotes_and_trailing_slashes() {
        for arg in [
            "",
            "space here",
            "plain",
            "unicode 日本",
            "tab\there",
            "say \"hello\"",
            "C:\\space here\\",
            "backslash\\\"quote",
        ] {
            let quoted =
                String::from_utf16(&quote_wide(&arg.encode_utf16().collect::<Vec<_>>())).unwrap();
            assert_eq!(crate::platform::split_windows_args(&quoted), vec![arg]);
        }
        assert!(quote_wide(&[0xd800]).contains(&0xd800));
    }
}
