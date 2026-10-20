//! A small terminal toolkit for the setup wizard: the Tesserae palette,
//! capability detection, selection lists, text and masked input,
//! confirmations and spinners made of tesserae.
//!
//! With a capable terminal the widgets are key-driven and redraw in place;
//! otherwise (`NO_COLOR`, `TERM=dumb`, `--plain`, piped input or output)
//! they fall back to numbered, line-based prompts with no animation.

use std::future::Future;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zeroize::Zeroizing;

// ---- palette -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Smalti gold: the accent.
pub const GOLD: Rgb = Rgb(0xD4, 0xAF, 0x5F);
pub const LAPIS: Rgb = Rgb(0x7D, 0x9B, 0xE0);
pub const VERDIGRIS: Rgb = Rgb(0x5F, 0xB3, 0x9E);
pub const TERRACOTTA: Rgb = Rgb(0xE0, 0x7A, 0x56);
/// Warm stone greys, dark to light: the apps' tokens (tess stones, faint,
/// muted, text-2 and text of the dark theme).
pub const STONE: [Rgb; 6] = [
    Rgb(0x33, 0x2F, 0x2A),
    Rgb(0x4A, 0x45, 0x3F),
    Rgb(0x7A, 0x74, 0x6C),
    Rgb(0x95, 0x8E, 0x84),
    Rgb(0xB3, 0xAC, 0xA2),
    Rgb(0xEC, 0xE6, 0xDC),
];
/// The same roles on a light background (Travertine).
const LIGHT_STONE: [Rgb; 6] = [
    Rgb(0xE2, 0xDA, 0xCD),
    Rgb(0xCD, 0xC3, 0xB3),
    Rgb(0x85, 0x7D, 0x72),
    Rgb(0x6B, 0x64, 0x5B),
    Rgb(0x4F, 0x49, 0x42),
    Rgb(0x21, 0x1E, 0x1A),
];

/// The terminal has a light background: colours are drawn in the light
/// theme's tones (set once, when the wizard starts).
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_light(on: bool) {
    LIGHT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_light() -> bool {
    LIGHT.load(std::sync::atomic::Ordering::Relaxed)
}

/// A colour from the dark palette, in the tone that suits the terminal:
/// accents deepen and the stone ramp flips on a light background.
pub fn adapt(c: Rgb) -> Rgb {
    adapt_for(c, LIGHT.load(std::sync::atomic::Ordering::Relaxed))
}

fn adapt_for(c: Rgb, light: bool) -> Rgb {
    if !light {
        return c;
    }
    const PAIRS: [(Rgb, Rgb); 4] = [
        (GOLD, Rgb(0x9C, 0x74, 0x24)),
        (LAPIS, Rgb(0x3D, 0x5C, 0xA8)),
        (VERDIGRIS, Rgb(0x2C, 0x75, 0x64)),
        (TERRACOTTA, Rgb(0xA9, 0x4B, 0x2A)),
    ];
    if let Some((_, l)) = PAIRS.iter().find(|(d, _)| *d == c) {
        return *l;
    }
    if let Some(i) = STONE.iter().position(|s| *s == c) {
        return LIGHT_STONE[i];
    }
    let (r, g, b) = (c.0 as i32, c.1 as i32, c.2 as i32);
    let grey = (r - g).abs() < 28 && (g - b).abs() < 28 && (r - b).abs() < 36;
    if grey {
        // Mirror the lightness, keeping the warm tint.
        let avg = (r + g + b) / 3;
        let k = 1.0 - avg as f32 / 255.0;
        LIGHT_STONE[5].mix(LIGHT_STONE[0], k)
    } else {
        c.mix(Rgb(0, 0, 0), 0.32)
    }
}

impl Rgb {
    /// Mix toward `other` by `t` (0 = self, 1 = other).
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    pub fn scale(self, k: f32) -> Rgb {
        let s = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
        Rgb(s(self.0), s(self.1), s(self.2))
    }
}

/// Nearest xterm-256 colour (6×6×6 cube or the grey ramp).
pub fn to_256(c: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| -> usize {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (**l as i32 - v as i32).abs())
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (r, g, b) = (near(c.0), near(c.1), near(c.2));
    let cube = Rgb(LEVELS[r], LEVELS[g], LEVELS[b]);
    let avg = (c.0 as u32 + c.1 as u32 + c.2 as u32) / 3;
    let gi = ((avg as i32 - 8).max(0) / 10).min(23) as u8;
    let grey_v = 8 + 10 * gi;
    let grey = Rgb(grey_v, grey_v, grey_v);
    let dist = |a: Rgb| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, c.0) + d(a.1, c.1) + d(a.2, c.2)
    };
    if dist(grey) < dist(cube) {
        232 + gi
    } else {
        16 + 36 * r as u8 + 6 * g as u8 + b as u8
    }
}

/// Closest of the 16 ANSI colours (as an SGR foreground code).
pub fn to_16(c: Rgb) -> u8 {
    const TABLE: [(Rgb, u8); 12] = [
        (Rgb(0, 0, 0), 30),
        (Rgb(205, 49, 49), 31),
        (Rgb(13, 188, 121), 32),
        (Rgb(229, 229, 16), 33),
        (Rgb(36, 114, 200), 34),
        (Rgb(17, 168, 205), 36),
        (Rgb(190, 190, 190), 37),
        (Rgb(102, 102, 102), 90),
        (Rgb(241, 76, 76), 91),
        (Rgb(245, 245, 67), 93),
        (Rgb(59, 142, 234), 94),
        (Rgb(229, 229, 229), 97),
    ];
    let d = |a: Rgb| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, c.0) + d(a.1, c.1) + d(a.2, c.2)
    };
    TABLE
        .iter()
        .min_by_key(|(rgb, _)| d(*rgb))
        .map(|(_, code)| *code)
        .unwrap_or(37)
}

// ---- capabilities --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    None,
    Ansi16,
    Ansi256,
    True,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub depth: Depth,
    /// Key-driven widgets that redraw in place (needs a real terminal on
    /// both ends).
    pub fancy: bool,
    pub anim: bool,
    /// Standard input is a terminal (masked input can switch echo off).
    pub stdin_tty: bool,
}

impl Caps {
    pub fn detect(plain: bool, no_anim: bool) -> Self {
        let caps = Self::from_env(
            |k| std::env::var(k).ok(),
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
            plain,
            no_anim,
        );
        if caps.depth != Depth::None && caps.fancy {
            set_light(crate::tui::light_terminal());
        }
        caps
    }

    pub fn from_env(
        env: impl Fn(&str) -> Option<String>,
        stdin_tty: bool,
        stdout_tty: bool,
        plain: bool,
        no_anim: bool,
    ) -> Self {
        let term = env("TERM").unwrap_or_default();
        let dumb = term == "dumb";
        let no_color = env("NO_COLOR").is_some_and(|v| !v.is_empty());
        let fancy = stdin_tty && stdout_tty && !dumb;
        let depth = if plain || no_color || dumb || !stdout_tty {
            Depth::None
        } else if env("COLORTERM").is_some_and(|v| v.contains("truecolor") || v.contains("24bit")) {
            Depth::True
        } else if term.contains("256") || term.contains("kitty") || term.contains("alacritty") {
            Depth::Ansi256
        } else {
            Depth::Ansi16
        };
        let anim = fancy
            && depth != Depth::None
            && !no_anim
            && env("TESSERA_NO_ANIM").is_none_or(|v| v.is_empty());
        Self {
            depth,
            fancy: fancy && !plain,
            anim,
            stdin_tty,
        }
    }
}

// ---- styles --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sty {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub dim: bool,
    pub reverse: bool,
}

impl Sty {
    pub const fn fg(c: Rgb) -> Self {
        Self {
            fg: Some(c),
            bg: None,
            bold: false,
            dim: false,
            reverse: false,
        }
    }
    pub const fn plain() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            reverse: false,
        }
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn on(mut self, c: Rgb) -> Self {
        self.bg = Some(c);
        self
    }
    pub const fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }
}

pub const S_ACCENT: Sty = Sty::fg(GOLD);
pub const S_OK: Sty = Sty::fg(VERDIGRIS);
/// Warnings and errors: terracotta and its redder neighbour, as in the apps.
pub const S_WARN: Sty = Sty::fg(TERRACOTTA);
pub const S_ERR: Sty = Sty::fg(Rgb(0xE8, 0x71, 0x4C));
pub const S_LINK: Sty = Sty::fg(LAPIS);
pub const S_HINT: Sty = Sty::fg(STONE[3]);
pub const S_FAINT: Sty = Sty::fg(STONE[2]);
/// Choices that aren't highlighted: a step quieter than text.
pub const S_SOFT: Sty = Sty::fg(STONE[4]);
pub const S_TEXT: Sty = Sty::fg(STONE[5]);

/// SGR sequence for a style at a colour depth (empty for `Depth::None`).
pub fn sgr(st: Sty, depth: Depth) -> String {
    if depth == Depth::None {
        return String::new();
    }
    let mut codes: Vec<String> = Vec::new();
    if st.bold {
        codes.push("1".into());
    }
    if st.dim {
        codes.push("2".into());
    }
    if st.reverse {
        codes.push("7".into());
    }
    let color = |c: Rgb, bg: bool| -> String {
        let c = adapt(c);
        match depth {
            Depth::True => format!("{};2;{};{};{}", if bg { 48 } else { 38 }, c.0, c.1, c.2),
            Depth::Ansi256 => format!("{};5;{}", if bg { 48 } else { 38 }, to_256(c)),
            _ => format!("{}", to_16(c) + if bg { 10 } else { 0 }),
        }
    };
    if let Some(c) = st.fg {
        codes.push(color(c, false));
    }
    if let Some(c) = st.bg {
        codes.push(color(c, true));
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

pub fn paint_at(s: &str, st: Sty, depth: Depth) -> String {
    let open = sgr(st, depth);
    if open.is_empty() {
        s.to_string()
    } else {
        format!("{open}{s}\x1b[0m")
    }
}

// ---- text helpers --------------------------------------------------------------------

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(s).as_str())
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if chars.peek() == Some(&']') {
                // OSC: up to BEL or ST.
                for c in chars.by_ref() {
                    if c == '\x07' || c == '\\' {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Greedy word wrap of plain text to `width` columns. A `code span` (a
/// command to type) stays on one line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let glued = glue_code(text);
    let text = glued.as_str();
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut lw = 0;
        for word in para.split(' ') {
            let ww = UnicodeWidthStr::width(word);
            if lw > 0 && lw + 1 + ww > width {
                out.push(std::mem::take(&mut line));
                lw = 0;
            }
            if lw > 0 {
                line.push(' ');
                lw += 1;
            }
            line.push_str(word);
            lw += ww;
        }
        out.push(line);
    }
    for l in out.iter_mut() {
        if l.contains(GLUE) {
            *l = l.replace(GLUE, " ");
        }
    }
    out
}

/// Stands in for a space inside a `code span` while wrapping.
const GLUE: char = '\u{a0}';

/// `text` with the spaces of its `code spans` glued (balanced spans only).
fn glue_code(text: &str) -> String {
    if !text.contains('`') || !text.matches('`').count().is_multiple_of(2) {
        return text.to_string();
    }
    let mut code = false;
    text.chars()
        .map(|c| {
            if c == '`' {
                code = !code;
            }
            if code && c == ' ' { GLUE } else { c }
        })
        .collect()
}

/// `code spans` in prose: the code without its backticks, in gold (with
/// colour; plain output keeps the backticks). Unpaired backticks stay.
pub fn prose_at(s: &str, st: Sty, depth: Depth) -> String {
    if depth == Depth::None || !s.contains('`') || !s.matches('`').count().is_multiple_of(2) {
        return paint_at(s, st, depth);
    }
    let code = Sty {
        fg: S_ACCENT.fg,
        ..st
    };
    s.split('`')
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(i, part)| paint_at(part, if i % 2 == 1 { code } else { st }, depth))
        .collect()
}

/// The width `prose_at` shows `s` in.
pub fn prose_width(s: &str, depth: Depth) -> usize {
    let ticks = s.matches('`').count();
    if depth == Depth::None || !ticks.is_multiple_of(2) {
        width(s)
    } else {
        width(s) - ticks
    }
}

/// Spinner frames: one tessera orbiting a 2×2 square.
pub const SPIN: [&str; 4] = ["▖", "▘", "▝", "▗"];

// ---- errors ----------------------------------------------------------------------------

/// Where a step of the guided setup is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Done,
    Skipped,
    Now,
    Next,
}

/// A page of the setup (see [`Term::page`]).
pub struct Page<'a> {
    pub title: &'a str,
    pub sub: &'a str,
    /// The run's steps by short name and where each is (empty: none).
    pub steps: &'a [(&'a str, Mark)],
}

/// The highlighted choice's band (the darkest stone; the palest on a
/// light terminal).
const BAND: Rgb = STONE[0];

/// Display width of text that may carry colour codes.
fn width_of(s: &str) -> usize {
    width(s)
}

/// The user pressed Ctrl+C (or input ended): stop the wizard.
#[derive(Debug)]
pub struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("setup cancelled")
    }
}

impl std::error::Error for Aborted {}

// ---- terminal ----------------------------------------------------------------------------

#[derive(Clone)]
pub struct Choice {
    pub label: String,
    /// Shown under the highlighted choice.
    pub hint: String,
    /// Short badge after the label ("recommended", "current").
    pub tag: String,
}

impl Choice {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: String::new(),
            tag: String::new(),
        }
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = h.into();
        self
    }
    pub fn tag(mut self, t: impl Into<String>) -> Self {
        self.tag = t.into();
        self
    }
}

/// One `label   value` row (see [`Term::rows`]).
pub struct Row {
    pub label: String,
    pub value: String,
    pub st: Sty,
    /// Optional glyph before the label (a status tile).
    pub mark: &'static str,
    pub mark_st: Sty,
}

impl Row {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            st: S_TEXT,
            mark: "",
            mark_st: Sty::plain(),
        }
    }
    pub fn st(mut self, st: Sty) -> Self {
        self.st = st;
        self
    }
    /// A value that isn't set up yet.
    pub fn missing(self) -> Self {
        self.st(S_HINT)
    }
    pub fn mark(mut self, mark: &'static str, st: Sty) -> Self {
        self.mark = mark;
        self.mark_st = st;
        self
    }
}

pub(super) enum Ev {
    Key(KeyEvent),
    Paste(String),
    Redraw,
    Tick,
}

/// Restores the terminal however a widget ends (error, Ctrl+C, panic).
pub(super) struct RawGuard {
    paste: bool,
}

impl RawGuard {
    pub(super) fn new(paste: bool, hide_cursor: bool) -> Self {
        let _ = crossterm::terminal::enable_raw_mode();
        let mut out = std::io::stdout();
        if paste {
            let _ = out.write_all(b"\x1b[?2004h");
        }
        if hide_cursor {
            let _ = out.write_all(b"\x1b[?25l");
        }
        let _ = out.flush();
        Self { paste }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        if self.paste {
            let _ = out.write_all(b"\x1b[?2004l");
        }
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Makes sure the cursor is visible and raw mode is off if we panic.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = std::io::stdout().write_all(b"\x1b[?25h\x1b[?2004l\r\n");
        prev(info);
    }));
}

pub struct Term {
    pub caps: Caps,
    events: Option<EventStream>,
    /// Lines of the live region drawn so far (erased before a redraw).
    live: usize,
    /// Line-based input reached end of file.
    eof: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Something was said since the last answer (a page waits for it to
    /// be read before the next one clears the screen).
    said: std::sync::atomic::AtomicBool,
    /// What esc does in a list ("skip"; the dashboard's is "done").
    pub esc_word: &'static str,
}

/// Indent for everything the wizard prints.
pub const PAD: &str = "  ";

impl Term {
    pub fn new(caps: Caps) -> Self {
        Self {
            caps,
            events: None,
            live: 0,
            eof: Default::default(),
            said: Default::default(),
            esc_word: "skip",
        }
    }

    /// Piped input ran out: answers from then on were defaults or skips.
    pub fn input_ended(&self) -> bool {
        self.eof.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn cols(&self) -> usize {
        crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80)
            .clamp(20, 400)
    }

    /// Text width for paragraphs.
    pub fn text_width(&self) -> usize {
        self.cols().saturating_sub(PAD.len() * 2 + 1).clamp(20, 88)
    }

    pub fn paint(&self, s: &str, st: Sty) -> String {
        paint_at(s, st, self.caps.depth)
    }

    /// Prose with `code spans` (see [`prose_at`]).
    pub fn prose(&self, s: &str, st: Sty) -> String {
        prose_at(s, st, self.caps.depth)
    }

    fn prose_width(&self, s: &str) -> usize {
        prose_width(s, self.caps.depth)
    }

    // ---- plain output ----

    pub fn line(&self, s: &str) {
        self.said(true);
        println!("{PAD}{s}");
    }

    pub fn blank(&self) {
        println!();
    }

    /// A wrapped paragraph.
    pub fn para(&self, text: &str, st: Sty) {
        self.said(true);
        for l in wrap(text, self.text_width()) {
            println!("{PAD}{}", self.prose(&l, st));
        }
    }

    /// A wrapped paragraph with a hanging indent after `lead`.
    pub fn item(&self, lead: &str, lead_st: Sty, text: &str, st: Sty) {
        self.said(true);
        let lw = width(lead) + 1;
        let lines = wrap(text, self.text_width().saturating_sub(lw));
        for (i, l) in lines.iter().enumerate() {
            let head = if i == 0 {
                format!("{} ", self.paint(lead, lead_st))
            } else {
                " ".repeat(lw)
            };
            println!("{PAD}{head}{}", self.prose(l, st));
        }
    }

    pub fn ok(&self, text: &str) {
        self.item("✓", S_OK, text, S_TEXT);
    }

    pub fn todo(&self, text: &str) {
        self.item("○", S_HINT, text, S_HINT);
    }

    pub fn warn(&self, text: &str) {
        self.item("!", S_WARN.bold(), text, S_WARN);
    }

    pub fn err(&self, text: &str) {
        self.item("✗", S_ERR.bold(), text, S_ERR);
    }

    pub fn hint(&self, text: &str) {
        self.para(text, S_HINT);
    }

    /// `label   value` rows with the values in one column; long values
    /// wrap under themselves.
    pub fn rows(&self, rows: &[Row]) {
        self.said(true);
        let lw = rows.iter().map(|r| width(&r.label)).max().unwrap_or(0) + 3;
        for r in rows {
            let lead = if r.mark.is_empty() {
                String::new()
            } else {
                format!("{} ", self.paint(r.mark, r.mark_st))
            };
            let vw = self
                .text_width()
                .saturating_sub(lw + 2 + width(&lead))
                .max(16);
            let label = format!("{:<w$}", r.label, w = lw);
            for (i, l) in wrap(&r.value, vw).iter().enumerate() {
                let head = if i == 0 {
                    format!("{lead}{}", self.paint(&label, S_HINT))
                } else {
                    " ".repeat(width(&lead) + lw)
                };
                println!("{PAD}  {head}{}", self.prose(l, r.st));
            }
        }
    }

    /// Commands to type, each with what it does beside it; a long
    /// description wraps under itself.
    pub fn commands(&self, cmds: &[(&str, &str)]) {
        self.said(true);
        for l in self.command_lines(cmds, self.text_width()) {
            println!("{l}");
        }
    }

    fn command_lines(&self, cmds: &[(&str, &str)], text_width: usize) -> Vec<String> {
        let cw = cmds.iter().map(|(c, _)| width(c)).max().unwrap_or(0) + 3;
        let dw = text_width.saturating_sub(cw).max(16);
        let mut out = Vec::new();
        for (c, d) in cmds {
            for (i, l) in wrap(d, dw).iter().enumerate() {
                let head = if i == 0 {
                    self.paint(&format!("{c:<cw$}"), S_ACCENT)
                } else {
                    " ".repeat(cw)
                };
                out.push(format!("{PAD}{head}{}", self.prose(l, S_HINT)));
            }
        }
        out
    }

    /// Put `text` on the clipboard of the computer the terminal runs on
    /// (OSC 52: most terminals allow it, over SSH and in tmux too).
    pub fn copy_to_clipboard(&self, text: &str) {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(text);
        self.out(&format!("\x1b]52;c;{b64}\x07"));
    }

    // ---- pages ----

    /// Start a page of the setup: a clean screen (in a capable terminal),
    /// the brand line, the run's steps as a segmented bar with their names,
    /// then the page's title and what it is for.
    pub fn page(&mut self, p: &Page<'_>) {
        self.clear_screen();
        for l in self.page_lines(p) {
            println!("{l}");
        }
    }

    /// A clean screen for a new page (only in a capable terminal; what was
    /// there stays in the scrollback).
    pub fn clear_screen(&mut self) {
        self.said(false);
        if self.caps.fancy {
            self.out("\x1b[H\x1b[2J");
            self.live = 0;
        }
    }

    pub fn page_lines(&self, p: &Page<'_>) -> Vec<String> {
        let at = p.steps.iter().position(|(_, m)| *m == Mark::Now);
        let count = at.map(|i| format!("Step {} of {}", i + 1, p.steps.len()));
        if !self.caps.fancy {
            let mut out = vec![String::new()];
            out.push(match &count {
                Some(c) => format!("{PAD}{c} · {}", self.paint(p.title, S_TEXT.bold())),
                None => format!("{PAD}{}", self.paint(p.title, S_TEXT.bold())),
            });
            if !p.sub.is_empty() {
                out.extend(
                    wrap(p.sub, self.text_width())
                        .into_iter()
                        .map(|l| format!("{PAD}{l}")),
                );
            }
            out.push(String::new());
            return out;
        }
        let w = self.text_width();
        let mut out = vec![String::new()];
        let brand = format!(
            "{}  {}",
            self.paint("▀█▀", S_ACCENT.bold()),
            self.paint("Tessera setup", S_TEXT.bold())
        );
        let right = count.unwrap_or_default();
        let gap = w.saturating_sub(width_of(&brand) + width_of(&right)).max(2);
        out.push(format!(
            "{PAD}{brand}{}{}",
            " ".repeat(gap),
            self.paint(&right, S_HINT)
        ));
        if !p.steps.is_empty() {
            out.push(String::new());
            let n = p.steps.len();
            let seg = (w.saturating_sub(2 * (n - 1)) / n).max(4);
            let mut bar = Vec::new();
            let mut names = Vec::new();
            for (name, m) in p.steps {
                let (glyph, st, mark, mst, nst) = match m {
                    Mark::Done => ("━", S_OK, "✓", S_OK, S_SOFT),
                    Mark::Now => ("━", S_ACCENT, "●", S_ACCENT, S_TEXT.bold()),
                    Mark::Skipped => ("─", S_FAINT, "–", S_HINT, S_HINT),
                    Mark::Next => ("─", S_FAINT, "○", S_FAINT, S_HINT),
                };
                bar.push(self.paint(&glyph.repeat(seg), st));
                let label = fit(name, seg.saturating_sub(2));
                let pad = seg.saturating_sub(2 + width_of(&label));
                names.push(format!(
                    "{} {}{}",
                    self.paint(mark, mst),
                    self.paint(&label, nst),
                    " ".repeat(pad)
                ));
            }
            out.push(format!("{PAD}{}", bar.join("  ")));
            out.push(format!("{PAD}{}", names.join("  ")));
        }
        out.push(String::new());
        out.push(format!("{PAD}{}", self.paint(p.title, S_TEXT.bold())));
        if !p.sub.is_empty() {
            out.extend(
                wrap(p.sub, w)
                    .into_iter()
                    .map(|l| format!("{PAD}{}", self.prose(&l, S_HINT))),
            );
        }
        out.push(String::new());
        out
    }

    /// `rows` in a rounded card (a summary, what is set up).
    pub fn card(&self, rows: &[Row]) {
        self.said(true);
        for l in self.card_lines(rows) {
            println!("{l}");
        }
    }

    pub fn card_lines(&self, rows: &[Row]) -> Vec<String> {
        let w = self.text_width();
        let inner = w.saturating_sub(4);
        let lw = rows.iter().map(|r| width_of(&r.label)).max().unwrap_or(0) + 3;
        let mut body = Vec::new();
        for r in rows {
            let lead = if r.mark.is_empty() {
                String::new()
            } else {
                format!("{} ", self.paint(r.mark, r.mark_st))
            };
            let lead_w = if r.mark.is_empty() {
                0
            } else {
                width_of(r.mark) + 1
            };
            let vw = inner.saturating_sub(lw + lead_w).max(10);
            for (i, l) in wrap(&r.value, vw).iter().enumerate() {
                let head = if i == 0 {
                    format!("{lead}{}", self.paint(&format!("{:<lw$}", r.label), S_HINT))
                } else {
                    " ".repeat(lead_w + lw)
                };
                let used = lead_w + lw + self.prose_width(l);
                body.push(format!(
                    "{head}{}{}",
                    self.prose(l, r.st),
                    " ".repeat(inner.saturating_sub(used))
                ));
            }
        }
        if !self.caps.fancy {
            return body
                .into_iter()
                .map(|l| format!("{PAD}  {}", l.trim_end()))
                .collect();
        }
        let edge = |s: &str| self.paint(s, S_FAINT);
        let mut out = vec![format!(
            "{PAD}{}",
            edge(&format!("╭{}╮", "─".repeat(w - 2)))
        )];
        for l in body {
            out.push(format!("{PAD}{} {l} {}", edge("│"), edge("│")));
        }
        out.push(format!(
            "{PAD}{}",
            edge(&format!("╰{}╯", "─".repeat(w - 2)))
        ));
        out
    }

    /// The lines of a list of choices: the question, the choices (each
    /// with its one-line explanation when they fit, else the highlighted
    /// one's), and the keys.
    #[allow(clippy::too_many_arguments)]
    pub fn select_lines(
        &self,
        question: &str,
        choices: &[Choice],
        visible: &[usize],
        sel: usize,
        top: usize,
        rows: usize,
        query: Option<&str>,
    ) -> Vec<String> {
        let w = self.text_width();
        let screen = crossterm::terminal::size()
            .map(|(_, h)| h as usize)
            .unwrap_or(24);
        // Every choice explained when the list is short and there's room.
        let all_hints = query.is_none()
            && visible.len() <= 6
            && visible.len() * 2 + 6 <= screen.saturating_sub(8).max(14);
        let mut lines = vec![self.question_line(question), String::new()];
        if let Some(q) = query {
            lines.push(format!(
                "{PAD}  {} {q}{}",
                self.paint("⌕", S_HINT),
                self.paint("▏", S_ACCENT)
            ));
        }
        if top > 0 {
            lines.push(format!("{PAD}    {}", self.paint("↑ more", S_FAINT)));
        }
        for &i in visible.iter().skip(top).take(rows) {
            let c = &choices[i];
            let on = i == sel;
            let bar = if on {
                (" ▌ ".to_string(), S_ACCENT)
            } else {
                ("   ".to_string(), Sty::plain())
            };
            let tag_st = match c.tag.as_str() {
                "recommended" | "ready" | "found" | "signed in" => S_OK,
                _ => S_HINT,
            };
            let label_st = if on { S_TEXT.bold() } else { S_SOFT };
            let tag_w = if c.tag.is_empty() {
                0
            } else {
                width_of(&c.tag) + 2
            };
            let label = fit(&c.label, w.saturating_sub(4 + tag_w));
            let gap = w.saturating_sub(3 + width_of(&label) + tag_w + 1);
            let mut pieces = vec![bar.clone(), (label, label_st)];
            if !c.tag.is_empty() {
                pieces.push((" ".repeat(gap + 2), Sty::plain()));
                pieces.push((c.tag.clone(), tag_st));
            }
            lines.push(format!("{PAD}{}", self.band(&pieces, w, on)));
            if !c.hint.is_empty() && (on || all_hints) {
                let hint = fit(&c.hint, w.saturating_sub(4));
                let pieces = vec![
                    (
                        if on {
                            " ▌ ".to_string()
                        } else {
                            "   ".to_string()
                        },
                        bar.1,
                    ),
                    (hint, if on { S_SOFT } else { S_FAINT }),
                ];
                lines.push(format!("{PAD}{}", self.band(&pieces, w, on)));
            }
        }
        if visible.len() > top + rows {
            lines.push(format!(
                "{PAD}    {}",
                self.paint(&format!("↓ {} more", visible.len() - top - rows), S_FAINT)
            ));
        }
        if visible.is_empty() {
            lines.push(format!(
                "{PAD}    {}",
                self.paint("nothing matches", S_HINT)
            ));
        }
        lines.push(String::new());
        let keys: &[(&str, &str)] = if query.is_some() {
            &[
                ("type", "to search"),
                ("↑↓", "move"),
                ("⏎", "choose"),
                ("esc", self.esc_word),
            ]
        } else {
            &[("↑↓", "move"), ("⏎", "choose"), ("esc", self.esc_word)]
        };
        lines.push(self.keys_line(keys));
        lines
    }

    fn said(&self, on: bool) {
        self.said.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Before a new page clears the screen: when something was said since
    /// the last answer, wait for ⏎ so it can be read. Line-based output
    /// keeps everything on screen and doesn't wait.
    pub async fn pause(&mut self) -> Result<()> {
        if !self.caps.fancy || !self.said.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        let _raw = RawGuard::new(false, true);
        println!();
        self.draw_live(&[self.keys_line(&[("⏎", "continue")])]);
        loop {
            if let Ev::Key(k) = self.next(None).await?
                && matches!(k.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' '))
            {
                break;
            }
        }
        self.clear_live();
        self.said(false);
        Ok(())
    }

    // ---- live region ----

    fn out(&self, s: &str) {
        let mut o = std::io::stdout();
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }

    /// Replace the live region with `lines` (each truncated to one row).
    pub fn draw_live(&mut self, lines: &[String]) {
        let cols = self.cols().saturating_sub(1);
        let mut s = String::from("\x1b[?2026h");
        if self.live > 0 {
            s.push_str(&format!("\r\x1b[{}A", self.live));
        }
        s.push_str("\r\x1b[J");
        for l in lines {
            s.push_str(&fit(l, cols));
            s.push_str("\r\n");
        }
        s.push_str("\x1b[?2026l");
        self.out(&s);
        self.live = lines.len();
    }

    pub fn clear_live(&mut self) {
        if self.live > 0 {
            self.out(&format!("\r\x1b[{}A\x1b[J", self.live));
            self.live = 0;
        }
    }

    /// Next input event; `tick` also wakes up for animation.
    pub(super) async fn next(&mut self, tick: Option<Duration>) -> Result<Ev> {
        let stream = self.events.get_or_insert_with(EventStream::new);
        loop {
            let ev = match tick {
                Some(t) => match tokio::time::timeout(t, stream.next()).await {
                    Ok(ev) => ev,
                    Err(_) => return Ok(Ev::Tick),
                },
                None => stream.next().await,
            };
            match ev {
                Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => {
                    if k.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('d'))
                    {
                        return Err(Aborted.into());
                    }
                    return Ok(Ev::Key(k));
                }
                Some(Ok(Event::Paste(s))) => return Ok(Ev::Paste(s)),
                Some(Ok(Event::Resize(..))) => return Ok(Ev::Redraw),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => return Err(Aborted.into()),
            }
        }
    }

    fn question_line(&self, q: &str) -> String {
        format!("{PAD}{}", self.paint(q, S_TEXT.bold()))
    }

    /// Key hints: each key a step brighter than what it does.
    pub fn keys_line(&self, keys: &[(&str, &str)]) -> String {
        let parts: Vec<String> = keys
            .iter()
            .map(|(k, d)| {
                format!(
                    "{} {}",
                    self.paint(k, S_SOFT.bold()),
                    self.paint(d, S_FAINT)
                )
            })
            .collect();
        format!("{PAD}{}", parts.join("   "))
    }

    /// `pieces` on one row `width` cells wide, on the selection band where
    /// the terminal has the colours for it.
    fn band(&self, pieces: &[(String, Sty)], width: usize, on: bool) -> String {
        let used: usize = pieces.iter().map(|(t, _)| self.prose_width(t)).sum();
        let fill = width.saturating_sub(used);
        let banded = on && self.caps.depth >= Depth::Ansi256;
        let mut out = String::new();
        for (t, st) in pieces {
            let st = if banded { st.on(BAND) } else { *st };
            out.push_str(&self.prose(t, st));
        }
        if banded {
            out.push_str(&self.paint(&" ".repeat(fill), Sty::plain().on(BAND)));
        }
        out
    }

    /// The one-line record a finished prompt leaves behind. Written with
    /// `\r\n`: widgets call it while raw mode is still on.
    fn answered(&self, q: &str, answer: Option<&str>) {
        self.said(false);
        let line = match answer {
            Some(a) => format!(
                "{PAD}{} {} {} {}",
                self.paint("✓", S_OK),
                self.paint(q, S_HINT),
                self.paint("›", S_FAINT),
                self.paint(a, S_ACCENT)
            ),
            None => format!(
                "{PAD}{} {} {} {}",
                self.paint("○", S_HINT),
                self.paint(q, S_HINT),
                self.paint("›", S_FAINT),
                self.paint("skipped", S_HINT)
            ),
        };
        let cols = self.cols().saturating_sub(1);
        // A blank line after it: the next question or result stands apart.
        self.out(&format!("{}\r\n\r\n", fit(&line, cols)));
    }

    // ---- line-based fallbacks ----

    async fn read_line(&self) -> Result<Option<String>> {
        let _ = std::io::stdout().flush();
        let eof = self.eof.clone();
        let r = tokio::task::spawn_blocking(move || {
            let mut s = String::new();
            match std::io::stdin().read_line(&mut s) {
                Ok(0) | Err(_) => {
                    eof.store(true, std::sync::atomic::Ordering::SeqCst);
                    None
                }
                Ok(_) => Some(s.trim_end_matches(['\r', '\n']).to_string()),
            }
        })
        .await?;
        Ok(r)
    }

    // ---- widgets ----

    /// Pick one of `choices`. `None`: skipped (esc, or `s` in plain mode).
    pub async fn select(
        &mut self,
        question: &str,
        choices: &[Choice],
        default: usize,
    ) -> Result<Option<usize>> {
        self.select_opts(question, choices, default, false).await
    }

    /// `filter`: typing narrows the list (for long lists such as models).
    pub async fn select_opts(
        &mut self,
        question: &str,
        choices: &[Choice],
        default: usize,
        filter: bool,
    ) -> Result<Option<usize>> {
        if choices.is_empty() {
            return Ok(None);
        }
        let default = default.min(choices.len() - 1);
        if !self.caps.fancy {
            return self.select_plain(question, choices, default).await;
        }
        let _raw = RawGuard::new(false, true);
        let mut sel = default;
        let mut query = String::new();
        let mut top = 0usize;
        loop {
            let visible: Vec<usize> = (0..choices.len())
                .filter(|i| matches_query(&choices[*i], &query))
                .collect();
            if let Some(p) = visible.iter().position(|i| *i == sel) {
                sel = visible[p];
            } else if let Some(first) = visible.first() {
                sel = *first;
            }
            let pos = visible.iter().position(|i| *i == sel).unwrap_or(0);
            let rows = crossterm::terminal::size()
                .map(|(_, h)| h as usize)
                .unwrap_or(24)
                .saturating_sub(10)
                .clamp(3, 12);
            if pos < top {
                top = pos;
            } else if pos >= top + rows {
                top = pos + 1 - rows;
            }
            top = top.min(visible.len().saturating_sub(rows));
            let lines = self.select_lines(
                question,
                choices,
                &visible,
                sel,
                top,
                rows,
                filter.then_some(query.as_str()),
            );
            self.draw_live(&lines);
            let ev = self.next(None).await?;
            if visible.is_empty()
                && let Ev::Key(k) = &ev
                && !matches!(k.code, KeyCode::Esc | KeyCode::Backspace | KeyCode::Char(_))
            {
                continue;
            }
            match ev {
                Ev::Key(k) => match k.code {
                    KeyCode::Up | KeyCode::BackTab => {
                        if pos > 0 {
                            sel = visible[pos - 1];
                        } else if let Some(l) = visible.last() {
                            sel = *l;
                        }
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        if pos + 1 < visible.len() {
                            sel = visible[pos + 1];
                        } else if let Some(f) = visible.first() {
                            sel = *f;
                        }
                    }
                    KeyCode::Char('k') if !filter => {
                        sel = visible[pos.saturating_sub(1)];
                    }
                    KeyCode::Char('j') if !filter => {
                        sel = visible[(pos + 1).min(visible.len().saturating_sub(1))];
                    }
                    KeyCode::PageUp => sel = visible[pos.saturating_sub(rows)],
                    KeyCode::PageDown => {
                        sel = visible[(pos + rows).min(visible.len().saturating_sub(1))]
                    }
                    KeyCode::Home if !visible.is_empty() => sel = visible[0],
                    KeyCode::End if !visible.is_empty() => sel = visible[visible.len() - 1],
                    KeyCode::Char(c @ '1'..='9') if !filter => {
                        let n = c as usize - '1' as usize;
                        if let Some(i) = visible.get(n) {
                            sel = *i;
                            self.clear_live();
                            self.answered(question, Some(&choices[sel].label));
                            return Ok(Some(sel));
                        }
                    }
                    KeyCode::Backspace if filter => {
                        query.pop();
                    }
                    KeyCode::Char('u') if filter && k.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.clear()
                    }
                    KeyCode::Char(c) if filter && !k.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(c);
                        top = 0;
                    }
                    KeyCode::Enter if !visible.is_empty() => {
                        self.clear_live();
                        self.answered(question, Some(&choices[sel].label));
                        return Ok(Some(sel));
                    }
                    KeyCode::Esc => {
                        if filter && !query.is_empty() {
                            query.clear();
                            continue;
                        }
                        self.clear_live();
                        self.answered(question, None);
                        return Ok(None);
                    }
                    _ => {}
                },
                Ev::Paste(s) if filter => query.push_str(s.lines().next().unwrap_or("")),
                _ => {}
            }
        }
    }

    async fn select_plain(
        &mut self,
        question: &str,
        choices: &[Choice],
        default: usize,
    ) -> Result<Option<usize>> {
        println!("{PAD}{}", self.paint(question, Sty::plain().bold()));
        for (i, c) in choices.iter().enumerate() {
            let tag = if c.tag.is_empty() {
                String::new()
            } else {
                format!(" ({})", c.tag)
            };
            println!("{PAD}  {}) {}{tag}", i + 1, c.label);
            if !c.hint.is_empty() {
                for h in wrap(&c.hint, self.text_width().saturating_sub(7)) {
                    println!("{PAD}     {}", self.paint(&h, S_HINT));
                }
            }
        }
        for _ in 0..5 {
            print!(
                "{PAD}Choose 1-{} (enter = {}, s = skip): ",
                choices.len(),
                default + 1
            );
            let Some(line) = self.read_line().await? else {
                println!();
                println!("{PAD}(no input — skipped)");
                return Ok(None);
            };
            let t = line.trim().to_ascii_lowercase();
            if t.is_empty() {
                return Ok(Some(default));
            }
            if matches!(t.as_str(), "s" | "skip" | "q" | "n" | "no") {
                return Ok(None);
            }
            if let Ok(n) = t.parse::<usize>()
                && (1..=choices.len()).contains(&n)
            {
                return Ok(Some(n - 1));
            }
            println!("{PAD}Please type a number from 1 to {}.", choices.len());
        }
        Ok(None)
    }

    /// Yes/no. `None`: skipped.
    pub async fn confirm(&mut self, question: &str, default: bool) -> Result<Option<bool>> {
        if !self.caps.fancy {
            print!(
                "{PAD}{} [{}]: ",
                self.paint(question, Sty::plain().bold()),
                if default { "Y/n" } else { "y/N" }
            );
            let Some(line) = self.read_line().await? else {
                println!();
                return Ok(None);
            };
            return Ok(Some(match line.trim().to_ascii_lowercase().as_str() {
                "" => default,
                "y" | "yes" | "j" | "ja" | "o" | "oui" | "s" | "si" => true,
                _ => false,
            }));
        }
        let choices = [Choice::new("Yes"), Choice::new("No")];
        let r = self
            .select(question, &choices, if default { 0 } else { 1 })
            .await?;
        Ok(r.map(|i| i == 0))
    }

    /// A line of text. `validate` returns an error message to show.
    pub async fn input(
        &mut self,
        question: &str,
        default: &str,
        placeholder: &str,
        validate: impl Fn(&str) -> Result<(), String>,
    ) -> Result<Option<String>> {
        if !self.caps.fancy {
            loop {
                let d = if default.is_empty() {
                    String::new()
                } else {
                    format!(" [{default}]")
                };
                print!("{PAD}{}{d}: ", self.paint(question, Sty::plain().bold()));
                let Some(line) = self.read_line().await? else {
                    println!();
                    return Ok(None);
                };
                let v = if line.trim().is_empty() {
                    default.to_string()
                } else {
                    line.trim().to_string()
                };
                if v.is_empty() {
                    return Ok(None);
                }
                match validate(&v) {
                    Ok(()) => return Ok(Some(v)),
                    Err(e) => println!("{PAD}{}", self.paint(&format!("✗ {e}"), S_ERR)),
                }
            }
        }
        let _raw = RawGuard::new(true, true);
        let mut ed = LineEdit::new(default);
        let mut error: Option<String> = None;
        loop {
            let (before, after) = ed.split();
            let body = if ed.text.is_empty() && !placeholder.is_empty() {
                format!(
                    "{}{}",
                    self.paint(" ", Sty::plain().reverse()),
                    self.paint(placeholder, S_FAINT)
                )
            } else {
                let mut a = after.chars();
                let cur = a.next().map(|c| c.to_string()).unwrap_or(" ".into());
                format!(
                    "{before}{}{}",
                    self.paint(&cur, Sty::plain().reverse()),
                    a.as_str()
                )
            };
            let foot = match &error {
                Some(e) => self.paint(&format!("✗ {e}"), S_ERR),
                None => self
                    .keys_line(&[("⏎", "confirm"), ("esc", "skip"), ("ctrl+u", "clear")])
                    .trim_start()
                    .to_string(),
            };
            let lines = vec![
                self.question_line(question),
                format!("{PAD}  {} {body}", self.paint("›", S_ACCENT)),
                format!("{PAD}  {foot}"),
            ];
            self.draw_live(&lines);
            match self.next(None).await? {
                Ev::Key(k) => {
                    if k.code == KeyCode::Enter {
                        let v = ed.text.trim().to_string();
                        if v.is_empty() {
                            self.clear_live();
                            self.answered(question, None);
                            return Ok(None);
                        }
                        match validate(&v) {
                            Ok(()) => {
                                self.clear_live();
                                self.answered(question, Some(&v));
                                return Ok(Some(v));
                            }
                            Err(e) => error = Some(e),
                        }
                    } else if k.code == KeyCode::Esc {
                        self.clear_live();
                        self.answered(question, None);
                        return Ok(None);
                    } else {
                        ed.key(k);
                        error = None;
                    }
                }
                Ev::Paste(s) => {
                    ed.insert(s.lines().next().unwrap_or("").trim());
                    error = None;
                }
                _ => {}
            }
        }
    }

    /// Hidden input: characters are never echoed, only counted. The value
    /// is wiped from memory when dropped.
    pub async fn secret(&mut self, question: &str) -> Result<Option<Zeroizing<String>>> {
        if !self.caps.stdin_tty {
            // Piped input: read a line (nothing is echoed by us).
            println!("{PAD}{}:", self.paint(question, Sty::plain().bold()));
            let line = self.read_line().await?;
            return Ok(line
                .map(|l| Zeroizing::new(l.trim().to_string()))
                .filter(|l| !l.is_empty()));
        }
        let fancy = self.caps.fancy;
        let _raw = RawGuard::new(true, fancy);
        let mut value = Zeroizing::new(String::new());
        if !fancy {
            self.out(&format!(
                "{PAD}{} (hidden; press enter when done): ",
                question
            ));
        }
        loop {
            if fancy {
                let n = value.chars().count();
                let dots = if n == 0 {
                    self.paint("paste or type it — it stays hidden", S_FAINT)
                } else {
                    format!(
                        "{}  {}",
                        self.paint(&"•".repeat(n.min(32)), S_ACCENT),
                        self.paint(&format!("{n} characters"), S_HINT)
                    )
                };
                let lines = vec![
                    self.question_line(question),
                    format!("{PAD}  {} {dots}", self.paint("›", S_ACCENT)),
                    format!(
                        "{PAD}  {}",
                        self.keys_line(&[("⏎", "confirm"), ("esc", "skip"), ("ctrl+u", "clear")])
                            .trim_start()
                    ),
                ];
                self.draw_live(&lines);
            }
            match self.next(None).await? {
                Ev::Key(k) => match k.code {
                    KeyCode::Enter => {
                        let n = value.trim().chars().count();
                        if fancy {
                            self.clear_live();
                        } else {
                            self.out("\r\n");
                        }
                        if n == 0 {
                            if fancy {
                                self.answered(question, None);
                            }
                            return Ok(None);
                        }
                        let shown = format!("hidden · {n} characters");
                        if fancy {
                            self.answered(question, Some(&shown));
                        } else {
                            self.out(&format!("{PAD}({shown})\r\n"));
                        }
                        let trimmed = Zeroizing::new(value.trim().to_string());
                        return Ok(Some(trimmed));
                    }
                    KeyCode::Esc => {
                        if fancy {
                            self.clear_live();
                            self.answered(question, None);
                        } else {
                            self.out("\r\n");
                        }
                        return Ok(None);
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear()
                    }
                    KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.push(c)
                    }
                    _ => {}
                },
                Ev::Paste(s) => value.push_str(s.trim_matches(['\r', '\n'])),
                _ => {}
            }
        }
    }

    /// Run `fut` behind a tesserae spinner. Ctrl+C stops the wizard.
    pub async fn spin<F: Future>(&mut self, label: &str, fut: F) -> Result<F::Output> {
        match self.spin_inner(label, fut, false).await? {
            Some(v) => Ok(v),
            None => Err(Aborted.into()),
        }
    }

    /// Like `spin`, but esc stops waiting (`None`).
    pub async fn spin_esc<F: Future>(&mut self, label: &str, fut: F) -> Result<Option<F::Output>> {
        self.spin_inner(label, fut, true).await
    }

    async fn spin_inner<F: Future>(
        &mut self,
        label: &str,
        fut: F,
        esc: bool,
    ) -> Result<Option<F::Output>> {
        if !self.caps.fancy {
            println!("{PAD}{label}");
            return Ok(Some(fut.await));
        }
        let _raw = RawGuard::new(false, true);
        tokio::pin!(fut);
        let mut frame = 0usize;
        loop {
            let glyph = if self.caps.anim {
                self.paint(SPIN[frame % 4], S_ACCENT)
            } else {
                self.paint("…", S_ACCENT)
            };
            let hint = if esc {
                format!("  {}", self.paint("esc to stop waiting", S_FAINT))
            } else {
                String::new()
            };
            self.draw_live(&[format!("{PAD}{glyph} {}{hint}", self.paint(label, S_TEXT))]);
            let tick = Duration::from_millis(90);
            let done = tokio::select! {
                out = &mut fut => Some(Some(out)),
                ev = self.next(Some(tick)) => match ev? {
                    Ev::Key(k) if esc && k.code == KeyCode::Esc => Some(None),
                    _ => None,
                },
            };
            match done {
                Some(out) => {
                    self.clear_live();
                    return Ok(out);
                }
                None => frame += 1,
            }
        }
    }

    /// Wait for Enter (or Esc) while `poll` runs every `every`; `poll`
    /// returning `Some(msg)` prints it and stops waiting. With `copy`, the
    /// `c` key puts that text on the clipboard.
    pub async fn wait_enter<F, Fut>(
        &mut self,
        label: &str,
        every: Duration,
        copy: Option<&str>,
        mut poll: F,
    ) -> Result<Option<String>>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        if !self.caps.fancy {
            print!("{PAD}{label} — press enter to continue: ");
            let _ = self.read_line().await?;
            return Ok(poll().await);
        }
        let _raw = RawGuard::new(false, true);
        let mut frame = 0usize;
        let mut last = std::time::Instant::now();
        let mut copied: Option<std::time::Instant> = None;
        loop {
            let glyph = if self.caps.anim {
                self.paint(SPIN[frame % 4], S_ACCENT)
            } else {
                self.paint("…", S_ACCENT)
            };
            let keys = match (copy, copied) {
                (Some(_), Some(at)) if at.elapsed() < Duration::from_secs(3) => {
                    self.paint("copied to the clipboard", S_OK)
                }
                (Some(_), _) => self.paint("c copy link · enter done", S_FAINT),
                (None, _) => self.paint("enter to continue", S_FAINT),
            };
            self.draw_live(&[format!(
                "{PAD}{glyph} {}  {keys}",
                self.paint(label, S_TEXT)
            )]);
            if last.elapsed() >= every {
                last = std::time::Instant::now();
                if let Some(msg) = poll().await {
                    self.clear_live();
                    return Ok(Some(msg));
                }
            }
            match self.next(Some(Duration::from_millis(120))).await? {
                Ev::Key(k) if matches!(k.code, KeyCode::Enter | KeyCode::Esc) => {
                    self.clear_live();
                    return Ok(None);
                }
                Ev::Key(k) if k.code == KeyCode::Char('c') && copy.is_some() => {
                    self.copy_to_clipboard(copy.unwrap_or_default());
                    copied = Some(std::time::Instant::now());
                }
                _ => frame += 1,
            }
        }
    }

    /// Show `label` with a spinner and the seconds waited while `poll` runs
    /// every `every`, until it returns `Some`, `limit` passes (`Ok(None)`)
    /// or esc is pressed (`Ok(None)` as well).
    pub async fn watch<T, F, Fut>(
        &mut self,
        label: &str,
        every: Duration,
        limit: Duration,
        mut poll: F,
    ) -> Result<Option<T>>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        let start = std::time::Instant::now();
        if !self.caps.fancy {
            println!("{PAD}{label}");
            while start.elapsed() < limit {
                if let Some(v) = poll().await {
                    return Ok(Some(v));
                }
                tokio::time::sleep(every).await;
            }
            return Ok(None);
        }
        let _raw = RawGuard::new(false, true);
        let mut frame = 0usize;
        let mut last: Option<std::time::Instant> = None;
        loop {
            let glyph = if self.caps.anim {
                self.paint(SPIN[frame % 4], S_ACCENT)
            } else {
                self.paint("…", S_ACCENT)
            };
            let secs = start.elapsed().as_secs();
            self.draw_live(&[format!(
                "{PAD}{glyph} {}  {}  {}",
                self.paint(label, S_TEXT),
                self.paint(&format!("{secs}s"), S_HINT),
                self.paint("esc to stop waiting", S_FAINT)
            )]);
            if last.is_none_or(|l| l.elapsed() >= every) {
                last = Some(std::time::Instant::now());
                if let Some(v) = poll().await {
                    self.clear_live();
                    return Ok(Some(v));
                }
            }
            if start.elapsed() >= limit {
                self.clear_live();
                return Ok(None);
            }
            match self.next(Some(Duration::from_millis(120))).await? {
                Ev::Key(k) if k.code == KeyCode::Esc => {
                    self.clear_live();
                    return Ok(None);
                }
                _ => frame += 1,
            }
        }
    }

    // ---- live steps (long server tasks) ----

    /// Begin a list of steps where only the current one animates.
    pub fn steps_start(&mut self) -> Steps {
        Steps {
            current: None,
            frame: 0,
        }
    }

    pub fn steps_draw(&mut self, s: &mut Steps) {
        if !self.caps.fancy {
            return;
        }
        let lines = match &s.current {
            Some(c) => {
                let glyph = if self.caps.anim {
                    self.paint(SPIN[s.frame % 4], S_ACCENT)
                } else {
                    self.paint("…", S_ACCENT)
                };
                vec![format!("{PAD}  {glyph} {}", self.paint(c, S_TEXT))]
            }
            None => Vec::new(),
        };
        self.draw_live(&lines);
    }

    /// Mark the current step done and start `next`.
    pub fn steps_next(&mut self, s: &mut Steps, next: Option<String>) {
        if self.caps.fancy {
            self.clear_live();
        }
        if let Some(prev) = s.current.take() {
            self.out(&format!("{PAD}  {} {prev}\r\n", self.paint("✓", S_OK)));
        }
        if let Some(n) = &next
            && !self.caps.fancy
        {
            self.out(&format!("{PAD}  … {n}\r\n"));
        }
        s.current = next;
        self.steps_draw(s);
    }

    pub fn steps_fail(&mut self, s: &mut Steps, msg: &str) {
        if self.caps.fancy {
            self.clear_live();
        }
        if let Some(prev) = s.current.take() {
            println!("{PAD}  {} {prev}", self.paint("✗", S_ERR));
        }
        self.err(msg);
    }

    /// Animation tick / Ctrl+C check while a long task runs.
    pub async fn steps_tick(&mut self, s: &mut Steps) -> Result<()> {
        if !self.caps.fancy {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Ok(());
        }
        let _raw = RawGuard::new(false, true);
        let _ = self.next(Some(Duration::from_millis(90))).await?;
        s.frame += 1;
        self.steps_draw(s);
        Ok(())
    }

    /// Keep what the live region shows as ordinary output.
    pub fn commit_live(&mut self) {
        self.live = 0;
    }
}

pub struct Steps {
    current: Option<String>,
    frame: usize,
}

fn matches_query(c: &Choice, q: &str) -> bool {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = format!("{} {} {}", c.label, c.tag, c.hint).to_lowercase();
    q.split_whitespace().all(|w| hay.contains(w))
}

/// Fit a (possibly styled) line into `cols` display columns.
fn fit(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_string();
    }
    // Walk the string keeping escapes, counting visible columns.
    let mut out = String::new();
    let mut w = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if chars.peek() == Some(&'[') {
                for c in chars.by_ref() {
                    out.push(c);
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > cols {
            out.push('…');
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push_str("\x1b[0m");
    out
}

/// Minimal single-line editor for `Term::input`.
struct LineEdit {
    text: String,
    /// Byte offset.
    cursor: usize,
}

impl LineEdit {
    fn new(s: &str) -> Self {
        Self {
            text: s.to_string(),
            cursor: s.len(),
        }
    }

    fn split(&self) -> (&str, &str) {
        self.text.split_at(self.cursor)
    }

    fn insert(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn prev(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }

    fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => self.text.truncate(self.cursor),
            KeyCode::Char('w') if ctrl => {
                let before = &self.text[..self.cursor];
                let trimmed = before.trim_end();
                let start = trimmed
                    .rfind([' ', '/', '.', ':'])
                    .map(|i| i + 1)
                    .unwrap_or(0);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl => self.insert(&c.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                let p = self.prev();
                self.text.drain(p..self.cursor);
                self.cursor = p;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let n = self.next();
                self.text.drain(self.cursor..n);
            }
            KeyCode::Left => self.cursor = self.prev(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(vars: &[(&str, &str)], tty: bool, plain: bool, no_anim: bool) -> Caps {
        let map: std::collections::HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Caps::from_env(|k| map.get(k).cloned(), tty, tty, plain, no_anim)
    }

    #[test]
    fn light_terminals_get_light_tones() {
        assert_eq!(adapt_for(GOLD, true), Rgb(0x9C, 0x74, 0x24));
        assert_eq!(adapt_for(STONE[5], true), LIGHT_STONE[5], "text turns dark");
        // A dark stone tile becomes a pale one.
        let pale = adapt_for(Rgb(0x3a, 0x36, 0x31), true);
        assert!(pale.0 > 180 && pale.1 > 170, "{pale:?}");
        assert_eq!(adapt_for(GOLD, false), GOLD);
    }

    #[test]
    fn capabilities_degrade() {
        let c = caps(
            &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
            true,
            false,
            false,
        );
        assert_eq!((c.depth, c.fancy, c.anim), (Depth::True, true, true));
        let c = caps(&[("TERM", "xterm-256color")], true, false, false);
        assert_eq!((c.depth, c.anim), (Depth::Ansi256, true));
        let c = caps(&[("TERM", "xterm")], true, false, false);
        assert_eq!(c.depth, Depth::Ansi16);
        let c = caps(
            &[("TERM", "xterm-256color"), ("NO_COLOR", "1")],
            true,
            false,
            false,
        );
        assert_eq!((c.depth, c.fancy, c.anim), (Depth::None, true, false));
        let c = caps(&[("TERM", "dumb")], true, false, false);
        assert_eq!((c.depth, c.fancy, c.anim), (Depth::None, false, false));
        let c = caps(&[("TERM", "xterm-256color")], false, false, false);
        assert_eq!((c.depth, c.fancy, c.anim), (Depth::None, false, false));
        let c = caps(&[("TERM", "xterm-256color")], true, false, true);
        assert!(
            c.fancy && !c.anim,
            "--no-anim keeps widgets, drops animation"
        );
        let c = caps(&[("TERM", "xterm-256color")], true, true, false);
        assert!(!c.fancy && c.depth == Depth::None, "--plain");
        let c = caps(
            &[("TERM", "xterm-256color"), ("TESSERA_NO_ANIM", "1")],
            true,
            false,
            false,
        );
        assert!(!c.anim);
    }

    #[test]
    fn colour_fallbacks() {
        assert_eq!(to_256(Rgb(0, 0, 0)), 16);
        assert_eq!(to_256(Rgb(255, 255, 255)), 231);
        assert_eq!(to_256(Rgb(128, 128, 128)), 244);
        // Gold lands on a warm yellow, lapis on a blue, in the 256 cube.
        assert_eq!(to_256(GOLD), 179);
        assert_eq!(to_256(LAPIS), 104);
        assert_eq!(to_16(GOLD), 93);
        assert_eq!(to_16(TERRACOTTA), 91);
        assert_eq!(sgr(S_ACCENT, Depth::None), "");
        assert_eq!(sgr(S_ACCENT, Depth::True), "\x1b[38;2;212;175;95m");
        assert_eq!(sgr(S_ACCENT.bold(), Depth::Ansi256), "\x1b[1;38;5;179m");
        assert_eq!(paint_at("x", S_ACCENT, Depth::None), "x");
    }

    #[test]
    fn text_helpers() {
        assert_eq!(width("\x1b[1mabc\x1b[0m"), 3);
        let w = wrap("the quick brown fox jumps over the lazy dog", 12);
        assert!(w.iter().all(|l| l.len() <= 12), "{w:?}");
        assert_eq!(w.join(" "), "the quick brown fox jumps over the lazy dog");
        assert_eq!(wrap("a\nb", 20), vec!["a", "b"]);
        let f = fit("\x1b[1mhello world\x1b[0m", 6);
        assert_eq!(strip_ansi(&f), "hello…");
    }

    #[test]
    fn line_editor() {
        let mut e = LineEdit::new("héllo");
        e.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        e.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(e.text, "hélo");
        e.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        e.insert("¡");
        assert_eq!(e.text, "¡hélo");
        e.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        e.insert(" wörld");
        e.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(e.text, "¡hélo ");
        e.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((e.text.as_str(), e.cursor), ("", 0));
        e.insert("a\u{7}b");
        assert_eq!(e.text, "ab", "control characters are dropped");
    }

    #[test]
    fn filter_matches_every_word() {
        let c = Choice::new("anthropic/claude-sonnet").hint("200k ctx");
        assert!(matches_query(&c, "claude 200k"));
        assert!(matches_query(&c, ""));
        assert!(!matches_query(&c, "gpt"));
    }

    /// A terminal that draws pages (a TTY) without colours, so the lines
    /// can be read as text.
    fn page_term() -> Term {
        Term::new(caps(
            &[("TERM", "xterm"), ("NO_COLOR", "1")],
            true,
            false,
            true,
        ))
    }

    const STEPS: [(&str, Mark); 4] = [
        ("AI", Mark::Done),
        ("Reachable", Mark::Now),
        ("Background", Mark::Next),
        ("Devices", Mark::Next),
    ];

    #[test]
    fn pages_show_where_the_setup_is() {
        let t = page_term();
        let lines = t.page_lines(&Page {
            title: "Where you'll use it",
            sub: "This computer, your phone at home, or anywhere.",
            steps: &STEPS,
        });
        let text = lines.join("\n");
        assert!(!text.contains('\x1b'), "no colour codes: {text}");
        assert!(lines[1].contains("Tessera setup") && lines[1].ends_with("Step 2 of 4"));
        // The step bar: done and current segments drawn heavy, the rest light.
        let bar = &lines[3];
        assert_eq!(bar.matches('━').count(), bar.matches('─').count());
        assert!(lines[4].contains("✓ AI") && lines[4].contains("● Reachable"));
        assert!(lines[4].contains("○ Background") && lines[4].contains("○ Devices"));
        assert!(text.contains("\n  Where you'll use it\n  This computer, your phone"));
        for l in &lines {
            assert!(width(l) <= t.cols(), "{l}");
        }
        // Line by line (piped): no bar, the step in words.
        let plain = Term::new(caps(&[], false, true, true));
        let lines = plain.page_lines(&Page {
            title: "Your AI",
            sub: "",
            steps: &[("AI", Mark::Now), ("Devices", Mark::Next)],
        });
        assert_eq!(lines[1], "  Step 1 of 2 · Your AI");
    }

    #[test]
    fn choices_each_say_what_they_do_and_the_keys() {
        let t = page_term();
        let choices: Vec<Choice> = ["At home", "At home and away", "Only on this computer"]
            .iter()
            .map(|l| Choice::new(*l).hint(format!("What {l} means.")))
            .collect();
        let vis: Vec<usize> = (0..choices.len()).collect();
        let lines = t.select_lines(
            "Where will you use Tessera?",
            &choices,
            &vis,
            1,
            0,
            12,
            None,
        );
        assert_eq!(lines[0], "  Where will you use Tessera?");
        assert_eq!(lines[1], "");
        // A short list: every choice with its explanation; the highlighted
        // one marked.
        assert!(lines[2].starts_with("     At home") && lines[3].contains("What At home means."));
        assert!(lines[4].starts_with("   ▌ At home and away"));
        assert!(lines[5].starts_with("   ▌ What At home and away means."));
        assert!(
            lines
                .last()
                .unwrap()
                .contains("↑↓ move   ⏎ choose   esc skip")
        );
        // A long list explains only the highlighted choice.
        let many: Vec<Choice> = (0..9)
            .map(|i| Choice::new(format!("Model {i}")).hint("context"))
            .collect();
        let vis: Vec<usize> = (0..9).collect();
        let lines = t.select_lines("Model?", &many, &vis, 3, 0, 12, None);
        assert_eq!(lines.iter().filter(|l| l.contains("context")).count(), 1);
        // With a filter, what was typed shows above.
        let lines = t.select_lines("Model?", &many, &vis, 0, 0, 12, Some("qw"));
        assert!(lines[2].contains("⌕ qw"));
    }

    #[test]
    fn cards_frame_their_rows() {
        let t = page_term();
        let rows = [
            Row::new("AI", "local/qwen").mark("✓", S_OK),
            Row::new("Reachable", "only on this computer")
                .mark("○", S_FAINT)
                .missing(),
            Row::new("Safety", "end-to-end encrypted · asks before risky actions · secrets kept from the AI · commands sandboxed")
                .mark("✓", S_OK),
        ];
        let lines = t.card_lines(&rows);
        assert!(lines[0].starts_with("  ╭") && lines[0].ends_with("╮"));
        assert!(lines.last().unwrap().starts_with("  ╰"));
        let w = width(&lines[0]);
        for l in &lines {
            assert_eq!(width(l), w, "every row as wide as the frame: {l}");
        }
        assert!(lines[1].contains("✓ AI") && lines[1].contains("local/qwen"));
        // The long one wraps under itself, in the frame.
        assert!(lines.len() >= 6, "{lines:#?}");
        // Without a TTY: the rows alone.
        let plain = Term::new(caps(&[], false, true, true));
        let lines = plain.card_lines(&rows);
        assert!(lines.iter().all(|l| !l.contains('│')));
    }

    #[test]
    fn command_lists_wrap_under_their_descriptions() {
        let t = page_term();
        let cmds = [
            ("tessera", "chat in this terminal"),
            (
                "tessera setup",
                "change any of this; also web search and security",
            ),
        ];
        let lines: Vec<String> = t
            .command_lines(&cmds, 50)
            .iter()
            .map(|l| strip_ansi(l))
            .collect();
        assert!(lines.iter().all(|l| width(l) <= 2 + 50), "{lines:?}");
        assert_eq!(lines[0], "  tessera         chat in this terminal");
        assert!(
            lines[1].starts_with("  tessera setup   change any"),
            "{lines:?}"
        );
        // The rest of a long description lines up under its start.
        assert!(
            lines.len() > 2 && lines[2].starts_with(&" ".repeat(18)),
            "{lines:?}"
        );
        assert!(
            lines[2].trim_start().starts_with(char::is_alphabetic),
            "{lines:?}"
        );
    }

    #[test]
    fn commands_to_type_show_as_code() {
        // A `code span` stays on one line when wrapping.
        let w = wrap("then run `tessera setup models` again please", 18);
        assert!(w.contains(&"`tessera setup models`".to_string()), "{w:?}");
        // With colour: in gold, without its backticks; without: as typed.
        let colour = Term::new(caps(
            &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
            true,
            false,
            true,
        ));
        let s = colour.prose("run `tessera pair` there", S_HINT);
        assert_eq!(strip_ansi(&s), "run tessera pair there");
        assert!(s.contains(&sgr(S_ACCENT, Depth::True)), "{s:?}");
        assert_eq!(strip_ansi(&page_term().prose("run `x`", S_HINT)), "run `x`");
        assert_eq!(strip_ansi(&colour.prose("it's `odd", S_HINT)), "it's `odd");
        // Frames and bands stay as wide as the rest.
        let rows = [
            Row::new("AI", "none yet: run `tessera setup models`"),
            Row::new("Devices", "none paired yet"),
        ];
        let lines = colour.card_lines(&rows);
        let w = width(&lines[0]);
        for l in &lines {
            assert_eq!(width(l), w, "{l}");
        }
        let choices = [
            Choice::new("Start"),
            Choice::new("Not now").hint("`tessera setup` comes back here."),
        ];
        let lines = colour.select_lines("Ready?", &choices, &[0, 1], 1, 0, 12, None);
        let banded: Vec<&String> = lines.iter().filter(|l| l.contains("▌")).collect();
        assert_eq!(banded.len(), 2);
        assert_eq!(width(banded[0]), width(banded[1]), "{banded:?}");
    }
}
