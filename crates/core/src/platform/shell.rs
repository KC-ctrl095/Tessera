//! Which shell runs the agent's commands on Windows. Models know bash best,
//! so Git Bash when Git for Windows is installed, else PowerShell 7, else
//! Windows PowerShell 5.1 (part of every Windows 10 and 11), else cmd.
//! Linux and macOS keep using bash (or sh), chosen in `tools::bash`.
//!
//! The choice works on path strings, so it can be tested on any system.

/// The kinds of shell a Windows computer may run commands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    GitBash,
    Pwsh,
    PowerShell,
    Cmd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub kind: Kind,
    /// Full path of the program (or a bare name found on `PATH` at start).
    pub program: String,
}

/// Where the command text travels for Git Bash: in a variable, so no
/// quoting rule of the Windows command line can change it.
pub const COMMAND_VAR: &str = "TESSERA_COMMAND";

impl Shell {
    /// Its name, for the model and for people.
    pub fn name(&self) -> &'static str {
        match self.kind {
            Kind::GitBash => "Git Bash (bash)",
            Kind::Pwsh => "PowerShell 7 (pwsh)",
            Kind::PowerShell => "Windows PowerShell 5.1",
            Kind::Cmd => "cmd",
        }
    }

    /// One sentence for the `bash` tool's description: what to write.
    pub fn hint(&self) -> &'static str {
        match self.kind {
            Kind::GitBash => {
                " On this Windows computer commands run in Git Bash: Unix tools work; write paths as C:/Users/… or /c/Users/…."
            }
            Kind::Pwsh => {
                " On this Windows computer commands run in PowerShell 7, not bash: use PowerShell syntax (`;` or `&&` between commands, `$env:NAME`, Get-ChildItem)."
            }
            Kind::PowerShell => {
                " On this Windows computer commands run in Windows PowerShell 5.1, not bash: use PowerShell syntax (`;` between commands, never `&&`; `$env:NAME`; Get-ChildItem)."
            }
            Kind::Cmd => {
                " On this Windows computer commands run in cmd.exe, not bash: use cmd syntax (`&&` between commands, `%NAME%`, dir, type)."
            }
        }
    }

    /// What follows the program on the Windows command line (passed as is),
    /// and the variables the command needs.
    pub fn command_line(&self, command: &str) -> (String, Vec<(String, String)>) {
        match self.kind {
            // The only quotes are `\"`, which both the Microsoft C runtime
            // and the MSYS runtime read as a plain quote; the command itself
            // never passes through the command line.
            Kind::GitBash => (
                "-c \"__tessera=$TESSERA_COMMAND; unset TESSERA_COMMAND; eval \\\"$__tessera\\\"\""
                    .to_string(),
                vec![(COMMAND_VAR.to_string(), command.to_string())],
            ),
            Kind::Pwsh | Kind::PowerShell => (
                format!(
                    "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -EncodedCommand {}",
                    encode_powershell(&format!("{POWERSHELL_PRELUDE}\n{command}"))
                ),
                Vec::new(),
            ),
            // `/s` takes the outer quotes off and leaves the rest as typed.
            Kind::Cmd => (format!("/d /s /c \"{command}\""), Vec::new()),
        }
    }
}

/// Output as UTF-8 and no progress bars (they arrive as XML noise when the
/// output is a pipe), before the command itself.
const POWERSHELL_PRELUDE: &str = "$ProgressPreference='SilentlyContinue'; try { [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding } catch {}";

/// `-EncodedCommand` takes Base64 of the script's UTF-16LE text: nothing in
/// the script can be misread as quoting.
pub fn encode_powershell(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The shell for this Windows computer. `env` reads a variable, `exists`
/// tells whether a file is there, `on_path` finds a program on `PATH`.
/// `TESSERA_SHELL` (a path to bash.exe, pwsh.exe, powershell.exe or cmd.exe)
/// overrides the choice.
pub fn choose(
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
    on_path: &dyn Fn(&str) -> Option<String>,
) -> Shell {
    if let Some(p) = env("TESSERA_SHELL").filter(|p| !p.trim().is_empty() && exists(p.trim())) {
        let p = p.trim().to_string();
        if let Some(kind) = kind_of(&p) {
            return Shell { kind, program: p };
        }
    }
    if let Some(p) = git_bash(env, exists, on_path) {
        return Shell {
            kind: Kind::GitBash,
            program: p,
        };
    }
    let program_files = env("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into());
    let pwsh = on_path("pwsh.exe").or_else(|| {
        let p = join(&program_files, r"PowerShell\7\pwsh.exe");
        exists(&p).then_some(p)
    });
    if let Some(p) = pwsh {
        return Shell {
            kind: Kind::Pwsh,
            program: p,
        };
    }
    let system = env("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let ps = join(&system, r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if exists(&ps) {
        return Shell {
            kind: Kind::PowerShell,
            program: ps,
        };
    }
    if let Some(p) = on_path("powershell.exe") {
        return Shell {
            kind: Kind::PowerShell,
            program: p,
        };
    }
    let cmd = env("ComSpec")
        .filter(|p| exists(p))
        .unwrap_or_else(|| join(&system, r"System32\cmd.exe"));
    Shell {
        kind: Kind::Cmd,
        program: cmd,
    }
}

/// Claude Code needs Git Bash even when Tessera's command shell is
/// PowerShell. Honour Claude's own override; otherwise find custom Git
/// installs with the same lookup as Tessera, without touching sign-in files.
pub fn claude_git_bash(
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
    on_path: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    if env("CLAUDE_CODE_GIT_BASH_PATH").is_some() {
        return None;
    }
    git_bash(env, exists, on_path)
}

/// Git for Windows' `bin\bash.exe` (a launcher that puts its Unix tools on
/// `PATH`), found next to `git.exe` or where its installers put it. Never
/// `System32\bash.exe`: that one starts WSL, a different computer as far as
/// paths go.
fn git_bash(
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
    on_path: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut roots: Vec<String> = Vec::new();
    if let Some(git) = on_path("git.exe") {
        let lower = git.to_ascii_lowercase().replace('/', "\\");
        for tail in [r"\cmd\git.exe", r"\mingw64\bin\git.exe", r"\bin\git.exe"] {
            if lower.ends_with(tail) {
                roots.push(git[..git.len() - tail.len()].to_string());
                break;
            }
        }
    }
    for (var, rel) in [
        ("ProgramFiles", "Git"),
        ("ProgramW6432", "Git"),
        ("ProgramFiles(x86)", "Git"),
        ("LOCALAPPDATA", r"Programs\Git"),
        ("USERPROFILE", r"scoop\apps\git\current"),
    ] {
        if let Some(base) = env(var) {
            roots.push(join(&base, rel));
        }
    }
    roots
        .into_iter()
        .map(|r| join(&r, r"bin\bash.exe"))
        .find(|p| exists(p))
}

fn kind_of(program: &str) -> Option<Kind> {
    let name = program
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    match name.trim_end_matches(".exe") {
        "bash" => Some(Kind::GitBash),
        "pwsh" => Some(Kind::Pwsh),
        "powershell" => Some(Kind::PowerShell),
        "cmd" => Some(Kind::Cmd),
        _ => None,
    }
}

fn join(base: &str, rel: &str) -> String {
    format!("{}\\{rel}", base.trim_end_matches(['\\', '/']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pick(vars: &[(&str, &str)], files: &[&str], path: &[(&str, &str)]) -> Shell {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let files: Vec<String> = files.iter().map(|f| f.to_ascii_lowercase()).collect();
        let path: HashMap<String, String> = path
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        choose(
            &|k| vars.get(k).cloned(),
            &|p| files.contains(&p.to_ascii_lowercase()),
            &|n| path.get(n).cloned(),
        )
    }

    const WIN: &[(&str, &str)] = &[
        ("ProgramFiles", r"C:\Program Files"),
        ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ("LOCALAPPDATA", r"C:\Users\Ann Lee\AppData\Local"),
        ("USERPROFILE", r"C:\Users\Ann Lee"),
        ("SystemRoot", r"C:\Windows"),
        ("ComSpec", r"C:\Windows\system32\cmd.exe"),
    ];
    const PS51: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

    #[test]
    fn claude_finds_custom_git_without_overriding_its_own_setting() {
        let bash = r"D:\Tools\Git\bin\bash.exe";
        let exists = |p: &str| p == bash;
        let path = |p: &str| (p == "git.exe").then(|| r"D:\Tools\Git\cmd\git.exe".into());
        let env = |k: &str| (k == "TESSERA_SHELL").then(|| PS51.into());
        assert_eq!(claude_git_bash(&env, &exists, &path).as_deref(), Some(bash));
        for value in ["", r"C:\Other Git\bin\bash.exe"] {
            let env = |k: &str| (k == "CLAUDE_CODE_GIT_BASH_PATH").then(|| value.into());
            assert_eq!(claude_git_bash(&env, &exists, &path), None);
        }
        assert_eq!(claude_git_bash(&|_| None, &|_| false, &path), None);
    }

    #[test]
    fn git_bash_comes_first_wherever_git_is() {
        // Next to git.exe on PATH (a custom install folder).
        let s = pick(
            WIN,
            &[r"D:\Tools\Git\bin\bash.exe", PS51],
            &[("git.exe", r"D:\Tools\Git\cmd\git.exe")],
        );
        assert_eq!(s.kind, Kind::GitBash);
        assert_eq!(s.program, r"D:\Tools\Git\bin\bash.exe");
        // The usual install folder, without git on PATH.
        let s = pick(WIN, &[r"C:\Program Files\Git\bin\bash.exe", PS51], &[]);
        assert_eq!(s.program, r"C:\Program Files\Git\bin\bash.exe");
        // A per-user install.
        let s = pick(
            WIN,
            &[r"C:\Users\Ann Lee\AppData\Local\Programs\Git\bin\bash.exe"],
            &[],
        );
        assert_eq!(s.kind, Kind::GitBash);
    }

    #[test]
    fn never_the_wsl_bash() {
        // Only System32's bash.exe (WSL) and Windows PowerShell exist.
        let s = pick(
            WIN,
            &[r"C:\Windows\System32\bash.exe", PS51],
            &[("bash.exe", r"C:\Windows\System32\bash.exe")],
        );
        assert_eq!(s.kind, Kind::PowerShell);
        assert_eq!(s.program, PS51);
    }

    #[test]
    fn powershell_7_then_5_then_cmd() {
        let s = pick(WIN, &[r"C:\Program Files\PowerShell\7\pwsh.exe", PS51], &[]);
        assert_eq!(s.kind, Kind::Pwsh);
        let s = pick(WIN, &[PS51], &[]);
        assert_eq!(s.kind, Kind::PowerShell);
        let s = pick(WIN, &[r"C:\Windows\system32\cmd.exe"], &[]);
        assert_eq!(s.kind, Kind::Cmd);
        assert_eq!(s.program, r"C:\Windows\system32\cmd.exe");
    }

    #[test]
    fn the_override_wins_when_it_exists() {
        let mut vars = WIN.to_vec();
        vars.push(("TESSERA_SHELL", r"C:\Windows\system32\cmd.exe"));
        let s = pick(
            &vars,
            &[
                r"C:\Program Files\Git\bin\bash.exe",
                r"C:\Windows\system32\cmd.exe",
            ],
            &[],
        );
        assert_eq!(s.kind, Kind::Cmd);
        // A missing one is ignored.
        let mut vars = WIN.to_vec();
        vars.push(("TESSERA_SHELL", r"C:\nowhere\pwsh.exe"));
        let s = pick(&vars, &[r"C:\Program Files\Git\bin\bash.exe"], &[]);
        assert_eq!(s.kind, Kind::GitBash);
    }

    #[test]
    fn command_lines_keep_the_command_out_of_quoting() {
        let bash = Shell {
            kind: Kind::GitBash,
            program: "bash.exe".into(),
        };
        let cmd = r#"echo "a  b" 'c' \"d\" && ls *.rs"#;
        let (line, env) = bash.command_line(cmd);
        assert!(!line.contains("echo"), "{line}");
        assert_eq!(env, vec![(COMMAND_VAR.to_string(), cmd.to_string())]);

        let ps = Shell {
            kind: Kind::Pwsh,
            program: "pwsh.exe".into(),
        };
        let (line, env) = ps.command_line("Write-Output \"hi\"");
        assert!(env.is_empty());
        let b64 = line.rsplit(' ').next().unwrap();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.ends_with("\nWrite-Output \"hi\""), "{script}");
        assert!(script.starts_with("$ProgressPreference"), "{script}");

        let cmd = Shell {
            kind: Kind::Cmd,
            program: "cmd.exe".into(),
        };
        assert_eq!(
            cmd.command_line(r#"dir "C:\Program Files""#).0,
            r#"/d /s /c "dir "C:\Program Files"""#
        );
    }

    #[test]
    fn hints_name_the_syntax() {
        for (kind, word) in [
            (Kind::GitBash, "Git Bash"),
            (Kind::Pwsh, "PowerShell 7"),
            (Kind::PowerShell, "never `&&`"),
            (Kind::Cmd, "cmd.exe"),
        ] {
            let s = Shell {
                kind,
                program: String::new(),
            };
            assert!(s.hint().contains(word), "{kind:?}: {}", s.hint());
            assert!(s.hint().starts_with(' '));
        }
    }
}
