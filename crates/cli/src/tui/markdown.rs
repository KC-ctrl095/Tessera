//! Markdown → styled, pre-wrapped terminal lines.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::text::{self, Align, span};
use super::theme::Theme;

/// Render Markdown to lines no wider than `width` cells.
pub fn render(md: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut r = Renderer::new(width.max(8), theme);
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(md, opts) {
        r.event(ev);
    }
    r.finish()
}

enum Container {
    Quote,
    Item {
        marker: Vec<Span<'static>>,
        indent: usize,
        first: bool,
    },
}

struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    row: Vec<Vec<Span<'static>>>,
    header: bool,
}

struct Renderer<'t> {
    width: usize,
    theme: &'t Theme,
    out: Vec<Line<'static>>,
    inline: Vec<Span<'static>>,
    styles: Vec<Style>,
    containers: Vec<Container>,
    lists: Vec<Option<u64>>,
    pending_blank: bool,
    code: Option<(String, String)>,
    table: Option<Table>,
    links: Vec<(String, usize)>,
    heading: Option<u8>,
}

impl<'t> Renderer<'t> {
    fn new(width: usize, theme: &'t Theme) -> Self {
        Self {
            width,
            theme,
            out: Vec::new(),
            inline: Vec::new(),
            styles: Vec::new(),
            containers: Vec::new(),
            lists: Vec::new(),
            pending_blank: false,
            code: None,
            table: None,
            links: Vec::new(),
            heading: None,
        }
    }

    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::new(), |acc, s| acc.patch(*s))
    }

    fn push_text(&mut self, t: &str, style: Style) {
        if t.is_empty() {
            return;
        }
        let t = text::sanitize(t);
        match self.inline.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(&t),
            _ => self.inline.push(span(t.into_owned(), style)),
        }
    }

    /// Prefixes for the next block's first and continuation lines.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let faint = self.theme.hairline();
        let mut first = Vec::new();
        let mut cont = Vec::new();
        for c in self.containers.iter_mut() {
            match c {
                Container::Quote => {
                    first.push(span("▏ ", faint));
                    cont.push(span("▏ ", faint));
                }
                Container::Item {
                    marker,
                    indent,
                    first: is_first,
                } => {
                    if *is_first {
                        first.extend(marker.iter().cloned());
                        *is_first = false;
                    } else {
                        first.push(Span::raw(" ".repeat(*indent)));
                    }
                    cont.push(Span::raw(" ".repeat(*indent)));
                }
            }
        }
        (first, cont)
    }

    fn blank(&mut self) {
        let faint = self.theme.hairline();
        let bars: Vec<Span<'static>> = self
            .containers
            .iter()
            .filter(|c| matches!(c, Container::Quote))
            .map(|_| span("▏", faint))
            .collect();
        self.out.push(Line::from(bars));
    }

    fn flush_inline(&mut self) {
        if self.inline.iter().all(|s| s.content.trim().is_empty()) {
            self.inline.clear();
            return;
        }
        let spans = std::mem::take(&mut self.inline);
        let (first, cont) = self.prefixes();
        let lines = text::wrap_spans(&spans, self.width, &first, &cont);
        self.out.extend(lines);
    }

    fn begin_block(&mut self) {
        self.flush_inline();
        if self.pending_blank && !self.out.is_empty() {
            self.blank();
        }
        self.pending_blank = false;
    }

    fn event(&mut self, ev: Event<'_>) {
        if let Some((_, buf)) = self.code.as_mut() {
            match ev {
                Event::Text(t) => {
                    buf.push_str(&t);
                    return;
                }
                Event::End(TagEnd::CodeBlock) => {}
                _ => return,
            }
        }
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                let s = self.style();
                self.push_text(&t, s);
            }
            Event::Code(t) => {
                let s = self.style().patch(self.theme.code());
                self.push_text(&t, s);
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                let s = self.style().patch(self.theme.code());
                self.push_text(&t, s);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let s = self.style().patch(self.theme.dim());
                self.push_text(t.trim_end_matches('\n'), s);
                if t.ends_with('\n') {
                    self.push_text("\n", s);
                }
            }
            // Models use single newlines as line breaks (lists of values,
            // addresses, verse); keep them rather than reflowing.
            Event::SoftBreak => {
                let s = self.style();
                self.push_text("\n", s);
            }
            Event::HardBreak => {
                let s = self.style();
                self.push_text("\n", s);
            }
            Event::Rule => {
                self.begin_block();
                let (first, _) = self.prefixes();
                let used: usize = first.iter().map(|s| text::width(&s.content)).sum();
                let mut spans = first;
                spans.push(span(
                    "─".repeat(self.width.saturating_sub(used).min(80)),
                    self.theme.hairline(),
                ));
                self.out.push(Line::from(spans));
                self.pending_blank = true;
            }
            Event::FootnoteReference(l) => {
                let s = self.style().patch(self.theme.dim());
                self.push_text(&format!("[^{l}]"), s);
            }
            Event::TaskListMarker(checked) => {
                let (mark, style) = if checked {
                    ("■ ", self.theme.success())
                } else {
                    ("□ ", self.theme.faint())
                };
                let in_bullets = matches!(self.lists.last(), Some(None));
                if let Some(Container::Item { marker, indent, .. }) = self.containers.last_mut() {
                    if in_bullets {
                        *marker = vec![span(mark, style)];
                        *indent = 2;
                    } else {
                        marker.push(span(mark, style));
                        *indent += 2;
                    }
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.begin_block(),
            Tag::Heading { level, .. } => {
                self.begin_block();
                if !self.out.is_empty()
                    && !self.out.last().is_some_and(|l| {
                        l.spans
                            .iter()
                            .all(|s| s.content.trim().is_empty() || s.content.trim() == "▏")
                    })
                {
                    self.blank();
                }
                let n = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                self.heading = Some(n);
                self.styles.push(self.theme.heading(n));
            }
            Tag::BlockQuote(_) => {
                self.begin_block();
                self.containers.push(Container::Quote);
                self.styles.push(self.theme.muted());
            }
            Tag::CodeBlock(kind) => {
                self.begin_block();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split([',', ' ']).next().unwrap_or("").trim().to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::HtmlBlock => self.begin_block(),
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.begin_block();
                } else {
                    self.flush_inline();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush_inline();
                if self.pending_blank && !self.out.is_empty() {
                    self.blank();
                }
                self.pending_blank = false;
                let depth = self.lists.len().saturating_sub(1);
                let (marker, style) = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        (m, self.theme.dim())
                    }
                    _ => {
                        let b = ["•", "◦", "▪", "‣"][depth % 4];
                        (format!("{b} "), self.theme.dim())
                    }
                };
                let indent = text::width(&marker);
                self.containers.push(Container::Item {
                    marker: vec![span(marker, style)],
                    indent,
                    first: true,
                });
            }
            Tag::Table(aligns) => {
                self.begin_block();
                self.table = Some(Table {
                    aligns,
                    rows: Vec::new(),
                    row: Vec::new(),
                    header: false,
                });
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.row.clear();
                    t.header = true;
                }
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.row.clear();
                }
            }
            Tag::TableCell => self.inline.clear(),
            Tag::Emphasis => self
                .styles
                .push(Style::new().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self
                .styles
                .push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push((dest_url.to_string(), self.inline.len()));
                self.styles.push(self.theme.link());
            }
            Tag::Image { dest_url, .. } => {
                let s = self.style().patch(self.theme.dim());
                self.push_text("[image: ", s);
                self.links.push((dest_url.to_string(), usize::MAX));
                self.styles.push(self.theme.dim());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_inline();
                self.pending_blank = true;
            }
            TagEnd::Heading(_) => {
                self.styles.pop();
                if self.heading.take() == Some(1) {
                    // Nothing extra: bold accent already stands out.
                }
                self.flush_inline();
                self.pending_blank = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.styles.pop();
                self.containers.pop();
                self.pending_blank = true;
            }
            TagEnd::CodeBlock => {
                if let Some((lang, body)) = self.code.take() {
                    self.code_block(&lang, &body);
                }
                self.pending_blank = true;
            }
            TagEnd::HtmlBlock => {
                self.flush_inline();
                self.pending_blank = true;
            }
            TagEnd::List(_) => {
                self.flush_inline();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.pending_blank = true;
                }
            }
            TagEnd::Item => {
                self.flush_inline();
                // An item that produced no lines still consumed its marker slot.
                if let Some(Container::Item { first: true, .. }) = self.containers.last() {
                    let (first, _) = self.prefixes();
                    self.out.push(Line::from(first));
                }
                self.containers.pop();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                if let Some(t) = self.table.as_mut() {
                    t.row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.render_table(t);
                }
                self.pending_blank = true;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some((url, start)) = self.links.pop() {
                    let label: String = self
                        .inline
                        .get(start.min(self.inline.len())..)
                        .unwrap_or(&[])
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect();
                    let shown = url.trim_start_matches("mailto:");
                    if !url.is_empty()
                        && !url.starts_with('#')
                        && label.trim() != shown
                        && label.trim() != url
                    {
                        let s = self.theme.dim();
                        self.push_text(&format!(" ({url})"), s);
                    }
                }
            }
            TagEnd::Image => {
                self.styles.pop();
                let s = self.style().patch(self.theme.dim());
                self.links.pop();
                self.push_text("]", s);
            }
            _ => {}
        }
    }

    /// A fenced block: with stone surfaces, a sunken slab with its language
    /// on top (as in the apps); otherwise a hairline frame.
    fn code_block(&mut self, lang: &str, body: &str) {
        let (first, cont) = self.prefixes();
        let body = body.strip_suffix('\n').unwrap_or(body);
        let style = self.theme.code_block();
        if self.theme.surfaces() {
            let used: usize = cont.iter().map(|s| text::width(&s.content)).sum();
            let w = self.width.saturating_sub(used).max(4);
            let slab = self.theme.bg(super::theme::Tok::CodeBg);
            let pad = |mut spans: Vec<Span<'static>>, used: usize| {
                if used < w {
                    spans.push(span(" ".repeat(w - used), slab));
                }
                spans
            };
            // The label row doubles as the slab's top padding.
            let mut top = first;
            let label = if lang.is_empty() {
                String::new()
            } else {
                format!(" {lang}")
            };
            let lw = text::width(&label);
            top.extend(pad(vec![span(label, self.theme.faint().patch(slab))], lw));
            self.out.push(Line::from(top));
            let inner = w.saturating_sub(2).max(2);
            for raw in body.split('\n') {
                let clean = text::sanitize(raw);
                let clean = clean.trim_end_matches('\r');
                for l in text::hard_wrap(clean, style, inner, &[], &[]) {
                    let lw = text::line_width(&l);
                    let mut spans = cont.clone();
                    let mut row = vec![span(" ", slab)];
                    row.extend(l.spans);
                    spans.extend(pad(row, lw + 1));
                    self.out.push(Line::from(spans));
                }
            }
            let mut bottom = cont;
            bottom.extend(pad(Vec::new(), 0));
            self.out.push(Line::from(bottom));
            return;
        }
        let faint = self.theme.hairline();
        let mut top = first;
        top.push(span("╭─", faint));
        if !lang.is_empty() {
            top.push(span(" ", faint));
            top.push(span(lang.to_string(), self.theme.faint()));
        }
        self.out.push(Line::from(top));
        let mut gutter = cont.clone();
        gutter.push(span("│ ", faint));
        for raw in body.split('\n') {
            let clean = text::sanitize(raw);
            let clean = clean.trim_end_matches('\r');
            self.out
                .extend(text::hard_wrap(clean, style, self.width, &gutter, &gutter));
        }
        let mut bottom = cont;
        bottom.push(span("╰─", faint));
        self.out.push(Line::from(bottom));
    }

    fn render_table(&mut self, t: Table) {
        let faint = self.theme.hairline();
        let ncols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        let (first, cont) = self.prefixes();
        let prefix_w: usize = cont.iter().map(|s| text::width(&s.content)).sum();
        let sep_w = 3 * (ncols - 1);
        let avail = self.width.saturating_sub(prefix_w + sep_w).max(ncols);
        let cell_w =
            |c: &Vec<Span<'static>>| c.iter().map(|s| text::width(&s.content)).sum::<usize>();
        let mut widths = vec![1usize; ncols];
        for row in &t.rows {
            for (i, c) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell_w(c));
            }
        }
        // Shrink the widest columns until the table fits.
        while widths.iter().sum::<usize>() > avail {
            let (i, w) = widths
                .iter()
                .enumerate()
                .max_by_key(|(_, w)| **w)
                .map(|(i, w)| (i, *w))
                .unwrap_or((0, 1));
            if w <= 3 {
                break;
            }
            widths[i] = w - 1;
        }
        let align = |i: usize| match t.aligns.get(i) {
            Some(Alignment::Right) => Align::Right,
            Some(Alignment::Center) => Align::Center,
            _ => Align::Left,
        };
        let mut first_line = Some(first);
        let header_rows = usize::from(t.header);
        for (ri, row) in t.rows.iter().enumerate() {
            let is_header = ri < header_rows;
            let cells: Vec<Vec<Line<'static>>> = (0..ncols)
                .map(|i| {
                    let mut spans = row.get(i).cloned().unwrap_or_default();
                    if is_header {
                        // Small caps, quiet: as the apps' table heads.
                        for s in spans.iter_mut() {
                            s.content = s.content.to_uppercase().into();
                            s.style = s.style.patch(self.theme.dim());
                        }
                    }
                    text::wrap_spans(&spans, widths[i], &[], &[])
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for li in 0..height {
                let mut spans = first_line.take().unwrap_or_else(|| cont.clone());
                for (ci, cell) in cells.iter().enumerate() {
                    if ci > 0 {
                        spans.push(span(" │ ", faint));
                    }
                    let line = cell.get(li).cloned().unwrap_or_default();
                    spans.extend(text::pad_line(line, widths[ci], align(ci)).spans);
                }
                self.out.push(Line::from(spans));
            }
            if is_header {
                let mut spans = cont.clone();
                let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                spans.push(span(rule.join("─┼─"), faint));
                self.out.push(Line::from(spans));
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if let Some((lang, body)) = self.code.take() {
            // Unterminated fence (e.g. still streaming).
            self.code_block(&lang, &body);
        }
        if let Some(t) = self.table.take() {
            self.render_table(t);
        }
        self.flush_inline();
        while self
            .out
            .last()
            .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
        {
            self.out.pop();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::text::{line_text, line_width};
    use crate::tui::theme::{Depth, ThemePref};

    fn plain(md: &str, width: usize) -> Vec<String> {
        render(md, width, &Theme::test())
            .iter()
            .map(line_text)
            .collect()
    }

    #[test]
    fn paragraphs_wrap_and_separate() {
        let l = plain("Hello world, this is a paragraph.\n\nSecond one.", 16);
        assert_eq!(
            l,
            vec!["Hello world,", "this is a", "paragraph.", "", "Second one."]
        );
    }

    #[test]
    fn soft_breaks_are_kept() {
        assert_eq!(plain("1\n2\n3", 20), vec!["1", "2", "3"]);
        assert_eq!(plain("- a\n  still a", 20), vec!["• a", "  still a"]);
    }

    #[test]
    fn inline_styles() {
        let lines = render("a **bold** and *it* and `code`", 80, &Theme::test());
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(
            spans
                .iter()
                .any(|s| s.content == "bold" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content == "it" && s.style.add_modifier.contains(Modifier::ITALIC))
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content == "code" && s.style == Theme::test().code())
        );
    }

    #[test]
    fn code_block_hard_wraps_and_labels() {
        let md = "```rust\nfn main() { println!(\"a very long line that must wrap\"); }\n\tindented\n```";
        // Stone surfaces: a slab with the language on top, padded to width.
        let l = plain(md, 24);
        assert_eq!(l[0].trim_end(), " rust");
        assert!(l[1].starts_with(" fn main()"), "{l:?}");
        assert!(l.iter().any(|x| x.trim_end() == "     indented"), "{l:?}");
        for line in render(md, 24, &Theme::test()) {
            assert_eq!(line_width(&line), 24);
        }
        // Without surfaces: a hairline frame.
        let t = Theme::new(Depth::True, ThemePref::Auto, None);
        let l: Vec<String> = render(md, 24, &t).iter().map(line_text).collect();
        assert_eq!(l[0], "╭─ rust");
        assert!(l[1].starts_with("│ fn main()"));
        assert!(l.iter().any(|x| x == "│     indented"), "{l:?}");
        assert_eq!(l.last().unwrap(), "╰─");
        for line in render(md, 24, &t) {
            assert!(line_width(&line) <= 24);
        }
    }

    #[test]
    fn lists_indent_properly() {
        let md = "- one\n- two is a longer item that wraps\n  - nested\n1. first\n2. second";
        let l = plain(md, 20);
        assert_eq!(l[0], "• one");
        assert_eq!(l[1], "• two is a longer");
        assert_eq!(l[2], "  item that wraps");
        assert_eq!(l[3], "  ◦ nested");
        assert!(l.contains(&"1. first".to_string()), "{l:?}");
        assert!(l.contains(&"2. second".to_string()), "{l:?}");
    }

    #[test]
    fn ordered_list_start_and_tasks() {
        let l = plain("3. c\n4. d", 20);
        assert_eq!(l, vec!["3. c", "4. d"]);
        let l = plain("- [x] done\n- [ ] todo", 20);
        assert_eq!(l, vec!["■ done", "□ todo"]);
    }

    #[test]
    fn quotes_and_headings() {
        let l = plain("# Title\n\ntext\n\n> quoted words here", 40);
        assert_eq!(l, vec!["Title", "", "text", "", "▏ quoted words here"]);
    }

    #[test]
    fn links_show_url() {
        let l = plain("see [docs](https://x.dev/d) and <https://y.dev>", 80);
        assert_eq!(l, vec!["see docs (https://x.dev/d) and https://y.dev"]);
    }

    #[test]
    fn tables_align() {
        let md = "| name | n |\n|---|--:|\n| alpha | 1 |\n| b | 22 |";
        let l = plain(md, 40);
        assert_eq!(l[0], "NAME  │  N");
        assert_eq!(l[1], "──────┼───");
        assert_eq!(l[2], "alpha │  1");
        assert_eq!(l[3], "b     │ 22");
    }

    #[test]
    fn wide_table_shrinks() {
        let md = "| a | b |\n|---|---|\n| aaaaaaaaaaaaaaaaaaaa | bbbbbbbbbbbbbbbbbbbbbbbb |";
        for line in render(md, 20, &Theme::test()) {
            assert!(line_width(&line) <= 20, "{:?}", line_text(&line));
        }
    }

    #[test]
    fn cjk_paragraph_fits() {
        for line in render(
            "这是一个很长的中文段落，用来测试换行是否正确处理宽字符。",
            12,
            &Theme::test(),
        ) {
            assert!(line_width(&line) <= 12);
        }
    }

    #[test]
    fn unterminated_fence_and_rule() {
        let t = Theme::new(Depth::True, ThemePref::Auto, None);
        let l: Vec<String> = render("```\ncode", 20, &t).iter().map(line_text).collect();
        assert_eq!(l, vec!["╭─", "│ code", "╰─"]);
        let l = plain("a\n\n---\n\nb", 10);
        assert_eq!(l, vec!["a", "", "──────────", "", "b"]);
    }

    #[test]
    fn odd_input_does_not_panic() {
        for md in [
            "",
            "\n\n",
            "* ",
            "> ",
            "|",
            "```",
            "- [ ]",
            "#",
            "1.",
            "<div>x</div>",
            "![img](x.png)",
            "\t\tx",
            "a\u{0}b",
        ] {
            for w in [0, 1, 5, 80] {
                let _ = render(md, w, &Theme::test());
            }
        }
    }
}
