//! Terminal identity. Decorative output is skipped for narrow terminals,
//! non-TTY output, `--plain` and `NO_COLOR`.

use std::io::IsTerminal;

#[allow(dead_code)] // the tile mark on its own, for narrow layouts
pub const LOGO: &[&str] = &[r"  ▗▄▖▗▄▖  ", r"  ▐█▌▐▒▌  ", r"  ▗▄▖▗▄▖  ", r"  ▐▒▌▐▒▌  "];

pub const WORDMARK: &[&str] = &[
    r"▀█▀ █▀▀ █▀▀ █▀▀ █▀▀ █▀█ ▄▀█",
    r" █  ██▄ ▄▄█ ▄▄█ ██▄ █▀▄ █▀█",
];

pub fn fancy() -> bool {
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

/// Banner lines (plain text) that fit `width` columns.
pub fn lines(width: u16) -> Vec<String> {
    if width < 44 {
        return vec!["◆ tessera".into()];
    }
    let mut out = Vec::new();
    for (i, w) in WORDMARK.iter().enumerate() {
        let tile = if i == 0 {
            "▗▄▖▗▄▖ "
        } else {
            "▝▀▘▝█▘ "
        };
        out.push(format!("{tile}{w}"));
    }
    out
}

pub fn print_banner(subtitle: &str) {
    if !fancy() {
        println!("tessera {} — {subtitle}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
    let accent = "\x1b[38;2;94;234;212m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    println!();
    for l in lines(width) {
        println!("  {accent}{l}{reset}");
    }
    println!(
        "  {dim}v{} · {subtitle}{reset}\n",
        env!("CARGO_PKG_VERSION")
    );
}
