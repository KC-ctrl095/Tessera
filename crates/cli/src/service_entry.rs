//! The startup entries Tessera writes on Windows (see `service_windows.rs`):
//! their names and command lines, written and read back. Plain text work,
//! so it is tested on every system.

use std::path::{Path, PathBuf};

use tessera_core::platform::{quote_windows_arg, split_windows_args};

/// The server's entry name (`tessera service install`), as in `service.rs`.
pub const DEFAULT_NAME: &str = "tessera";
/// The machine's entry (`tessera node service install`).
pub const NODE_NAME: &str = "tessera-node";

/// The startup entry's name as people see it in the registry.
pub fn entry_name(name: &str) -> String {
    match name {
        DEFAULT_NAME => "Tessera".into(),
        NODE_NAME => "Tessera machine".into(),
        other => format!("Tessera ({other})"),
    }
}

/// The startup entry's command line.
pub fn command_line(launcher: &Path, log: &Path, home: Option<&Path>, args: &[String]) -> String {
    let mut parts = vec![
        quote_windows_arg(&launcher.display().to_string()),
        "--log".into(),
        quote_windows_arg(&log.display().to_string()),
    ];
    if let Some(h) = home {
        parts.push("--home".into());
        parts.push(quote_windows_arg(&h.display().to_string()));
    }
    parts.extend(args.iter().map(|a| quote_windows_arg(a)));
    parts.join(" ")
}

/// What an entry's command line holds: its log, `--home` and Tessera's own
/// arguments.
pub struct Entry {
    pub log: PathBuf,
    pub home: Option<PathBuf>,
    pub args: Vec<String>,
}

pub fn parse_entry(line: &str) -> Option<Entry> {
    let mut it = split_windows_args(line).into_iter().skip(1);
    let (mut log, mut home) = (None, None);
    let mut args = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--log" => log = it.next().map(PathBuf::from),
            "--home" => home = it.next().map(PathBuf::from),
            _ => {
                args.push(a);
                args.extend(it.by_ref());
            }
        }
    }
    Some(Entry {
        log: log?,
        home,
        args,
    })
}

pub fn bind_of(args: &[String]) -> Option<String> {
    args.iter()
        .position(|a| a == "--bind")
        .and_then(|i| args.get(i + 1).cloned())
}

/// Whether a server on Windows keeps the computer awake while it is
/// plugged in: when `tessera-service.exe` runs it (`TESSERA_BACKGROUND=1`),
/// unless `server.keep_awake` is off or the desktop app started it only
/// for as long as the user wants (`TESSERA_KEEP_AWAKE=0`).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn keeps_awake(background: Option<&str>, keep_awake: Option<&str>, setting: bool) -> bool {
    background == Some("1") && keep_awake != Some("0") && setting
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_read_back_what_was_written() {
        let line = command_line(
            Path::new(r"C:\Users\Ann Lee\AppData\Local\Programs\Tessera\tessera-service.exe"),
            Path::new(r"C:\Users\Ann Lee\AppData\Roaming\tessera\logs\tessera.log"),
            Some(Path::new(r"D:\tessera data")),
            &["serve".into(), "--bind".into(), "127.0.0.1:7878".into()],
        );
        assert!(line.starts_with(
            r#""C:\Users\Ann Lee\AppData\Local\Programs\Tessera\tessera-service.exe" --log "#
        ));
        let e = parse_entry(&line).unwrap();
        assert_eq!(
            e.log,
            PathBuf::from(r"C:\Users\Ann Lee\AppData\Roaming\tessera\logs\tessera.log")
        );
        assert_eq!(e.home, Some(PathBuf::from(r"D:\tessera data")));
        assert_eq!(e.args, vec!["serve", "--bind", "127.0.0.1:7878"]);
        assert_eq!(bind_of(&e.args).as_deref(), Some("127.0.0.1:7878"));
        assert!(parse_entry(r#""C:\x\tessera-service.exe" serve"#).is_none());
    }

    #[test]
    fn only_a_background_server_keeps_the_computer_awake() {
        assert!(keeps_awake(Some("1"), None, true));
        assert!(!keeps_awake(None, None, true), "started by hand");
        assert!(!keeps_awake(Some("1"), None, false), "turned off");
        assert!(
            !keeps_awake(Some("1"), Some("0"), true),
            "the app runs it only while wanted"
        );
    }

    #[test]
    fn entry_names() {
        assert_eq!(entry_name(DEFAULT_NAME), "Tessera");
        assert_eq!(entry_name(NODE_NAME), "Tessera machine");
        assert_eq!(entry_name("work"), "Tessera (work)");
    }
}
