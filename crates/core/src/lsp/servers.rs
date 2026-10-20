//! Which language servers exist, which one handles a file, and where its
//! project root is. Detection is cheap (a few `stat`s): project markers plus
//! a binary on `PATH`. Nothing is ever installed.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::config::LspConfig;

#[derive(Debug, Clone, PartialEq)]
pub struct ServerDef {
    /// Shown to the model and used in approval keys (`rust-analyzer`).
    pub name: String,
    /// Key for `[lsp.command]` overrides (`rust`).
    pub language: String,
    /// Program (as configured, resolved on `PATH` when started) and arguments.
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub markers: Vec<String>,
    root: RootRule,
    language_id: Option<String>,
    /// `initializationOptions`.
    pub init_options: Value,
    /// Answers to `workspace/configuration`, by section.
    pub settings: Value,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RootRule {
    /// The nearest directory with a marker.
    Nearest,
    /// Cargo: the outermost `Cargo.toml` that declares a `[workspace]`,
    /// else the nearest one.
    CargoWorkspace,
    /// Go: the outermost `go.work`, else the nearest `go.mod`.
    GoWork,
}

struct Builtin {
    language: &'static str,
    /// Alternatives in order of preference: (name, argv).
    candidates: &'static [(&'static str, &'static [&'static str])],
    extensions: &'static [&'static str],
    markers: &'static [&'static str],
    root: RootRule,
}

const BUILTINS: &[Builtin] = &[
    Builtin {
        language: "rust",
        candidates: &[("rust-analyzer", &["rust-analyzer"])],
        extensions: &["rs"],
        markers: &["Cargo.toml"],
        root: RootRule::CargoWorkspace,
    },
    Builtin {
        language: "python",
        candidates: &[
            ("pyright", &["pyright-langserver", "--stdio"]),
            ("basedpyright", &["basedpyright-langserver", "--stdio"]),
            ("pylsp", &["pylsp"]),
        ],
        extensions: &["py", "pyi"],
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "Pipfile",
            "pyrightconfig.json",
        ],
        root: RootRule::Nearest,
    },
    Builtin {
        language: "typescript",
        candidates: &[(
            "typescript-language-server",
            &["typescript-language-server", "--stdio"],
        )],
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"],
        markers: &["tsconfig.json", "jsconfig.json", "package.json"],
        root: RootRule::Nearest,
    },
    Builtin {
        language: "go",
        candidates: &[("gopls", &["gopls"])],
        extensions: &["go"],
        markers: &["go.work", "go.mod"],
        root: RootRule::GoWork,
    },
    Builtin {
        language: "c",
        // No background index: it would write `.cache/clangd` into the
        // user's repository.
        candidates: &[(
            "clangd",
            &["clangd", "--background-index=false", "--log=error"],
        )],
        extensions: &[
            "c", "h", "cc", "cpp", "cxx", "c++", "hpp", "hh", "hxx", "m", "mm",
        ],
        markers: &[
            "compile_commands.json",
            "compile_flags.txt",
            ".clangd",
            "CMakeLists.txt",
            "meson.build",
        ],
        root: RootRule::Nearest,
    },
];

fn server_settings(language: &str) -> (Value, Value) {
    match language {
        // Own target directory: rust-analyzer's `cargo check` must not hold
        // the build lock while the agent runs `cargo test`. Its own file
        // watcher: Tessera doesn't forward file-system events.
        "rust" => {
            let ra = json!({
                "cargo": {"targetDir": true},
                "checkOnSave": true,
                "files": {"watcher": "server"}
            });
            (ra.clone(), json!({"rust-analyzer": ra}))
        }
        _ => (Value::Null, Value::Null),
    }
}

/// Built-ins (with `[lsp.command]` overrides applied) plus `[[lsp.server]]`.
/// Every alternative is listed; `detect`/`route` pick the first installed.
pub fn definitions(cfg: &LspConfig) -> Vec<ServerDef> {
    let mut out = Vec::new();
    for b in BUILTINS {
        let (init_options, settings) = server_settings(b.language);
        let base = |name: &str, command: Vec<String>| ServerDef {
            name: name.to_string(),
            language: b.language.to_string(),
            command,
            extensions: b.extensions.iter().map(|s| s.to_string()).collect(),
            markers: b.markers.iter().map(|s| s.to_string()).collect(),
            root: b.root,
            language_id: None,
            init_options: init_options.clone(),
            settings: settings.clone(),
        };
        match cfg.command.get(b.language) {
            Some(cmd) if cmd.is_empty() => {} // disabled
            Some(cmd) => {
                let name = Path::new(&cmd[0])
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| cmd[0].clone());
                out.push(base(&name, cmd.clone()));
            }
            None => {
                for (name, argv) in b.candidates {
                    out.push(base(name, argv.iter().map(|s| s.to_string()).collect()));
                }
            }
        }
    }
    for s in &cfg.servers {
        if s.command.is_empty() || s.extensions.is_empty() {
            continue;
        }
        out.push(ServerDef {
            name: s.name.clone(),
            language: s.name.clone(),
            command: s.command.clone(),
            extensions: s
                .extensions
                .iter()
                .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
            markers: s.markers.clone(),
            root: RootRule::Nearest,
            language_id: s.language_id.clone(),
            init_options: Value::Null,
            settings: Value::Null,
        });
    }
    out
}

impl ServerDef {
    pub fn handles(&self, path: &Path) -> bool {
        ext(path).is_some_and(|e| self.extensions.contains(&e))
    }

    pub fn language_id(&self, path: &Path) -> String {
        if let Some(id) = &self.language_id {
            return id.clone();
        }
        let e = ext(path).unwrap_or_default();
        match (self.language.as_str(), e.as_str()) {
            ("typescript", "tsx") => "typescriptreact".into(),
            ("typescript", "jsx") => "javascriptreact".into(),
            ("typescript", "js" | "mjs" | "cjs") => "javascript".into(),
            ("typescript", _) => "typescript".into(),
            ("c", "c" | "h") => "c".into(),
            ("c", "m") => "objective-c".into(),
            ("c", "mm") => "objective-cpp".into(),
            ("c", _) => "cpp".into(),
            ("rust" | "python" | "go", _) => self.language.clone(),
            _ => e,
        }
    }

    /// The installed program for work in `dir`, if any: `None` for a rustup
    /// proxy whose toolchains lack the component, and for
    /// typescript-language-server without TypeScript (global `tsserver`, or
    /// `node_modules/typescript` in `dir` or above).
    pub fn binary_for(&self, dir: &Path) -> Option<PathBuf> {
        let bin = which(self.command.first()?)?;
        if self.name == "rust-analyzer" && !rustup_component_ok(&bin) {
            return None;
        }
        if self.language == "typescript"
            && which("tsserver").is_none()
            && !dir
                .ancestors()
                .any(|d| d.join("node_modules/typescript/lib/tsserver.js").is_file())
        {
            return None;
        }
        Some(bin)
    }

    /// Whether `cwd` looks like a project for this server: a marker in it or
    /// above it, a marker one level below, or a matching file in it.
    pub fn matches_project(&self, cwd: &Path) -> bool {
        let home = dirs::home_dir();
        // The home directory (or `/`) isn't a project, whatever is below it.
        if is_too_broad(cwd, home.as_deref()) {
            return false;
        }
        let mut dir = Some(cwd);
        while let Some(d) = dir {
            if !is_too_broad(d, home.as_deref()) && self.markers.iter().any(|m| d.join(m).exists())
            {
                return true;
            }
            dir = d.parent();
        }
        let Ok(rd) = std::fs::read_dir(cwd) else {
            return false;
        };
        for entry in rd.flatten().take(500) {
            let p = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if name.starts_with('.')
                    || matches!(
                        name.as_ref(),
                        "node_modules" | "target" | "vendor" | "build" | "dist"
                    )
                {
                    continue;
                }
                if self.markers.iter().any(|m| p.join(m).exists()) {
                    return true;
                }
            } else if self.handles(&p) {
                return true;
            }
        }
        false
    }

    /// Project root for `file`: see [`RootRule`]; without a marker, `cwd`
    /// when the file is inside it. Never `/` or the home directory itself.
    pub fn root_for(&self, file: &Path, cwd: &Path) -> Option<PathBuf> {
        let home = dirs::home_dir();
        let start = file.parent()?;
        let ancestors = || {
            start
                .ancestors()
                .filter(|d| !is_too_broad(d, home.as_deref()))
        };
        let found = match self.root {
            RootRule::Nearest => {
                ancestors().find(|d| self.markers.iter().any(|m| d.join(m).exists()))
            }
            RootRule::CargoWorkspace => {
                let nearest = ancestors().find(|d| d.join("Cargo.toml").is_file());
                nearest.map(|n| {
                    n.ancestors()
                        .filter(|d| !is_too_broad(d, home.as_deref()))
                        .filter(|d| {
                            std::fs::read_to_string(d.join("Cargo.toml"))
                                .is_ok_and(|t| t.lines().any(|l| l.trim() == "[workspace]"))
                        })
                        .last()
                        .unwrap_or(n)
                })
            }
            RootRule::GoWork => ancestors()
                .filter(|d| d.join("go.work").is_file())
                .last()
                .or_else(|| ancestors().find(|d| d.join("go.mod").is_file())),
        };
        found.map(Path::to_path_buf).or_else(|| {
            (file.starts_with(cwd) && !is_too_broad(cwd, home.as_deref()))
                .then(|| cwd.to_path_buf())
        })
    }
}

/// `/` and the home directory: indexing those would mean everything.
fn is_too_broad(dir: &Path, home: Option<&Path>) -> bool {
    dir.parent().is_none() || Some(dir) == home
}

fn ext(path: &Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// A program on `PATH` (or an explicit path) that is an executable file.
pub fn which(program: &str) -> Option<PathBuf> {
    let executable = |p: &Path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    };
    // Windows: `.exe`, and npm's `.cmd` launchers (most language servers).
    #[cfg(windows)]
    if !crate::platform::program_is_path(program, true) {
        return crate::platform::find_program(program);
    }
    if crate::platform::program_is_path(program, cfg!(windows)) {
        let p = crate::util::expand_home(program);
        return executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| executable(p))
}

/// `~/.cargo/bin/rust-analyzer` is usually a rustup proxy that fails unless
/// the component is installed for some toolchain.
fn rustup_component_ok(bin: &Path) -> bool {
    let real = bin.canonicalize().unwrap_or_else(|_| bin.to_path_buf());
    if real.file_name().is_none_or(|n| n != "rustup") {
        return true;
    }
    let home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".rustup")));
    let Some(toolchains) = home.map(|h| h.join("toolchains")) else {
        return false;
    };
    std::fs::read_dir(toolchains)
        .map(|rd| {
            rd.flatten()
                .any(|t| t.path().join("bin/rust-analyzer").is_file())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(server: crate::config::LspServerConfig) -> LspConfig {
        LspConfig {
            servers: vec![server],
            ..Default::default()
        }
    }

    #[test]
    fn overrides_custom_servers_and_language_ids() {
        let mut cfg = LspConfig::default();
        cfg.command.insert("python".into(), vec!["pylsp".into()]);
        cfg.command.insert("c".into(), vec![]);
        let defs = definitions(&cfg);
        let py: Vec<&ServerDef> = defs.iter().filter(|d| d.language == "python").collect();
        assert_eq!(py.len(), 1);
        assert_eq!(py[0].name, "pylsp");
        assert!(
            !defs.iter().any(|d| d.language == "c"),
            "empty command disables"
        );
        let ts = defs.iter().find(|d| d.language == "typescript").unwrap();
        assert_eq!(ts.language_id(Path::new("a.tsx")), "typescriptreact");
        assert_eq!(ts.language_id(Path::new("a.mjs")), "javascript");
        let custom = definitions(&cfg_with(crate::config::LspServerConfig {
            name: "foo-ls".into(),
            command: vec!["/bin/true".into()],
            extensions: vec![".Foo".into()],
            markers: vec!["foo.toml".into()],
            language_id: None,
        }));
        let foo = custom.iter().find(|d| d.name == "foo-ls").unwrap();
        assert!(foo.handles(Path::new("x/a.FOO")));
        assert_eq!(foo.language_id(Path::new("a.foo")), "foo");
        assert_eq!(
            foo.binary_for(Path::new("/")),
            Some(PathBuf::from("/bin/true"))
        );
    }

    #[test]
    fn roots_and_project_detection() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("crates/a/src")).unwrap();
        std::fs::write(
            ws.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        )
        .unwrap();
        std::fs::write(ws.join("crates/a/Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        let defs = definitions(&LspConfig::default());
        let ra = defs.iter().find(|d| d.name == "rust-analyzer").unwrap();
        let file = ws.join("crates/a/src/lib.rs");
        assert_eq!(ra.root_for(&file, &ws.join("crates/a")), Some(ws.clone()));
        assert!(ra.matches_project(&ws.join("crates/a/src")));
        assert!(ra.matches_project(&ws), "marker in cwd");
        // A monorepo parent with the project one level down.
        assert!(ra.matches_project(dir.path()));
        // No marker: the session directory, only for files inside it.
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("x.py"), "").unwrap();
        let py = defs.iter().find(|d| d.language == "python").unwrap();
        assert!(py.matches_project(&plain), "a matching file in cwd");
        assert_eq!(
            py.root_for(&plain.join("x.py"), &plain),
            Some(plain.clone())
        );
        assert_eq!(py.root_for(Path::new("/elsewhere/x.py"), &plain), None);
        assert!(!ra.matches_project(&plain));
    }
}
