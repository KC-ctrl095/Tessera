//! Small motifs shared by every part of the interface: the working
//! indicator made of tesserae, status squares, step strips, the context
//! bar and the "T" logo.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::text::span;
use super::theme::Theme;

/// How often the working indicator steps (the apps use 200 ms).
pub const STEP_MS: u64 = 200;

/// How much the interface moves. Slow terminals (a slow link, a busy
/// machine) step down so typing stays responsive; `--plain`, `NO_COLOR` and
/// `TESSERA_NO_ANIM` turn it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Full,
    /// Indicators step at half the rate.
    Reduced,
    Off,
}

/// The working indicator: `n` small tiles with a light walking across,
/// like the apps' tessellate (each tile lights in turn, holds, and goes
/// out in the same order).
pub fn spinner(t: &Theme, step: usize, n: usize) -> Vec<Span<'static>> {
    let n = n.max(1);
    let s = step % (2 * n);
    (0..n)
        .map(|i| {
            let lit = s >= i && s < i + n;
            if !t.has_color() {
                span(if lit { "▪" } else { "·" }, Style::new())
            } else if lit {
                span("▪", t.accent())
            } else {
                span("▪", t.gold_faded(0.72))
            }
        })
        .collect()
}

/// State of a step (tool call) or a subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sq {
    Done,
    Failed,
    Running,
    Waiting,
    Approval,
    Unknown,
    /// Thinking inside a step run (not a tool).
    Thought,
}

/// A status tessera: ■ done / failed, ▣ running, □ waiting.
pub fn square(t: &Theme, s: Sq) -> Span<'static> {
    let (g, st) = match s {
        Sq::Done => ("■", t.success()),
        Sq::Failed => ("■", t.error()),
        Sq::Running => ("▣", t.accent()),
        Sq::Approval => ("▣", t.warning()),
        Sq::Waiting => ("□", t.faint()),
        Sq::Unknown => ("□", t.dim()),
        Sq::Thought => ("▪", t.tile_off()),
    };
    if !t.has_color() {
        let g = match s {
            Sq::Failed => "×",
            _ => g,
        };
        return span(g, Style::new());
    }
    span(g, st)
}

/// A strip of small tiles, one per step (at most `max`).
pub fn strip(t: &Theme, states: &[Sq], max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for s in states.iter().take(max) {
        let st = match s {
            Sq::Done => t.success(),
            Sq::Failed => t.error(),
            Sq::Running => t.accent(),
            Sq::Approval => t.warning(),
            Sq::Waiting | Sq::Unknown | Sq::Thought => t.tile_off(),
        };
        let g = if !t.has_color() {
            match s {
                Sq::Done => "■",
                Sq::Failed => "×",
                Sq::Running | Sq::Approval => "▣",
                _ => "□",
            }
        } else {
            "▪"
        };
        match out.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push_str(g),
            _ => out.push(span(g, st)),
        }
    }
    if states.len() > max {
        out.push(span("…", t.faint()));
    }
    out
}

/// A slim bar of `n` tesserae filled to `ratio`.
pub fn bar(t: &Theme, ratio: f64, n: usize) -> Vec<Span<'static>> {
    let ratio = ratio.clamp(0.0, 1.0);
    let mut filled = (ratio * n as f64).round() as usize;
    if ratio > 0.0 && filled == 0 {
        filled = 1;
    }
    let filled = filled.min(n);
    let on = if ratio < 0.6 {
        t.accent()
    } else if ratio < 0.85 {
        t.warning()
    } else {
        t.error()
    };
    let (g_on, g_off) = if t.has_color() {
        ("▪", "▪")
    } else {
        ("■", "·")
    };
    vec![
        span(g_on.repeat(filled), on),
        span(g_off.repeat(n - filled), t.tile_off()),
    ]
}

/// The Tessera mark: a "T" laid in tiles.
pub fn logo(t: &Theme) -> Span<'static> {
    span("▀█▀", t.accent().add_modifier(Modifier::BOLD))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Depth, ThemePref};

    #[test]
    fn spinner_walks() {
        let t = Theme::new(Depth::None, ThemePref::Auto, None);
        let s = |k| {
            spinner(&t, k, 3)
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(s(0), "▪··");
        assert_eq!(s(2), "▪▪▪");
        assert_eq!(s(4), "··▪");
        assert_eq!(s(5), "···");
        assert_eq!(s(6), s(0));
    }

    #[test]
    fn bars_and_strips() {
        let t = Theme::test();
        let b: String = bar(&t, 0.01, 8)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(b.chars().count(), 8);
        assert_eq!(bar(&t, 0.01, 8)[0].content, "▪", "anything used shows");
        let s = strip(&t, &[Sq::Done, Sq::Done, Sq::Failed, Sq::Running], 3);
        let text: String = s.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "▪▪▪…");
    }
}
