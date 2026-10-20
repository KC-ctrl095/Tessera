//! Multi-line prompt editor with word-aware soft wrapping and history.

use super::text::clusters;

#[derive(Debug, Default, Clone)]
pub struct Editor {
    text: String,
    /// Byte offset, always on a cluster boundary.
    cursor: usize,
    /// Display column to aim for when moving vertically.
    goal_col: Option<usize>,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
}

/// One visual row: byte range into the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub rows: Vec<Row>,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

const HISTORY_MAX: usize = 500;

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_multiline(&self) -> bool {
        self.text.contains('\n')
    }

    pub fn set_text(&mut self, s: impl Into<String>) {
        self.text = s.into();
        self.cursor = self.text.len();
        self.goal_col = None;
    }

    /// Add text at the very end without moving the cursor.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.goal_col = None;
        self.hist_pos = None;
    }

    /// Take the text, leaving the editor empty.
    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        self.clear();
        t
    }

    // ---- history -------------------------------------------------------------

    pub fn set_history(&mut self, items: Vec<String>) {
        self.history = items;
        let excess = self.history.len().saturating_sub(HISTORY_MAX);
        self.history.drain(..excess);
        self.hist_pos = None;
    }

    pub fn push_history(&mut self, s: &str) {
        let s = s.trim_end();
        if s.trim().is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) != Some(s) {
            self.history.push(s.to_string());
        }
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
        self.hist_pos = None;
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let pos = match self.hist_pos {
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(p) => p - 1,
        };
        self.hist_pos = Some(pos);
        self.text = self.history[pos].clone();
        self.cursor = self.text.len();
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(p) = self.hist_pos else { return false };
        if p + 1 < self.history.len() {
            self.hist_pos = Some(p + 1);
            self.text = self.history[p + 1].clone();
        } else {
            self.hist_pos = None;
            self.text = std::mem::take(&mut self.draft);
        }
        self.cursor = self.text.len();
        true
    }

    pub fn browsing_history(&self) -> bool {
        self.hist_pos.is_some()
    }

    // ---- editing -------------------------------------------------------------

    fn edited(&mut self) {
        self.goal_col = None;
        self.hist_pos = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.edited();
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.edited();
    }

    fn prev_boundary(&self, at: usize) -> usize {
        let mut last = 0;
        let mut pos = 0;
        for (g, _) in clusters(&self.text) {
            if pos >= at {
                break;
            }
            last = pos;
            pos += g.len();
        }
        last
    }

    fn next_boundary(&self, at: usize) -> usize {
        if at >= self.text.len() {
            return self.text.len();
        }
        clusters(&self.text[at..])
            .next()
            .map(|(g, _)| at + g.len())
            .unwrap_or(self.text.len())
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let p = self.prev_boundary(self.cursor);
        self.text.replace_range(p..self.cursor, "");
        self.cursor = p;
        self.edited();
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let n = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..n, "");
        self.edited();
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary(self.cursor);
        self.goal_col = None;
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
        self.goal_col = None;
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len())
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
        self.goal_col = None;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
        self.goal_col = None;
    }

    fn word_left_pos(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');

        trimmed
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + trimmed[i..].chars().next().map(char::len_utf8).unwrap_or(1))
            .unwrap_or(0)
    }

    fn word_right_pos(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skip = after
            .find(|c: char| c.is_alphanumeric() || c == '_')
            .unwrap_or(after.len());
        let rest = &after[skip..];
        let word = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        self.cursor + skip + word
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_left_pos();
        self.goal_col = None;
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_right_pos();
        self.goal_col = None;
    }

    /// Ctrl+W: delete the word before the cursor (and whitespace before it).
    pub fn delete_word_back(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed_len = before.trim_end().len();
        let word_start = before[..trimmed_len]
            .rfind(char::is_whitespace)
            .map(|i| i + before[i..].chars().next().map(char::len_utf8).unwrap_or(1))
            .unwrap_or(0);
        self.text.replace_range(word_start..self.cursor, "");
        self.cursor = word_start;
        self.edited();
    }

    /// Alt+Backspace / Alt+D style deletion by word boundaries.
    pub fn delete_word_forward(&mut self) {
        let end = self.word_right_pos();
        self.text.replace_range(self.cursor..end, "");
        self.edited();
    }

    /// Ctrl+U: delete from the start of the line to the cursor (the whole
    /// line when the cursor is at its start).
    pub fn kill_line_back(&mut self) {
        let start = self.line_start();
        if start == self.cursor && start > 0 {
            // Join with the previous line.
            self.text.replace_range(start - 1..start, "");
            self.cursor = start - 1;
        } else {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
        self.edited();
    }

    /// Ctrl+K: delete to the end of the line.
    pub fn kill_line_forward(&mut self) {
        let end = self.line_end();
        if end == self.cursor && end < self.text.len() {
            self.text.replace_range(end..end + 1, "");
        } else {
            self.text.replace_range(self.cursor..end, "");
        }
        self.edited();
    }

    /// Replace `range` with `with`, leaving the cursor after the insertion.
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, with: &str) {
        let start = range.start.min(self.text.len());
        let end = range.end.min(self.text.len()).max(start);
        self.text.replace_range(start..end, with);
        self.cursor = start + with.len();
        self.edited();
    }

    /// The whitespace-delimited token that ends at the cursor.
    pub fn token_before_cursor(&self) -> (usize, &str) {
        let before = &self.text[..self.cursor];
        let start = before
            .rfind(char::is_whitespace)
            .map(|i| i + before[i..].chars().next().map(char::len_utf8).unwrap_or(1))
            .unwrap_or(0);
        (start, &before[start..])
    }

    // ---- layout ----------------------------------------------------------------

    /// Soft-wrap into rows of at most `width` cells, breaking after spaces
    /// where possible.
    pub fn layout(&self, width: usize) -> Layout {
        let width = width.max(2);
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in self.text.split('\n') {
            wrap_line(line, line_start, width, &mut rows);
            line_start += line.len() + 1;
        }
        // Cursor position: the last row whose start <= cursor, preferring the
        // later row at a soft-wrap boundary.
        let mut cursor_row = 0;
        for (i, r) in rows.iter().enumerate() {
            if r.start <= self.cursor {
                cursor_row = i;
            } else {
                break;
            }
        }
        // At a soft boundary the cursor belongs to the row it starts, except
        // when that row is on a different logical line (handled by split).
        let r = rows[cursor_row];
        let end = self.cursor.min(r.end.max(r.start));
        let col = super::text::width(&self.text[r.start..end.max(r.start)]);
        Layout {
            rows,
            cursor_row,
            cursor_col: col,
        }
    }

    /// Move one visual row up/down. Returns false at the first/last row.
    pub fn move_vertical(&mut self, width: usize, down: bool) -> bool {
        let lay = self.layout(width);
        let target = if down {
            if lay.cursor_row + 1 >= lay.rows.len() {
                return false;
            }
            lay.cursor_row + 1
        } else {
            if lay.cursor_row == 0 {
                return false;
            }
            lay.cursor_row - 1
        };
        let goal = *self.goal_col.get_or_insert(lay.cursor_col);
        let row = lay.rows[target];
        let mut pos = row.start;
        let mut col = 0;
        for (g, w) in clusters(&self.text[row.start..row.end]) {
            if col + w > goal {
                break;
            }
            col += w;
            pos += g.len();
        }
        self.cursor = pos;
        true
    }

    pub fn at_first_row(&self, width: usize) -> bool {
        self.layout(width).cursor_row == 0
    }

    pub fn at_last_row(&self, width: usize) -> bool {
        let l = self.layout(width);
        l.cursor_row + 1 >= l.rows.len()
    }
}

fn wrap_line(line: &str, offset: usize, width: usize, rows: &mut Vec<Row>) {
    if line.is_empty() {
        rows.push(Row {
            start: offset,
            end: offset,
        });
        return;
    }
    let mut row_start = 0;
    let mut row_w = 0;
    let mut last_space: Option<usize> = None; // byte index just after a space
    let mut pos = 0;
    let mut w_at_space = 0;
    for (g, w) in clusters(line) {
        if row_w + w > width && pos > row_start {
            // Break after the last space in this row if there is one; the
            // space stays on the previous row so the cursor can sit on it.
            let brk = match last_space {
                Some(s) if s > row_start && s < pos + g.len() => s,
                _ => pos,
            };
            rows.push(Row {
                start: offset + row_start,
                end: offset + brk,
            });
            row_w = if brk == pos { 0 } else { row_w - w_at_space };
            row_start = brk;
            last_space = None;
        }
        row_w += w;
        pos += g.len();
        if g == " " {
            last_space = Some(pos);
            w_at_space = row_w;
        }
    }
    rows.push(Row {
        start: offset + row_start,
        end: offset + line.len(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        let mut e = Editor::default();
        e.set_text(s);
        e
    }

    #[test]
    fn insert_and_delete_unicode() {
        let mut e = Editor::default();
        for c in "héllo 你好 👍".chars() {
            e.insert_char(c);
        }
        assert_eq!(e.text(), "héllo 你好 👍");
        e.backspace();
        assert_eq!(e.text(), "héllo 你好 ");
        e.left();
        e.left();
        e.backspace();
        assert_eq!(e.text(), "héllo 好 ");
        e.home();
        e.delete();
        assert_eq!(e.text(), "éllo 好 ");
        // Combining sequences are removed as one unit.
        let mut e = ed("e\u{301}");
        e.backspace();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn word_motions() {
        let mut e = ed("foo bar_baz  qux");
        e.word_left();
        assert_eq!(e.cursor(), 13);
        e.word_left();
        assert_eq!(e.cursor(), 4);
        e.word_right();
        assert_eq!(e.cursor(), 11);
        e.end();
        e.delete_word_back();
        assert_eq!(e.text(), "foo bar_baz  ");
        e.delete_word_back();
        assert_eq!(e.text(), "foo ");
    }

    #[test]
    fn kill_line() {
        let mut e = ed("one\ntwo three");
        e.kill_line_back();
        assert_eq!(e.text(), "one\n");
        e.kill_line_back();
        assert_eq!(e.text(), "one");
        e.home();
        e.kill_line_forward();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn layout_wraps_at_spaces() {
        let e = ed("hello world again");
        let l = e.layout(8);
        let rows: Vec<&str> = l.rows.iter().map(|r| &e.text()[r.start..r.end]).collect();
        assert_eq!(rows, vec!["hello ", "world ", "again"]);
        assert_eq!(l.cursor_row, 2);
        assert_eq!(l.cursor_col, 5);
    }

    #[test]
    fn layout_multiline_and_cursor() {
        let mut e = ed("ab\n\ncd");
        let l = e.layout(10);
        assert_eq!(l.rows.len(), 3);
        assert_eq!((l.cursor_row, l.cursor_col), (2, 2));
        assert!(e.move_vertical(10, false));
        assert_eq!(e.cursor(), 3);
        assert!(e.move_vertical(10, false));
        assert_eq!(e.cursor(), 2);
        assert!(!e.move_vertical(10, false));
        assert!(e.move_vertical(10, true));
        assert!(e.move_vertical(10, true));
        assert_eq!(e.cursor(), 6);
    }

    #[test]
    fn layout_wide_chars() {
        let e = ed("你好世界你好");
        let l = e.layout(5);
        for r in &l.rows {
            assert!(crate::tui::text::width(&e.text()[r.start..r.end]) <= 5);
        }
        assert_eq!(l.rows.len(), 3);
        let e = ed("");
        assert_eq!(e.layout(0).rows.len(), 1);
    }

    #[test]
    fn history_browse_restores_draft() {
        let mut e = Editor::default();
        e.push_history("first");
        e.push_history("second");
        e.set_text("draft");
        assert!(e.history_prev());
        assert_eq!(e.text(), "second");
        assert!(e.history_prev());
        assert_eq!(e.text(), "first");
        assert!(!e.history_prev());
        assert!(e.history_next());
        assert!(e.history_next());
        assert_eq!(e.text(), "draft");
        assert!(!e.history_next());
    }

    #[test]
    fn token_before_cursor() {
        let e = ed("look at @src/ma");
        assert_eq!(e.token_before_cursor(), (8, "@src/ma"));
        let mut e = ed("x");
        e.replace_range(0..1, "yz ");
        assert_eq!((e.text(), e.cursor()), ("yz ", 3));
    }

    #[test]
    fn paste_normalizes_newlines() {
        let mut e = Editor::default();
        e.insert_str("a\r\nb\rc");
        assert_eq!(e.text(), "a\nb\nc");
    }
}
