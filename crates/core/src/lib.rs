//! Tessera core: the agent engine behind every client.

pub mod activity;
pub mod agents;
pub(crate) mod approval;
pub mod auth;
mod blocking;
pub mod bots;
pub mod browser;
pub mod budget;
pub mod claude_code;
pub mod config;
pub mod context;
pub(crate) mod disk;
pub mod engine;
pub mod guard;
pub mod guides;
pub mod jobs;
pub mod limits;
pub mod llm;
pub mod lsp;
pub mod mcp;
pub mod memory;
pub mod memstats;
pub mod node;
pub mod platform;
pub mod pricing;
pub mod profiles;
pub mod prompt;
pub mod providers;
#[cfg(unix)]
pub mod pty;
#[cfg(windows)]
#[path = "pty_windows.rs"]
pub mod pty;
#[cfg(all(test, not(windows)))]
mod pty_windows;
pub mod remote;
pub mod sandbox;
pub mod scan;
pub mod skills;
pub mod store;
pub mod terminal;
pub mod thinking;
pub mod tools;
pub(crate) mod trace;
pub mod updates;
pub mod util;
pub mod voice;
pub mod worktree;

pub use engine::{ClientGuard, Engine, EngineOptions};

/// Tessera's public home: `repository` in the workspace Cargo.toml, the one
/// place it is set for the code (the web UI reads it from there too; the
/// installer keeps a copy at the top of scripts/install.sh).
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The one command that installs (or updates) Tessera on a computer.
pub fn install_command() -> String {
    format!("curl -fsSL {REPOSITORY}/releases/latest/download/install.sh | sh")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_repository_is_set() {
        assert!(super::REPOSITORY.starts_with("https://"));
        assert!(!super::REPOSITORY.ends_with('/'));
        assert!(super::install_command().ends_with("/install.sh | sh"));
    }
}
