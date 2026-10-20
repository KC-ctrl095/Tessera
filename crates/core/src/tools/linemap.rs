//! Where the lines of one version of a file went in the next.
//!
//! Used to rebase line-anchored edits made against an older read, to carry
//! "seen" line ranges across edits, and to tell diagnostics an edit
//! introduced from ones that were already there.

use similar::{Algorithm, DiffOp};

/// Fate of an old line (0-based indices throughout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapped {
    /// Unchanged, now at this line.
    Same(usize),
    /// Changed or deleted; this is the nearest line in the new version.
    Changed(usize),
}

impl Mapped {
    pub fn line(self) -> usize {
        match self {
            Mapped::Same(n) | Mapped::Changed(n) => n,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LineMap {
    old_to_new: Vec<Mapped>,
    /// New lines that were inserted or rewritten.
    new_changed: Vec<bool>,
}

/// Lines without their terminators (`\n` / `\r\n`), so a missing final
/// newline or a CRLF file doesn't make every line look different.
pub fn split_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

impl LineMap {
    pub fn new(old: &str, new: &str) -> Self {
        Self::from_lines(&split_lines(old), &split_lines(new))
    }

    pub fn from_lines(old: &[&str], new: &[&str]) -> Self {
        let ops = similar::TextDiff::configure()
            .algorithm(Algorithm::Myers)
            .timeout(std::time::Duration::from_millis(400))
            .diff_slices(old, new)
            .ops()
            .to_vec();
        let mut old_to_new = vec![Mapped::Changed(0); old.len()];
        let mut new_changed = vec![false; new.len()];
        let last_new = new.len().saturating_sub(1);
        for op in ops {
            match op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    for k in 0..len {
                        old_to_new[old_index + k] = Mapped::Same(new_index + k);
                    }
                }
                DiffOp::Delete {
                    old_index,
                    old_len,
                    new_index,
                } => {
                    for k in 0..old_len {
                        old_to_new[old_index + k] = Mapped::Changed(new_index.min(last_new));
                    }
                }
                DiffOp::Insert {
                    new_index, new_len, ..
                } => {
                    for flag in new_changed.iter_mut().skip(new_index).take(new_len) {
                        *flag = true;
                    }
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    for k in 0..old_len {
                        let n = new_index + k.min(new_len.saturating_sub(1));
                        old_to_new[old_index + k] = Mapped::Changed(n.min(last_new));
                    }
                    for flag in new_changed.iter_mut().skip(new_index).take(new_len) {
                        *flag = true;
                    }
                }
            }
        }
        Self {
            old_to_new,
            new_changed,
        }
    }

    pub fn map(&self, old_line: usize) -> Option<Mapped> {
        self.old_to_new.get(old_line).copied()
    }

    /// The new line was inserted or rewritten by the change.
    pub fn is_changed(&self, new_line: usize) -> bool {
        self.new_changed.get(new_line).copied().unwrap_or(false)
    }

    /// Indices of new lines that were inserted or rewritten.
    pub fn changed_lines(&self) -> impl Iterator<Item = usize> + '_ {
        self.new_changed
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(|(i, _)| i)
    }

    pub fn old_len(&self) -> usize {
        self.old_to_new.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_shifted_changed_and_inserted_lines() {
        let m = LineMap::new("a\nb\nc\nd\n", "new\na\nb\nC\nd");
        assert_eq!(m.map(0), Some(Mapped::Same(1)));
        assert_eq!(m.map(1), Some(Mapped::Same(2)));
        assert_eq!(m.map(2), Some(Mapped::Changed(3)));
        assert_eq!(
            m.map(3),
            Some(Mapped::Same(4)),
            "missing final newline is not a change"
        );
        assert!(m.is_changed(0) && m.is_changed(3) && !m.is_changed(1));
        assert_eq!(m.changed_lines().collect::<Vec<_>>(), vec![0, 3]);
        let crlf = LineMap::new("a\r\nb\r\n", "a\nb\n");
        assert_eq!(crlf.map(1), Some(Mapped::Same(1)));
        let del = LineMap::new("a\nb\nc\n", "a\nc\n");
        assert_eq!(del.map(1), Some(Mapped::Changed(1)));
        assert_eq!(del.map(2), Some(Mapped::Same(1)));
    }
}
