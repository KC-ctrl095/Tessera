//! Width-aware text helpers: sanitising untrusted output, cluster iteration
//! that matches how ratatui measures cells, truncation and word wrapping of
//! styled spans.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ZWJ: char = '\u{200d}';

fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Iterates approximate grapheme clusters (base char + combining marks,
/// variation selectors, ZWJ sequences and flag pairs) with their display
/// width. Close enough to Unicode segmentation for terminal layout.
pub struct Clusters<'a> {
    s: &'a str,
    pos: usize,
}

pub fn clusters(s: &str) -> Clusters<'_> {
    Clusters { s, pos: 0 }
}

impl<'a> Iterator for Clusters<'a> {
    type Item = (&'a str, usize);

    fn next(&mut self) -> Option<Self::Item> {
        let rest = &self.s[self.pos..];
        let mut it = rest.char_indices();
        let (_, first) = it.next()?;
        let mut end = first.len_utf8();
        let mut prev = first;
        let mut regional = usize::from(is_regional(first));
        for (i, c) in it {
            let join = prev == ZWJ
                || (c != '\n' && !c.is_control() && c.width() == Some(0))
                || (regional == 1 && is_regional(c));
            if !join {
                break;
            }
            if is_regional(c) {
                regional += 1;
            }
            end = i + c.len_utf8();
            prev = c;
        }
        let g = &rest[..end];
        self.pos += end;
        let w = if g.len() == first.len_utf8() {
            first.width().unwrap_or(0)
        } else {
            g.width()
        };
        Some((g, w))
    }
}

/// Display width in terminal cells.
pub fn width(s: &str) -> usize {
    clusters(s).map(|(_, w)| w).sum()
}

pub fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| width(&s.content)).sum()
}

/// Truncate to at most `max` cells, ending with `…` when shortened.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for (g, gw) in clusters(s) {
        if w + gw > max - 1 {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    out.push('…');
    out
}

/// Truncate a styled line to `max` cells.
pub fn truncate_line(line: Line<'static>, max: usize) -> Line<'static> {
    if line_width(&line) <= max {
        return line;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in line.spans {
        let w = width(&span.content);
        if used + w < max {
            used += w;
            out.push(span);
            continue;
        }
        let room = max.saturating_sub(used);
        let t = truncate(&span.content, room);
        let tw = width(&t);
        out.push(Span::styled(t, span.style));
        used += tw;
        break;
    }
    if used < max && !out.last().is_some_and(|s| s.content.ends_with('…')) {
        let style = out.last().map(|s| s.style).unwrap_or_default();
        out.push(Span::styled("…", style));
    }
    Line::from(out).style(line.style)
}

/// Make arbitrary text safe to draw: removes ANSI escape sequences and
/// control characters, applies carriage returns (last write wins) and
/// expands tabs to 4-column stops.
pub fn sanitize(s: &str) -> Cow<'_, str> {
    if !s.chars().any(|c| c.is_control() && c != '\n') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut line_start = 0;
    let mut col = 0usize;
    let mut pending_cr = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                out.push('\n');
                line_start = out.len();
                col = 0;
                pending_cr = false;
            }
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    pending_cr = true;
                }
            }
            '\x1b' => skip_escape(&mut chars),
            '\t' => {
                if pending_cr {
                    out.truncate(line_start);
                    col = 0;
                    pending_cr = false;
                }
                let n = 4 - col % 4;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\u{8}' => {
                // Backspace: drop the previous char on this line.
                if out.len() > line_start
                    && let Some(p) = out.pop()
                {
                    col = col.saturating_sub(p.width().unwrap_or(0));
                }
            }
            c if c.is_control() => {}
            c => {
                if pending_cr {
                    out.truncate(line_start);
                    col = 0;
                    pending_cr = false;
                }
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    Cow::Owned(out)
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        Some('[') => {
            // CSI: parameters then one final byte in @..~
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
            // String until BEL or ST (ESC \)
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        Some('(') | Some(')') | Some('*') | Some('+') | Some('#') | Some('%') => {
            chars.next();
        }
        _ => {}
    }
}

/// Owned span shorthand.
pub fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

fn prefix_width(p: &[Span<'_>]) -> usize {
    p.iter().map(|s| width(&s.content)).sum()
}

enum Tok<'a> {
    Word(&'a str, usize),
    Space(&'a str, usize),
    Break,
}

fn flush_tok<'a>(out: &mut Vec<Tok<'a>>, kind: u8, text: &'a str, w: usize) {
    if text.is_empty() {
        return;
    }
    match kind {
        1 => out.push(Tok::Word(text, w)),
        2 => out.push(Tok::Space(text, w)),
        _ => {}
    }
}

fn tokenize(s: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut acc_w = 0;
    // 0 = none, 1 = word, 2 = space
    let mut kind = 0u8;
    let mut pos = 0;
    let flush = flush_tok;
    let iter = clusters(s).peekable();
    for (g, w) in iter {
        let len = g.len();
        if g == "\n" {
            flush(&mut out, kind, &s[start..pos], acc_w);
            out.push(Tok::Break);
            pos += len;
            start = pos;
            acc_w = 0;
            kind = 0;
            continue;
        }
        let is_space = g.chars().all(char::is_whitespace);
        if is_space {
            if kind != 2 {
                flush(&mut out, kind, &s[start..pos], acc_w);
                start = pos;
                acc_w = 0;
                kind = 2;
            }
            acc_w += w.max(1);
        } else if w >= 2 {
            // Wide (CJK) characters are their own break opportunities.
            flush(&mut out, kind, &s[start..pos], acc_w);
            out.push(Tok::Word(g, w));
            start = pos + len;
            acc_w = 0;
            kind = 0;
        } else {
            if kind != 1 {
                flush(&mut out, kind, &s[start..pos], acc_w);
                start = pos;
                acc_w = 0;
                kind = 1;
            }
            acc_w += w;
        }
        pos += len;
    }
    flush(&mut out, kind, &s[start..pos], acc_w);
    out
}

struct Builder<'p> {
    width: usize,
    cont: &'p [Span<'static>],
    cont_w: usize,
    lines: Vec<Line<'static>>,
    cur: Vec<(String, Style)>,
    cur_w: usize,
    content: bool,
}

impl<'p> Builder<'p> {
    fn new(width: usize, first: &[Span<'static>], cont: &'p [Span<'static>]) -> Self {
        let mut b = Self {
            width,
            cont,
            cont_w: prefix_width(cont),
            lines: Vec::new(),
            cur: Vec::new(),
            cur_w: 0,
            content: false,
        };
        for s in first {
            b.push_raw(&s.content, s.style, width_of(&s.content));
        }
        b
    }

    fn push_raw(&mut self, text: &str, style: Style, w: usize) {
        if text.is_empty() {
            return;
        }
        match self.cur.last_mut() {
            Some((t, s)) if *s == style => t.push_str(text),
            _ => self.cur.push((text.to_string(), style)),
        }
        self.cur_w += w;
    }

    fn push(&mut self, text: &str, style: Style, w: usize) {
        self.push_raw(text, style, w);
        self.content = true;
    }

    fn newline(&mut self) {
        let spans: Vec<Span<'static>> = self
            .cur
            .drain(..)
            .map(|(t, s)| Span::styled(t, s))
            .collect();
        self.lines.push(Line::from(spans));
        self.cur_w = 0;
        self.content = false;
        let cont = self.cont;
        for s in cont {
            self.push_raw(&s.content, s.style, width_of(&s.content));
        }
    }

    fn room(&self) -> usize {
        self.width.saturating_sub(self.cur_w)
    }

    /// Place clusters one by one, breaking lines as needed.
    fn hard(&mut self, text: &str, style: Style) {
        for (g, w) in clusters(text) {
            if self.content && self.cur_w + w > self.width {
                self.newline();
            }
            self.push(g, style, w);
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        let spans: Vec<Span<'static>> = self
            .cur
            .drain(..)
            .map(|(t, s)| Span::styled(t, s))
            .collect();
        self.lines.push(Line::from(spans));
        self.lines
    }
}

fn width_of(s: &str) -> usize {
    width(s)
}

/// Word-wrap styled spans to `width` cells. `first` prefixes the first line,
/// `cont` every following one. Newlines in span text force breaks; leading
/// whitespace of a logical line is kept, whitespace at wrap points dropped.
/// Always returns at least one line.
pub fn wrap_spans(
    spans: &[Span<'_>],
    width: usize,
    first: &[Span<'static>],
    cont: &[Span<'static>],
) -> Vec<Line<'static>> {
    /// A word may span styles ("**4 minutes**." is one word): its parts.
    enum Unit {
        Break,
        Space(String, Style, usize),
        Word(Vec<(String, Style, usize)>),
    }
    let mut units: Vec<Unit> = Vec::new();
    for sp in spans {
        let style = sp.style;
        for (i, tok) in tokenize(&sp.content).into_iter().enumerate() {
            match tok {
                Tok::Break => units.push(Unit::Break),
                Tok::Space(t, w) => units.push(Unit::Space(t.to_string(), style, w)),
                Tok::Word(t, w) => match units.last_mut() {
                    // Glued to the previous span's last word (no space between).
                    Some(Unit::Word(parts)) if i == 0 => parts.push((t.to_string(), style, w)),
                    _ => units.push(Unit::Word(vec![(t.to_string(), style, w)])),
                },
            }
        }
    }
    let min = prefix_width(first).max(prefix_width(cont)) + 1;
    let width = width.max(min);
    let mut b = Builder::new(width, first, cont);
    let mut logical_start = true;
    let mut pending: Vec<(String, Style, usize)> = Vec::new();
    for u in units {
        match u {
            Unit::Break => {
                pending.clear();
                b.newline();
                logical_start = true;
            }
            Unit::Space(t, style, w) => {
                if b.content {
                    pending.push((t, style, w));
                } else if logical_start {
                    // Indentation: keep it, hard-breaking if absurdly wide.
                    b.hard(&t, style);
                }
            }
            Unit::Word(parts) => {
                logical_start = false;
                let w: usize = parts.iter().map(|p| p.2).sum();
                let pw: usize = pending.iter().map(|p| p.2).sum();
                if !b.content || b.cur_w + pw + w <= b.width {
                    if b.content {
                        for (pt, ps, pwid) in pending.drain(..) {
                            b.push(&pt, ps, pwid);
                        }
                    }
                    pending.clear();
                    if w <= b.room() {
                        for (t, st, pw) in &parts {
                            b.push(t, *st, *pw);
                        }
                    } else {
                        for (t, st, _) in &parts {
                            b.hard(t, *st);
                        }
                    }
                    continue;
                }
                pending.clear();
                if w <= b.width.saturating_sub(b.cont_w) {
                    b.newline();
                    for (t, st, pw) in &parts {
                        b.push(t, *st, *pw);
                    }
                } else {
                    // Longer than a whole line: fill the current one first.
                    let style = parts.first().map(|p| p.1).unwrap_or_default();
                    if b.room() > 1 {
                        b.push(" ", style, 1);
                    } else {
                        b.newline();
                    }
                    for (t, st, _) in &parts {
                        b.hard(t, *st);
                    }
                }
            }
        }
    }
    b.finish()
}

/// Wrap plain text with one style.
pub fn wrap_plain(
    text: &str,
    style: Style,
    width: usize,
    first: &[Span<'static>],
    cont: &[Span<'static>],
) -> Vec<Line<'static>> {
    wrap_spans(&[Span::styled(text, style)], width, first, cont)
}

/// Text with `code` in it as spans: the code in `code` style, without its
/// backticks. Unpaired backticks stay as they are.
pub fn code_spans(text: &str, style: Style, code: Style) -> Vec<Span<'static>> {
    if !text.matches('`').count().is_multiple_of(2) {
        return vec![Span::styled(text.to_string(), style)];
    }
    text.split('`')
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(i, part)| Span::styled(part.to_string(), if i % 2 == 1 { code } else { style }))
        .collect()
}

/// Character-wrap a single line (code, tool output): no word breaking.
pub fn hard_wrap(
    text: &str,
    style: Style,
    width: usize,
    first: &[Span<'static>],
    cont: &[Span<'static>],
) -> Vec<Line<'static>> {
    let min = prefix_width(first).max(prefix_width(cont)) + 1;
    let width = width.max(min);
    let mut b = Builder::new(width, first, cont);
    for (g, w) in clusters(text) {
        if b.content && b.cur_w + w > b.width {
            b.newline();
        }
        b.push(g, style, w);
    }
    b.finish()
}

/// Pad a line with spaces to exactly `w` cells (truncating when longer).
pub fn pad_line(line: Line<'static>, w: usize, align: Align) -> Line<'static> {
    let lw = line_width(&line);
    if lw >= w {
        return truncate_line(line, w);
    }
    let gap = w - lw;
    let (left, right) = match align {
        Align::Left => (0, gap),
        Align::Right => (gap, 0),
        Align::Center => (gap / 2, gap - gap / 2),
    };
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    spans.extend(line.spans);
    if right > 0 {
        spans.push(Span::raw(" ".repeat(right)));
    }
    Line::from(spans)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Plain text of a line.
#[cfg(test)]
pub fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Compact token count with at most three digits: 950, 1.23k, 42.3k, 262k, 1.5M, 15.3M, 1.5B.
/// The same rules as the app's `fmtTokens` (apps/tessera/ui/src/lib/util.ts): a number that rounds
/// up to 1,000 of its unit is shown in the next one ("1M", never "1000k").
pub fn tokens(n: u64) -> String {
    const UNITS: [&str; 5] = ["", "k", "M", "B", "T"];
    if n < 1000 {
        return n.to_string();
    }
    let (mut unit, mut scale) = (0, 1u64);
    while n / scale >= 1000 && unit < UNITS.len() - 1 {
        scale *= 1000;
        unit += 1;
    }
    let mut decimals: u32 = if n < scale * 10 {
        2
    } else if n < scale * 100 {
        1
    } else {
        0
    };
    // Whole numbers throughout: a half rounds up the way it reads (1,005 is 1.01k).
    let step = scale / 10u64.pow(decimals);
    let mut shown = n.saturating_add(step / 2) / step;
    if shown >= 1000 * 10u64.pow(decimals) && unit < UNITS.len() - 1 {
        shown = 1;
        decimals = 0;
        unit += 1;
    }
    let (whole, frac) = (shown / 10u64.pow(decimals), shown % 10u64.pow(decimals));
    if frac == 0 {
        format!("{whole}{}", UNITS[unit])
    } else {
        let frac = format!("{frac:0width$}", width = decimals as usize);
        format!("{whole}.{}{}", frac.trim_end_matches('0'), UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(line_text).collect()
    }

    #[test]
    fn wraps_words() {
        let l = wrap_plain(
            "the quick brown fox jumps over the lazy dog",
            Style::new(),
            10,
            &[],
            &[],
        );
        assert_eq!(
            texts(&l),
            vec!["the quick", "brown fox", "jumps over", "the lazy", "dog"]
        );
        for line in &l {
            assert!(line_width(line) <= 10);
        }
    }

    #[test]
    fn code_shows_without_its_backticks() {
        let code = Style::new().add_modifier(ratatui::style::Modifier::BOLD);
        let s = code_spans("`bash` changes files", Style::new(), code);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].content.as_ref(), s[0].style), ("bash", code));
        assert_eq!(s[1].content, " changes files");
        // An unpaired backtick is just a character.
        let s = code_spans("it's `odd", Style::new(), code);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].content, "it's `odd");
    }

    #[test]
    fn words_stay_whole_across_styles() {
        // "**4 minutes**." must not leave the full stop alone on a line.
        let spans = [
            Span::raw("after that about "),
            Span::styled(
                "4 minutes",
                Style::new().add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::raw(". Next"),
        ];
        let lines = wrap_spans(&spans, 25, &[], &[]);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(text, vec!["after that about 4", "minutes. Next"]);
    }

    #[test]
    fn wraps_with_prefixes() {
        let first = [span("› ", Style::new())];
        let cont = [span("  ", Style::new())];
        let l = wrap_plain("aaa bbb ccc", Style::new(), 7, &first, &cont);
        assert_eq!(texts(&l), vec!["› aaa", "  bbb", "  ccc"]);
    }

    #[test]
    fn hard_breaks_long_words() {
        let l = wrap_plain("abcdefghijklmnop", Style::new(), 5, &[], &[]);
        assert_eq!(texts(&l), vec!["abcde", "fghij", "klmno", "p"]);
    }

    #[test]
    fn cjk_width_and_breaks() {
        assert_eq!(width("你好"), 4);
        assert_eq!(width("a你b"), 4);
        let l = wrap_plain("你好世界你好世界", Style::new(), 6, &[], &[]);
        assert_eq!(texts(&l), vec!["你好世", "界你好", "世界"]);
        for line in &l {
            assert!(line_width(line) <= 6);
        }
        // Mixed text never exceeds the width, even with odd remainders.
        let l = wrap_plain("ab你好cd世界ef", Style::new(), 5, &[], &[]);
        for line in &l {
            assert!(line_width(line) <= 5, "{:?}", texts(&l));
        }
    }

    #[test]
    fn emoji_and_combining() {
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("👍"), 2);
        assert_eq!(clusters("👨\u{200d}👩\u{200d}👧").count(), 1);
        assert_eq!(clusters("🇳🇱🇩🇪").count(), 2);
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("你好世界", 5), "你好…");
    }

    #[test]
    fn keeps_newlines_and_indent() {
        let l = wrap_plain("a\n  b\n\nc", Style::new(), 20, &[], &[]);
        assert_eq!(texts(&l), vec!["a", "  b", "", "c"]);
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(sanitize("a\tb"), "a   b");
        assert_eq!(sanitize("10%\r20%\r30%\n"), "30%\n");
        assert_eq!(sanitize("x\r\ny"), "x\ny");
        assert_eq!(sanitize("\x1b]0;title\x07ok"), "ok");
        assert_eq!(sanitize("bell\x07"), "bell");
    }

    #[test]
    fn tiny_widths_do_not_panic() {
        for w in 0..4 {
            let _ = wrap_plain(
                "hello 你好 world",
                Style::new(),
                w,
                &[span("› ", Style::new())],
                &[],
            );
            let _ = hard_wrap("hello 你好 world", Style::new(), w, &[], &[]);
        }
        assert!(!wrap_plain("", Style::new(), 10, &[], &[]).is_empty());
    }

    #[test]
    fn token_format() {
        for (n, want) in [
            (0, "0"),
            (950, "950"),
            (999, "999"),
            (1000, "1k"),
            (1005, "1.01k"),
            (1234, "1.23k"),
            (1500, "1.5k"),
            (9995, "10k"),
            (42_300, "42.3k"),
            (99_949, "99.9k"),
            (99_950, "100k"),
            (262_144, "262k"),
            (999_499, "999k"),
            (999_500, "1M"),
            (999_999, "1M"),
            (1_000_000, "1M"),
            (1_048_576, "1.05M"),
            (15_300_000, "15.3M"),
            (123_456_789, "123M"),
            (999_499_999, "999M"),
            (999_500_000, "1B"),
            (1_499_999_999, "1.5B"),
            (2_500_000_000, "2.5B"),
            (15_300_000_000, "15.3B"),
            (999_999_999_999, "1T"),
            (2_500_000_000_000, "2.5T"),
            (u64::MAX, "18446744T"),
        ] {
            assert_eq!(tokens(n), want, "{n}");
        }
    }
}
