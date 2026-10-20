//! Tesserae in the terminal: the apps' palette (warm stone surfaces, smalti
//! gold, calm greys) as terminal styles, and a few other themes.
//!
//! Colours come from the UI's design tokens, dark ("Nocturne") and light
//! ("Travertine"). The other built-in themes (high contrast, Catppuccin,
//! Gruvbox, Tokyo Night) fill the same roles with their own colours.
//! Truecolor terminals get them exactly, 256-colour ones the nearest xterm
//! colours, 16-colour ones hand-picked ANSI colours. With `--plain`,
//! `NO_COLOR` or `TERM=dumb` only attributes (bold, dim, reverse) are used.
//!
//! Auto (the default) keeps the terminal's own background and colours the
//! text for its tone, dark or light. The named themes paint their surfaces
//! (popups, code) as backgrounds when the terminal has at least 256 colours.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// YOLO (nothing asks): a warning sign, never the bolt.
pub const YOLO: &str = "⚠";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    None,
    Ansi16,
    Ansi256,
    True,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Dark,
    Light,
}

/// What the user asked for (`/theme`, `TESSERA_THEME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePref {
    /// The terminal's own background, text coloured for its tone.
    #[default]
    Auto,
    Dark,
    Light,
    /// Black or white (like the terminal), the strongest text.
    Contrast,
    /// Catppuccin Mocha.
    Catppuccin,
    /// Gruvbox dark.
    Gruvbox,
    /// Tokyo Night ("night").
    TokyoNight,
}

impl ThemePref {
    /// Every theme, in the order `/theme` lists them.
    pub const ALL: [Self; 7] = [
        Self::Auto,
        Self::Dark,
        Self::Light,
        Self::Contrast,
        Self::Catppuccin,
        Self::Gruvbox,
        Self::TokyoNight,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        let key: String = s
            .trim()
            .chars()
            .filter(|c| !matches!(c, ' ' | '-' | '_'))
            .collect::<String>()
            .to_ascii_lowercase();
        Some(match key.as_str() {
            "auto" | "system" | "" | "terminal" | "transparent" | "none" => Self::Auto,
            "dark" | "nocturne" => Self::Dark,
            "light" | "travertine" => Self::Light,
            "contrast" | "highcontrast" | "hc" => Self::Contrast,
            "catppuccin" | "catppuccinmocha" | "mocha" => Self::Catppuccin,
            "gruvbox" | "gruvboxdark" => Self::Gruvbox,
            "tokyonight" | "tokyo" => Self::TokyoNight,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Contrast => "contrast",
            Self::Catppuccin => "catppuccin",
            Self::Gruvbox => "gruvbox",
            Self::TokyoNight => "tokyo-night",
        }
    }

    /// The name shown in `/theme`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Contrast => "High contrast",
            Self::Catppuccin => "Catppuccin",
            Self::Gruvbox => "Gruvbox",
            Self::TokyoNight => "Tokyo Night",
        }
    }

    /// One line about the theme.
    pub fn about(self) -> &'static str {
        match self {
            Self::Auto => "Your terminal's colours, dark or light",
            Self::Dark => "Nocturne: warm stone, smalti gold",
            Self::Light => "Travertine: pale stone, deep gold",
            Self::Contrast => "Black or white like the terminal, the strongest text",
            Self::Catppuccin => "Mocha: soft pastels on deep blue-grey",
            Self::Gruvbox => "Retro warmth: cream and amber on charcoal",
            Self::TokyoNight => "Night: cool indigo, neon blues",
        }
    }

    /// Always dark or always light (else the terminal's background decides).
    pub fn fixed_tone(self) -> Option<Tone> {
        match self {
            Self::Dark | Self::Catppuccin | Self::Gruvbox | Self::TokyoNight => Some(Tone::Dark),
            Self::Light => Some(Tone::Light),
            Self::Auto | Self::Contrast => None,
        }
    }

    /// Tessera's own colours (Nocturne or Travertine).
    fn tessera(self) -> bool {
        matches!(self, Self::Auto | Self::Dark | Self::Light)
    }
}

type Rgb = (u8, u8, u8);

/// One palette (a tone).
struct Pal {
    stone: Rgb,
    stone_focus: Rgb,
    raised: Rgb,
    hover: Rgb,
    code_bg: Rgb,
    text: Rgb,
    text2: Rgb,
    muted: Rgb,
    faint: Rgb,
    hairline_strong: Rgb,
    gold: Rgb,
    gold_text: Rgb,
    lapis: Rgb,
    verdigris: Rgb,
    terracotta: Rgb,
    err: Rgb,
}

/// Nocturne: `apps/tessera/ui/src/styles/tokens.css` `:root`. Hairlines are
/// the tokens' translucent text colour composed over stone.
const DARK: Pal = Pal {
    stone: (0x17, 0x16, 0x14),
    stone_focus: (0x1b, 0x1a, 0x17),
    raised: (0x1e, 0x1c, 0x1a),
    hover: (0x26, 0x24, 0x21),
    code_bg: (0x12, 0x11, 0x10),
    text: (0xec, 0xe6, 0xdc),
    text2: (0xb3, 0xac, 0xa2),
    muted: (0x95, 0x8e, 0x84),
    faint: (0x7a, 0x74, 0x6c),
    hairline_strong: (0x3d, 0x3a, 0x36),
    gold: (0xd4, 0xaf, 0x5f),
    gold_text: (0xdc, 0xbd, 0x7c),
    lapis: (0x7d, 0x9b, 0xe0),
    verdigris: (0x5f, 0xb3, 0x9e),
    terracotta: (0xe0, 0x7a, 0x56),
    err: (0xe8, 0x71, 0x4c),
};

/// Travertine: `[data-theme="light"]`.
const LIGHT: Pal = Pal {
    stone: (0xf8, 0xf5, 0xef),
    stone_focus: (0xfc, 0xfa, 0xf6),
    raised: (0xff, 0xff, 0xff),
    hover: (0xef, 0xea, 0xe1),
    code_bg: (0xf0, 0xeb, 0xe3),
    text: (0x21, 0x1e, 0x1a),
    text2: (0x4f, 0x49, 0x42),
    muted: (0x6b, 0x64, 0x5b),
    faint: (0x85, 0x7d, 0x72),
    hairline_strong: (0xc9, 0xc3, 0xba),
    gold: (0x9c, 0x74, 0x24),
    gold_text: (0x8f, 0x6b, 0x21),
    lapis: (0x3d, 0x5c, 0xa8),
    verdigris: (0x2c, 0x75, 0x64),
    terracotta: (0xa9, 0x4b, 0x2a),
    err: (0xa8, 0x43, 0x1f),
};

/// High contrast on black: white text, bright accents (xterm colours, so
/// 256-colour terminals show them exactly).
const CONTRAST_DARK: Pal = Pal {
    stone: (0x00, 0x00, 0x00),
    stone_focus: (0x00, 0x00, 0x00),
    raised: (0x12, 0x12, 0x12),
    hover: (0x30, 0x30, 0x30),
    code_bg: (0x12, 0x12, 0x12),
    text: (0xff, 0xff, 0xff),
    text2: (0xee, 0xee, 0xee),
    muted: (0xd0, 0xd0, 0xd0),
    faint: (0xbc, 0xbc, 0xbc),
    hairline_strong: (0x8a, 0x8a, 0x8a),
    gold: (0xff, 0xd7, 0x5f),
    gold_text: (0xff, 0xd7, 0x5f),
    lapis: (0x87, 0xaf, 0xff),
    verdigris: (0x87, 0xd7, 0x87),
    terracotta: (0xff, 0xaf, 0x5f),
    err: (0xff, 0x87, 0x87),
};

/// High contrast on white: black text, deep accents.
const CONTRAST_LIGHT: Pal = Pal {
    stone: (0xff, 0xff, 0xff),
    stone_focus: (0xff, 0xff, 0xff),
    raised: (0xee, 0xee, 0xee),
    hover: (0xda, 0xda, 0xda),
    code_bg: (0xee, 0xee, 0xee),
    text: (0x00, 0x00, 0x00),
    text2: (0x12, 0x12, 0x12),
    muted: (0x26, 0x26, 0x26),
    faint: (0x44, 0x44, 0x44),
    hairline_strong: (0x6c, 0x6c, 0x6c),
    gold: (0x6b, 0x4a, 0x00),
    gold_text: (0x6b, 0x4a, 0x00),
    lapis: (0x00, 0x00, 0xd7),
    verdigris: (0x00, 0x5f, 0x00),
    terracotta: (0xa3, 0x36, 0x00),
    err: (0xaf, 0x00, 0x00),
};

/// Catppuccin Mocha: base, mantle and crust for surfaces, mauve as the
/// accent (catppuccin.com/palette).
const MOCHA: Pal = Pal {
    stone: (0x1e, 0x1e, 0x2e),
    stone_focus: (0x22, 0x22, 0x32),
    raised: (0x25, 0x25, 0x36),
    hover: (0x31, 0x32, 0x44),
    code_bg: (0x18, 0x18, 0x25),
    text: (0xcd, 0xd6, 0xf4),
    text2: (0xba, 0xc2, 0xde),
    muted: (0xa6, 0xad, 0xc8),
    faint: (0x7f, 0x84, 0x9c),
    hairline_strong: (0x45, 0x47, 0x5a),
    gold: (0xcb, 0xa6, 0xf7),
    gold_text: (0xcb, 0xa6, 0xf7),
    lapis: (0x89, 0xb4, 0xfa),
    verdigris: (0xa6, 0xe3, 0xa1),
    terracotta: (0xfa, 0xb3, 0x87),
    err: (0xf3, 0x8b, 0xa8),
};

/// Gruvbox dark (medium): yellow as the accent, the bright colours for
/// text.
const GRUVBOX: Pal = Pal {
    stone: (0x28, 0x28, 0x28),
    stone_focus: (0x2c, 0x2b, 0x2b),
    raised: (0x32, 0x30, 0x2f),
    hover: (0x3c, 0x38, 0x36),
    code_bg: (0x1d, 0x20, 0x21),
    text: (0xeb, 0xdb, 0xb2),
    text2: (0xd5, 0xc4, 0xa1),
    muted: (0xbd, 0xae, 0x93),
    faint: (0x92, 0x83, 0x74),
    hairline_strong: (0x50, 0x49, 0x45),
    gold: (0xd7, 0x99, 0x21),
    gold_text: (0xfa, 0xbd, 0x2f),
    lapis: (0x83, 0xa5, 0x98),
    verdigris: (0xb8, 0xbb, 0x26),
    terracotta: (0xfe, 0x80, 0x19),
    err: (0xfb, 0x49, 0x34),
};

/// Tokyo Night ("night"): blue as the accent, cyan for lapis.
const TOKYO_NIGHT: Pal = Pal {
    stone: (0x1a, 0x1b, 0x26),
    stone_focus: (0x1e, 0x20, 0x2e),
    raised: (0x1f, 0x23, 0x35),
    hover: (0x29, 0x2e, 0x42),
    code_bg: (0x16, 0x16, 0x1e),
    text: (0xc0, 0xca, 0xf5),
    text2: (0xa9, 0xb1, 0xd6),
    muted: (0x9a, 0xa5, 0xce),
    faint: (0x73, 0x7a, 0xa2),
    hairline_strong: (0x3b, 0x42, 0x61),
    gold: (0x7a, 0xa2, 0xf7),
    gold_text: (0x7a, 0xa2, 0xf7),
    lapis: (0x7d, 0xcf, 0xff),
    verdigris: (0x9e, 0xce, 0x6a),
    terracotta: (0xff, 0x9e, 0x64),
    err: (0xf7, 0x76, 0x8e),
};

/// A palette colour, by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Stone,
    StoneFocus,
    Raised,
    Hover,
    CodeBg,
    Text,
    Text2,
    Muted,
    Faint,
    HairlineStrong,
    Gold,
    GoldText,
    Lapis,
    Verdigris,
    Terracotta,
    Err,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    depth: Depth,
    tone: Tone,
    /// Paint stone surfaces.
    surfaces: bool,
    pref: ThemePref,
    /// The terminal's own background, when known.
    terminal: Option<Tone>,
}

impl Theme {
    /// From the environment. `tone` is what the terminal's background
    /// looks like, when known (see `detect_tone`).
    pub fn detect(plain: bool, pref: ThemePref, terminal_tone: Option<Tone>) -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let depth = detect_depth(plain, &env);
        let pref = env("TESSERA_THEME")
            .and_then(|v| ThemePref::parse(&v))
            .filter(|p| *p != ThemePref::Auto)
            .unwrap_or(pref);
        Self::new(depth, pref, terminal_tone)
    }

    pub fn new(depth: Depth, pref: ThemePref, terminal_tone: Option<Tone>) -> Self {
        Self {
            depth,
            tone: pref.fixed_tone().or(terminal_tone).unwrap_or(Tone::Dark),
            surfaces: depth >= Depth::Ansi256
                && !matches!(pref, ThemePref::Auto | ThemePref::Contrast),
            pref,
            terminal: terminal_tone,
        }
    }

    /// The same terminal with another theme (`/theme`).
    pub fn with_pref(self, pref: ThemePref) -> Self {
        Self::new(self.depth, pref, self.terminal)
    }

    #[cfg(test)]
    pub fn test() -> Self {
        Self::new(Depth::True, ThemePref::Dark, None)
    }

    pub fn has_color(&self) -> bool {
        self.depth != Depth::None
    }

    pub fn depth(&self) -> Depth {
        self.depth
    }

    #[cfg(test)]
    pub fn tone(&self) -> Tone {
        self.tone
    }

    pub fn pref(&self) -> ThemePref {
        self.pref
    }

    /// Stone surfaces are painted (else the terminal background shows).
    pub fn surfaces(&self) -> bool {
        self.surfaces
    }

    /// Distinguishes themes in render-cache keys.
    pub fn cache_key(&self) -> u64 {
        (self.depth as u64)
            | (self.tone as u64) << 4
            | (self.surfaces as u64) << 8
            | (self.pref as u64) << 12
    }

    fn pal(&self) -> &'static Pal {
        match (self.pref, self.tone) {
            (ThemePref::Contrast, Tone::Dark) => &CONTRAST_DARK,
            (ThemePref::Contrast, Tone::Light) => &CONTRAST_LIGHT,
            (ThemePref::Catppuccin, _) => &MOCHA,
            (ThemePref::Gruvbox, _) => &GRUVBOX,
            (ThemePref::TokyoNight, _) => &TOKYO_NIGHT,
            (_, Tone::Dark) => &DARK,
            (_, Tone::Light) => &LIGHT,
        }
    }

    pub fn rgb(&self, t: Tok) -> Rgb {
        let p = self.pal();
        match t {
            Tok::Stone => p.stone,
            Tok::StoneFocus => p.stone_focus,
            Tok::Raised => p.raised,
            Tok::Hover => p.hover,
            Tok::CodeBg => p.code_bg,
            Tok::Text => p.text,
            Tok::Text2 => p.text2,
            Tok::Muted => p.muted,
            Tok::Faint => p.faint,
            Tok::HairlineStrong => p.hairline_strong,
            Tok::Gold => p.gold,
            Tok::GoldText => p.gold_text,
            Tok::Lapis => p.lapis,
            Tok::Verdigris => p.verdigris,
            Tok::Terracotta => p.terracotta,
            Tok::Err => p.err,
        }
    }

    /// The 16-colour stand-in for a token. The terminal's own 16 colours
    /// are what show, so themes differ only in their accent's hue (and
    /// high contrast in stronger greys).
    fn ansi(&self, t: Tok) -> Color {
        let dark = self.tone == Tone::Dark;
        if self.pref == ThemePref::Contrast {
            let c = match (t, dark) {
                (Tok::Text2, _) | (Tok::Muted, false) => Some(Color::Reset),
                (Tok::Muted | Tok::Faint | Tok::HairlineStrong, true) => Some(Color::Gray),
                (Tok::Gold | Tok::GoldText, true) => Some(Color::LightYellow),
                (Tok::Lapis, true) => Some(Color::LightBlue),
                (Tok::Verdigris, true) => Some(Color::LightGreen),
                (Tok::Terracotta, true) => Some(Color::Yellow),
                (Tok::Err, true) => Some(Color::LightRed),
                // Yellow on white is unreadable in most terminals.
                (Tok::Gold | Tok::GoldText | Tok::Lapis, false) => Some(Color::Blue),
                (Tok::Terracotta, false) => Some(Color::Magenta),
                _ => None,
            };
            if let Some(c) = c {
                return c;
            }
        } else if !self.pref.tessera() && matches!(t, Tok::Gold | Tok::GoldText) {
            // The accent's hue; success, warnings and errors keep the
            // terminal's green and reds.
            let (r, g, b) = self.rgb(t);
            return ansi16(r, g, b);
        }
        match t {
            Tok::Text => Color::Reset,
            Tok::Text2 => {
                if dark {
                    Color::Gray
                } else {
                    Color::Reset
                }
            }
            Tok::Muted | Tok::Faint => Color::DarkGray,
            Tok::HairlineStrong => Color::DarkGray,
            Tok::Gold | Tok::GoldText => Color::Yellow,
            Tok::Lapis => Color::Blue,
            Tok::Verdigris => Color::Green,
            Tok::Terracotta => Color::LightRed,
            Tok::Err => Color::Red,
            _ => Color::Reset,
        }
    }

    /// `rgb` as a terminal colour at this depth.
    pub fn color_of(&self, rgb: Rgb) -> Option<Color> {
        match self.depth {
            Depth::None => None,
            Depth::True => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
            Depth::Ansi256 => Some(Color::Indexed(to_256(rgb))),
            Depth::Ansi16 => Some(ansi16(rgb.0, rgb.1, rgb.2)),
        }
    }

    pub fn c(&self, t: Tok) -> Option<Color> {
        match self.depth {
            Depth::None => None,
            Depth::Ansi16 => Some(self.ansi(t)),
            Depth::Ansi256 if matches!(t, Tok::Raised | Tok::Hover | Tok::CodeBg) => {
                Some(Color::Indexed(self.apart_from_stone(t)))
            }
            _ => self.color_of(self.rgb(t)),
        }
    }

    /// A surface's xterm colour. Surfaces sit a few shades from stone,
    /// which the grey ramp often can't resolve: step one grey away (darker
    /// or lighter, as the surface is) so popups and code slabs
    /// still show.
    fn apart_from_stone(&self, t: Tok) -> u8 {
        let (stone_rgb, own_rgb) = (self.rgb(Tok::Stone), self.rgb(t));
        let (stone, own) = (to_256(stone_rgb), to_256(own_rgb));
        if own != stone || own_rgb == stone_rgb {
            return own;
        }
        let darker = luma(own_rgb) < luma(stone_rgb)
            || (luma(own_rgb) == luma(stone_rgb) && self.tone == Tone::Dark);
        match (stone, darker) {
            (232, true) => 16,
            (255, false) => 231,
            (233..=255, true) => stone - 1,
            (232..=254, false) => stone + 1,
            _ => own,
        }
    }

    /// `a` mixed toward `b` by `k` (0 = a), as a colour at this depth.
    pub fn mix(&self, a: Tok, b: Tok, k: f32) -> Option<Color> {
        match self.depth {
            Depth::None => None,
            Depth::Ansi16 => Some(self.ansi(if k < 0.5 { a } else { b })),
            _ => self.color_of(mix(self.rgb(a), self.rgb(b), k)),
        }
    }

    pub fn fg(&self, t: Tok) -> Style {
        match self.c(t) {
            Some(Color::Reset) | None => Style::new(),
            Some(c) => Style::new().fg(c),
        }
    }

    /// A background, only where surfaces are painted.
    pub fn bg(&self, t: Tok) -> Style {
        if !self.surfaces {
            return Style::new();
        }
        match self.c(t) {
            Some(c) => Style::new().bg(c),
            None => Style::new(),
        }
    }

    // ---- surfaces ---------------------------------------------------------------

    /// A tile. The focused one is a touch lighter, as in the apps.
    pub fn stone(&self, focused: bool) -> Style {
        self.bg(if focused { Tok::StoneFocus } else { Tok::Stone })
            .patch(self.text())
    }

    /// Popups, chips, the user's messages.
    pub fn raised(&self) -> Style {
        self.bg(Tok::Raised).patch(self.text())
    }

    // ---- text -------------------------------------------------------------------

    /// Smalti gold.
    pub fn accent(&self) -> Style {
        self.fg(Tok::Gold)
    }

    /// Gold for text (a touch brighter on stone, deeper on travertine).
    pub fn accent_text(&self) -> Style {
        self.fg(Tok::GoldText)
    }

    /// Body text.
    pub fn text(&self) -> Style {
        if self.surfaces {
            self.fg(Tok::Text)
        } else {
            Style::new()
        }
    }

    pub fn bold(&self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }

    /// A title drawn over a border: bold body text that brings its own
    /// colour (`bold` keeps whatever colour the cell had, the border's).
    pub fn title(&self) -> Style {
        if self.surfaces || !self.has_color() {
            self.bold()
        } else {
            self.bold().fg(Color::Reset)
        }
    }

    /// Readable secondary text (titles of unfocused tiles, descriptions).
    pub fn muted(&self) -> Style {
        if self.has_color() {
            self.fg(Tok::Text2)
        } else {
            Style::new()
        }
    }

    /// De-emphasised text (metadata, placeholders).
    pub fn dim(&self) -> Style {
        if self.has_color() {
            self.fg(Tok::Muted)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        }
    }

    /// Hints, gutters and separators.
    pub fn faint(&self) -> Style {
        if self.has_color() {
            self.fg(Tok::Faint)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        }
    }

    /// Borders and rules: thin and quiet.
    pub fn hairline(&self) -> Style {
        if self.has_color() {
            self.fg(Tok::HairlineStrong)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        }
    }

    /// Inline `code`: a small stone chip where surfaces are painted.
    pub fn code(&self) -> Style {
        if self.surfaces {
            self.bg(Tok::Hover).patch(self.text())
        } else if self.has_color() {
            self.fg(Tok::GoldText)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        }
    }

    /// Text inside fenced code blocks.
    pub fn code_block(&self) -> Style {
        self.bg(Tok::CodeBg).patch(self.text())
    }

    /// Headings: no display face in a terminal, so weight and colour.
    pub fn heading(&self, level: u8) -> Style {
        match level {
            1 => self.accent_text().add_modifier(Modifier::BOLD),
            2 => self.bold(),
            3 => self.muted().add_modifier(Modifier::BOLD),
            _ => self.muted().add_modifier(Modifier::ITALIC),
        }
    }

    pub fn link(&self) -> Style {
        self.accent_text().add_modifier(Modifier::UNDERLINED)
    }

    pub fn success(&self) -> Style {
        self.fg(Tok::Verdigris)
    }

    pub fn error(&self) -> Style {
        self.fg(Tok::Err)
    }

    /// Terracotta: warnings, approvals, YOLO.
    pub fn warning(&self) -> Style {
        self.fg(Tok::Terracotta)
    }

    pub fn diff_add(&self) -> Style {
        self.success()
    }

    pub fn diff_del(&self) -> Style {
        self.error()
    }

    pub fn thinking(&self) -> Style {
        self.dim().add_modifier(Modifier::ITALIC)
    }

    pub fn user(&self) -> Style {
        self.text()
    }

    /// Highlighted row in lists and popups: a soft band where the
    /// terminal has the colours for it and its background is known (a
    /// guessed tone could put a dark band under dark text), reversed
    /// elsewhere.
    pub fn selected(&self) -> Style {
        let known = self.surfaces || self.terminal.is_some() || self.pref.fixed_tone().is_some();
        match (self.depth, known) {
            (Depth::Ansi256 | Depth::True, true) => match self.c(Tok::Hover) {
                Some(c) => Style::new().bg(c),
                None => Style::new().add_modifier(Modifier::REVERSED),
            },
            _ => Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    /// Tile borders: gold around the tile you're in, hairlines elsewhere.
    pub fn border(&self, focused: bool) -> Style {
        if !focused {
            return self.hairline();
        }
        match self.mix(Tok::Gold, Tok::Stone, 0.25) {
            Some(c) => Style::new().fg(c),
            None => Style::new().add_modifier(Modifier::BOLD),
        }
    }

    /// YOLO (auto-approve) marker.
    pub fn yolo(&self) -> Style {
        self.warning().add_modifier(Modifier::BOLD)
    }

    /// A key hint (`esc`, `⏎`): the key a shade brighter than its label.
    pub fn key(&self) -> Style {
        if self.has_color() {
            self.fg(Tok::Text2)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        }
    }

    /// Unlit tesserae of indicators and bars.
    pub fn tile_off(&self) -> Style {
        match self.depth {
            Depth::None => Style::new().add_modifier(Modifier::DIM),
            Depth::Ansi16 => Style::new().fg(Color::DarkGray),
            _ => self.fg(Tok::HairlineStrong),
        }
    }

    /// Gold faded toward stone (`k` = 0 full gold, 1 stone).
    pub fn gold_faded(&self, k: f32) -> Style {
        if self.depth == Depth::Ansi16 {
            return Style::new().fg(if k < 0.5 {
                Color::Yellow
            } else {
                Color::DarkGray
            });
        }
        match self.mix(Tok::Gold, Tok::Stone, k) {
            Some(c) => Style::new().fg(c),
            None => Style::new(),
        }
    }

    /// A bot's colour: its own `#rrggbb`, or one of the Tessera smalti
    /// colours picked from its id when it has none. High contrast deepens
    /// (or lightens) the colour until its name reads at 4.5:1.
    pub fn bot(&self, color: &str, id: &str) -> Style {
        if let Some(rgb) = parse_hex(color) {
            if self.pref != ThemePref::Contrast || self.depth < Depth::Ansi256 {
                return self.hex(color);
            }
            let (bg, text) = (self.rgb(Tok::Stone), self.rgb(Tok::Text));
            let readable = (0..=10)
                .map(|i| mix(rgb, text, i as f32 / 10.0))
                .find(|c| contrast(*c, bg) >= 4.5)
                .unwrap_or(text);
            return self
                .color_of(readable)
                .map_or(Style::new(), |c| Style::new().fg(c));
        }
        let h = id
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        let t = [Tok::Gold, Tok::Lapis, Tok::Verdigris, Tok::Terracotta][h as usize % 4];
        self.fg(t)
    }

    /// A `#rrggbb` colour (bots): exact in truecolor, the nearest xterm
    /// colour with 256 colours, a hue-matched ANSI colour otherwise.
    pub fn hex(&self, s: &str) -> Style {
        let Some(rgb) = parse_hex(s) else {
            return self.accent();
        };
        match self.color_of(rgb) {
            Some(c) => Style::new().fg(c),
            None => Style::new(),
        }
    }

    /// A small sample of the theme for `/theme`: text on its stone, then
    /// its accent, blue, green, warning and error colours (never shown
    /// reversed, even on a highlighted row).
    pub fn swatch(&self) -> Vec<Span<'static>> {
        let stone = self.bg(Tok::Stone).remove_modifier(Modifier::REVERSED);
        let mut out = vec![Span::styled(" Aa ", stone.patch(self.bold()))];
        for t in [
            Tok::Gold,
            Tok::Lapis,
            Tok::Verdigris,
            Tok::Terracotta,
            Tok::Err,
        ] {
            out.push(Span::styled("■", stone.patch(self.fg(t))));
        }
        out.push(Span::styled(" ", stone));
        out
    }
}

fn detect_depth(plain: bool, env: &dyn Fn(&str) -> Option<String>) -> Depth {
    let no_color = env("NO_COLOR").is_some_and(|v| !v.is_empty());
    let term = env("TERM").unwrap_or_default();
    if plain || no_color || term == "dumb" {
        return Depth::None;
    }
    let colorterm = env("COLORTERM").unwrap_or_default();
    let program = env("TERM_PROGRAM").unwrap_or_default();
    if colorterm.contains("truecolor")
        || colorterm.contains("24bit")
        || term.contains("direct")
        || [
            "kitty",
            "alacritty",
            "wezterm",
            "ghostty",
            "foot",
            "contour",
        ]
        .iter()
        .any(|t| term.contains(t))
        || ["iTerm.app", "WezTerm", "ghostty", "vscode", "Hyper"].contains(&program.as_str())
        || env("KITTY_WINDOW_ID").is_some()
    {
        Depth::True
    } else if term.contains("256") {
        Depth::Ansi256
    } else {
        Depth::Ansi16
    }
}

/// The terminal's background from `COLORFGBG` ("15;0": light text on a
/// dark background), when the terminal sets it.
pub fn tone_from_colorfgbg(v: &str) -> Option<Tone> {
    let bg: u8 = v.rsplit(';').next()?.trim().parse().ok()?;
    Some(if bg == 7 || bg >= 9 {
        Tone::Light
    } else {
        Tone::Dark
    })
}

/// Light or dark from an `rgb:RRRR/GGGG/BBBB` colour (an OSC 11 reply).
#[cfg_attr(not(unix), allow(dead_code))]
pub fn tone_from_osc(reply: &[u8]) -> Option<Tone> {
    let s = String::from_utf8_lossy(reply);
    let i = s.find("rgb:")?;
    let body: String = s[i + 4..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '/')
        .collect();
    let parts: Vec<&str> = body.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let chan = |p: &str| -> Option<f32> {
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = (1u32 << (4 * p.len().clamp(1, 4))) - 1;
        Some(v as f32 / max as f32)
    };
    let (r, g, b) = (chan(parts[0])?, chan(parts[1])?, chan(parts[2])?);
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(if luma > 0.5 { Tone::Light } else { Tone::Dark })
}

/// WCAG contrast ratio of two colours (1 to 21).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let lum = |c: Rgb| {
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.0) + 0.7152 * f(c.1) + 0.0722 * f(c.2)
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Relative brightness (Rec. 709 weights), for ordering colours.
fn luma(c: Rgb) -> u32 {
    2126 * c.0 as u32 + 7152 * c.1 as u32 + 722 * c.2 as u32
}

pub fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

pub(super) fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim().trim_start_matches('#');
    let h = if h.len() == 3 {
        h.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        h.to_string()
    };
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

/// Nearest xterm-256 colour: the 6×6×6 cube or the grey ramp.
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
    let cube = (LEVELS[r], LEVELS[g], LEVELS[b]);
    let avg = (c.0 as i32 + c.1 as i32 + c.2 as i32) / 3;
    let gi = (((avg - 8).max(0) + 4) / 10).min(23) as u8;
    let gv = 8 + 10 * gi;
    let dist = |a: Rgb| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, c.0) + d(a.1, c.1) + d(a.2, c.2)
    };
    if dist((gv, gv, gv)) <= dist(cube) {
        232 + gi
    } else {
        16 + 36 * r as u8 + 6 * g as u8 + b as u8
    }
}

/// Hue-matched basic colour (pastels stay distinguishable, unlike RGB distance).
fn ansi16(r: u8, g: u8, b: u8) -> Color {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let light = (max + min) / 2.0;
    let d = max - min;
    if d < 0.08 {
        return if light > 0.7 {
            Color::White
        } else if light > 0.35 {
            Color::Gray
        } else {
            Color::DarkGray
        };
    }
    let mut hue = if max == rf {
        60.0 * ((gf - bf) / d)
    } else if max == gf {
        60.0 * ((bf - rf) / d) + 120.0
    } else {
        60.0 * ((rf - gf) / d) + 240.0
    };
    if hue < 0.0 {
        hue += 360.0;
    }
    let bright = light > 0.55;
    let pick = |dark: Color, lightc: Color| if bright { lightc } else { dark };
    match hue {
        h if !(20.0..345.0).contains(&h) => pick(Color::Red, Color::LightRed),
        h if h < 75.0 => pick(Color::Yellow, Color::LightYellow),
        h if h < 160.0 => pick(Color::Green, Color::LightGreen),
        h if h < 195.0 => pick(Color::Cyan, Color::LightCyan),
        h if h < 250.0 => pick(Color::Blue, Color::LightBlue),
        _ => pick(Color::Magenta, Color::LightMagenta),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex("#5eead4"), Some((0x5e, 0xea, 0xd4)));
        assert_eq!(parse_hex("fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(ansi16(0x5e, 0xea, 0xd4), Color::LightCyan);
        assert_eq!(ansi16(0xfc, 0xa5, 0xa5), Color::LightRed);
        assert_eq!(ansi16(0xbe, 0xf2, 0x64), Color::LightGreen);
        assert_eq!(ansi16(0xc4, 0xb5, 0xfd), Color::LightMagenta);
        let t = Theme::new(Depth::Ansi256, ThemePref::Dark, None);
        assert_eq!(t.hex("#ff0000").fg, Some(Color::Indexed(196)));
        assert_eq!(Theme::test().hex("#010203").fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn palette_fallbacks() {
        // Smalti gold lands on its xterm neighbour; stone on the grey ramp.
        assert_eq!(to_256((0xd4, 0xaf, 0x5f)), 179);
        assert_eq!(to_256((0x17, 0x16, 0x14)), 233);
        assert_eq!(to_256((0x1e, 0x1c, 0x1a)), 234);
        let t16 = Theme::new(Depth::Ansi16, ThemePref::Auto, None);
        assert_eq!(t16.accent().fg, Some(Color::Yellow));
        assert!(!t16.surfaces(), "16 colours never paint backgrounds");
        // Auto keeps the terminal's own background.
        let auto = Theme::new(Depth::True, ThemePref::Auto, Some(Tone::Dark));
        assert!(!auto.surfaces() && auto.stone(true).bg.is_none());
        assert!(auto.selected().bg.is_some(), "a soft band, not reversed");
        // A background it couldn't tell: reversed, readable on any.
        let unknown = Theme::new(Depth::True, ThemePref::Auto, None);
        assert!(unknown.selected().add_modifier.contains(Modifier::REVERSED));
        let t256 = Theme::new(Depth::Ansi256, ThemePref::Dark, None);
        assert!(t256.surfaces());
        // Code slabs stay distinct from stone on the grey ramp.
        assert_ne!(t256.c(Tok::CodeBg), t256.c(Tok::Stone));
        let l256 = Theme::new(Depth::Ansi256, ThemePref::Light, None);
        assert_ne!(l256.c(Tok::CodeBg), l256.c(Tok::Stone));
        // NO_COLOR: attributes only.
        let none = Theme::new(Depth::None, ThemePref::Auto, None);
        assert!(!none.has_color());
        assert_eq!(none.accent(), Style::new());
        assert!(none.dim().add_modifier.contains(Modifier::DIM));
        assert!(none.stone(true).bg.is_none());
        // Light follows the terminal (or the choice).
        let light = Theme::new(Depth::True, ThemePref::Auto, Some(Tone::Light));
        assert_eq!(light.accent().fg, Some(Color::Rgb(0x9c, 0x74, 0x24)));
        assert_eq!(
            Theme::new(Depth::True, ThemePref::Dark, Some(Tone::Light)).tone(),
            Tone::Dark
        );
    }

    #[test]
    fn theme_names() {
        for p in ThemePref::ALL {
            assert_eq!(ThemePref::parse(p.name()), Some(p), "{p:?} by name");
            assert_eq!(ThemePref::parse(p.label()), Some(p), "{p:?} by label");
        }
        assert_eq!(
            ThemePref::parse(" Tokyo_Night "),
            Some(ThemePref::TokyoNight)
        );
        assert_eq!(
            ThemePref::parse("catppuccin-mocha"),
            Some(ThemePref::Catppuccin)
        );
        assert_eq!(ThemePref::parse("HC"), Some(ThemePref::Contrast));
        assert_eq!(ThemePref::parse("nocturne"), Some(ThemePref::Dark));
        assert_eq!(ThemePref::parse("solarized"), None);
        // A remembered name from an older or newer version: the default.
        assert_eq!(
            ThemePref::parse("dracula").unwrap_or_default(),
            ThemePref::Auto
        );
        // Palettes keep their tone; auto and high contrast follow the terminal.
        let on_light = |p| Theme::new(Depth::True, p, Some(Tone::Light)).tone();
        assert_eq!(on_light(ThemePref::Gruvbox), Tone::Dark);
        assert_eq!(on_light(ThemePref::Contrast), Tone::Light);
        assert_eq!(on_light(ThemePref::Auto), Tone::Light);
        let t = Theme::new(Depth::True, ThemePref::Dark, Some(Tone::Light));
        assert_eq!(t.with_pref(ThemePref::Contrast).tone(), Tone::Light);
        assert_eq!(
            t.with_pref(ThemePref::Contrast).fg(Tok::Text).fg,
            Some(Color::Rgb(0, 0, 0))
        );
        assert_ne!(
            t.cache_key(),
            t.with_pref(ThemePref::Catppuccin).cache_key()
        );
    }

    #[test]
    fn every_theme_is_readable() {
        for pref in ThemePref::ALL {
            for tone in [Tone::Dark, Tone::Light] {
                let t = Theme::new(Depth::True, pref, Some(tone));
                let strong = pref == ThemePref::Contrast;
                for bg in [Tok::Stone, Tok::Raised, Tok::Hover, Tok::CodeBg] {
                    let on = |fg: Tok| contrast(t.rgb(fg), t.rgb(bg));
                    let what = format!("{pref:?} {tone:?} on {bg:?}");
                    assert!(on(Tok::Text) >= if strong { 13.0 } else { 7.0 }, "{what}");
                    assert!(on(Tok::Text2) >= if strong { 11.0 } else { 6.0 }, "{what}");
                    assert!(on(Tok::Muted) >= if strong { 8.0 } else { 4.5 }, "{what}");
                    assert!(on(Tok::Faint) >= if strong { 6.0 } else { 3.0 }, "{what}");
                    for accent in [Tok::GoldText, Tok::Verdigris, Tok::Terracotta, Tok::Err] {
                        assert!(on(accent) >= 3.0, "{accent:?}: {what}");
                    }
                }
                // Selection and borders show.
                assert!(
                    contrast(t.rgb(Tok::Hover), t.rgb(Tok::Stone)) >= 1.08,
                    "{pref:?}"
                );
                assert!(
                    contrast(t.rgb(Tok::HairlineStrong), t.rgb(Tok::Stone)) >= 1.4,
                    "{pref:?}"
                );
            }
        }
    }

    #[test]
    fn every_theme_degrades() {
        for pref in ThemePref::ALL {
            for tone in [Tone::Dark, Tone::Light] {
                let what = format!("{pref:?} {tone:?}");
                // 256 colours: surfaces that differ from stone still do.
                let t = Theme::new(Depth::Ansi256, pref, Some(tone));
                assert_eq!(
                    t.surfaces(),
                    !matches!(pref, ThemePref::Auto | ThemePref::Contrast),
                    "{what}"
                );
                for s in [Tok::Raised, Tok::Hover, Tok::CodeBg] {
                    if t.rgb(s) != t.rgb(Tok::Stone) {
                        assert_ne!(t.c(s), t.c(Tok::Stone), "{s:?}: {what}");
                    }
                }
                for fg in [Tok::Text, Tok::Muted, Tok::Gold, Tok::Err] {
                    assert_ne!(t.c(fg), t.c(Tok::Stone), "{fg:?}: {what}");
                }
                // 16 colours: the terminal's own background and colours.
                let t = Theme::new(Depth::Ansi16, pref, Some(tone));
                assert!(!t.surfaces() && t.stone(true).bg.is_none(), "{what}");
                assert!(t.accent().fg.is_some() && t.error().fg.is_some(), "{what}");
                assert!(
                    t.c(Tok::Text) == Some(Color::Reset),
                    "text keeps the terminal's colour: {what}"
                );
                // NO_COLOR: attributes only.
                let t = Theme::new(Depth::None, pref, Some(tone));
                for st in [t.accent(), t.text(), t.stone(true), t.selected(), t.error()] {
                    assert!(st.fg.is_none() && st.bg.is_none(), "{what}");
                }
                assert!(t.dim().add_modifier.contains(Modifier::DIM), "{what}");
                assert!(t.swatch().iter().all(|s| s.style.fg.is_none()), "{what}");
            }
        }
        // Palettes keep their accent's hue with 16 colours; Tessera keeps
        // its hand-picked ones.
        let accent16 = |p| Theme::new(Depth::Ansi16, p, None).accent().fg;
        assert_eq!(accent16(ThemePref::Auto), Some(Color::Yellow));
        assert_eq!(accent16(ThemePref::Catppuccin), Some(Color::LightMagenta));
        assert_eq!(accent16(ThemePref::TokyoNight), Some(Color::LightBlue));
        assert_eq!(accent16(ThemePref::Contrast), Some(Color::LightYellow));
        let hc_light = Theme::new(Depth::Ansi16, ThemePref::Contrast, Some(Tone::Light));
        assert_eq!(hc_light.accent().fg, Some(Color::Blue));
        assert_eq!(hc_light.muted().fg, None, "full-strength text");
        // Exact xterm colours for high contrast on 256-colour terminals.
        let hc = Theme::new(Depth::Ansi256, ThemePref::Contrast, Some(Tone::Dark));
        assert_eq!(hc.accent().fg, Some(Color::Indexed(221)));
        assert_eq!(hc.selected().bg, Some(Color::Indexed(236)));
        let tn = Theme::new(Depth::Ansi256, ThemePref::TokyoNight, None);
        assert_eq!(tn.c(Tok::Stone), Some(Color::Indexed(234)));
    }

    #[test]
    fn swatches() {
        let t = Theme::new(Depth::True, ThemePref::Gruvbox, None);
        let sw = t.swatch();
        assert!(
            sw.iter()
                .all(|s| s.style.bg == Some(Color::Rgb(0x28, 0x28, 0x28)))
        );
        assert_eq!(sw[1].style.fg, Some(Color::Rgb(0xd7, 0x99, 0x21)));
        let term = t.with_pref(ThemePref::Auto).swatch();
        assert!(term.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn bot_names_read_in_high_contrast() {
        // A pale gold bot on white: deepened until it reads; elsewhere the
        // bot's own colour.
        let hc = Theme::new(Depth::True, ThemePref::Contrast, Some(Tone::Light));
        let Some(Color::Rgb(r, g, b)) = hc.bot("#d4af5f", "pip").fg else {
            panic!("a colour")
        };
        assert!(contrast((r, g, b), (255, 255, 255)) >= 4.5);
        let light = Theme::new(Depth::True, ThemePref::Light, None);
        assert_eq!(
            light.bot("#d4af5f", "pip").fg,
            Some(Color::Rgb(0xd4, 0xaf, 0x5f))
        );
        // A dark navy bot on black: lightened.
        let hc_dark = Theme::new(Depth::True, ThemePref::Contrast, Some(Tone::Dark));
        let Some(Color::Rgb(r, g, b)) = hc_dark.bot("#1a1a6e", "nav").fg else {
            panic!("a colour")
        };
        assert!(contrast((r, g, b), (0, 0, 0)) >= 4.5);
    }

    #[test]
    fn terminal_background_detection() {
        assert_eq!(
            tone_from_osc(b"\x1b]11;rgb:1717/1616/1414\x1b\\"),
            Some(Tone::Dark)
        );
        assert_eq!(
            tone_from_osc(b"\x1b]11;rgb:f8f8/f5f5/efef\x07"),
            Some(Tone::Light)
        );
        assert_eq!(
            tone_from_osc(b"\x1b]11;rgb:ff/ff/ff\x07"),
            Some(Tone::Light)
        );
        assert_eq!(tone_from_osc(b"garbage"), None);
        assert_eq!(tone_from_colorfgbg("15;0"), Some(Tone::Dark));
        assert_eq!(tone_from_colorfgbg("0;default;15"), Some(Tone::Light));
        assert_eq!(tone_from_colorfgbg("x"), None);
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(detect_depth(false, &env(&[("TERM", "dumb")])), Depth::None);
        assert_eq!(
            detect_depth(
                false,
                &env(&[("TERM", "xterm-256color"), ("NO_COLOR", "1")])
            ),
            Depth::None
        );
        assert_eq!(
            detect_depth(false, &env(&[("TERM", "tmux-256color")])),
            Depth::Ansi256
        );
        assert_eq!(
            detect_depth(
                false,
                &env(&[("TERM", "xterm"), ("COLORTERM", "truecolor")])
            ),
            Depth::True
        );
        assert_eq!(
            detect_depth(false, &env(&[("TERM", "xterm")])),
            Depth::Ansi16
        );
        assert_eq!(
            detect_depth(true, &env(&[("TERM", "xterm-kitty")])),
            Depth::None
        );
    }
}
