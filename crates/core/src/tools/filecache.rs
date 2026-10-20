//! What each session has been shown of each file.
//!
//! Every `read`, `grep`, `edit`, `write` and `lsp` result records the file
//! version it showed (its tag and, within a memory budget, its content) and
//! which lines of it were visible. Line-anchored edits use this to
//! - rebase an edit made against an older version when the lines it targets
//!   did not change since (`files::rebase`), and
//! - refuse edits to lines the model was never shown.
//!
//! Bounded: at most [`MAX_SESSIONS`] sessions, [`MAX_PATHS`] paths per
//! session and [`MAX_VERSIONS`] versions per path; file contents are kept
//! only below [`MAX_FILE`] bytes and within [`MAX_BYTES`] overall (the oldest
//! contents are dropped first, keeping their tags and seen lines).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use super::linemap::{LineMap, Mapped};

pub const MAX_SESSIONS: usize = 64;
pub const MAX_PATHS: usize = 256;
pub const MAX_VERSIONS: usize = 4;
pub const MAX_FILE: usize = 4 * 1024 * 1024;
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

/// What the kept file text may add up to: [`MAX_BYTES`], less on a small
/// machine (8 MB under 1 GB; see `budget.rs`).
pub fn max_bytes() -> usize {
    crate::budget::get().file_cache.min(MAX_BYTES)
}

/// Sorted, merged, inclusive 1-based line ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet(Vec<(usize, usize)>);

impl RangeSet {
    pub fn insert(&mut self, a: usize, b: usize) {
        if a == 0 || b < a {
            return;
        }
        let mut out = Vec::with_capacity(self.0.len() + 1);
        let (mut a, mut b) = (a, b);
        let mut placed = false;
        for &(x, y) in &self.0 {
            if y + 1 < a {
                out.push((x, y));
            } else if b + 1 < x {
                if !placed {
                    out.push((a, b));
                    placed = true;
                }
                out.push((x, y));
            } else {
                a = a.min(x);
                b = b.max(y);
            }
        }
        if !placed {
            out.push((a, b));
        }
        out.sort_unstable();
        self.0 = out;
    }

    pub fn insert_lines(&mut self, lines: impl IntoIterator<Item = usize>) {
        for n in lines {
            self.insert(n, n);
        }
    }

    pub fn contains(&self, n: usize) -> bool {
        self.0.iter().any(|&(a, b)| a <= n && n <= b)
    }

    /// Lines in `a..=b` that are not in the set, as ranges.
    pub fn missing(&self, a: usize, b: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut from = a;
        for &(x, y) in &self.0 {
            if y < from {
                continue;
            }
            if x > b {
                break;
            }
            if x > from {
                out.push((from, x - 1));
            }
            from = y + 1;
            if from > b {
                return out;
            }
        }
        if from <= b {
            out.push((from, b));
        }
        out
    }

    pub fn ranges(&self) -> &[(usize, usize)] {
        &self.0
    }

    /// Carry the set across a change: unchanged lines keep their seen state
    /// at their new position.
    pub fn mapped(&self, map: &LineMap) -> RangeSet {
        let mut out = RangeSet::default();
        for &(a, b) in &self.0 {
            for n in a..=b.min(map.old_len()) {
                if let Some(Mapped::Same(m)) = map.map(n - 1) {
                    out.insert(m + 1, m + 1);
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct Version {
    pub tag: String,
    /// `None` when the file was too large or the memory budget ran out.
    pub content: Option<Arc<str>>,
    pub seen: RangeSet,
    used: u64,
}

#[derive(Default)]
struct PathEntry {
    versions: VecDeque<Version>,
    used: u64,
}

#[derive(Default)]
struct SessionEntry {
    paths: HashMap<PathBuf, PathEntry>,
    used: u64,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, SessionEntry>,
    bytes: usize,
    tick: u64,
}

#[derive(Default)]
pub struct FileCache {
    state: Mutex<State>,
}

/// Canonical key for a path (symlinks resolved where the file exists).
pub fn key(path: &Path) -> PathBuf {
    super::canonical(path)
}

impl FileCache {
    /// Record that `lines` of this version were shown. `content` may be
    /// skipped (e.g. for grep hits beyond the first few files); a version
    /// recorded without content can't be rebased from.
    pub fn saw(
        &self,
        session: &str,
        path: &Path,
        tag: &str,
        content: Option<&str>,
        lines: impl IntoIterator<Item = (usize, usize)>,
    ) {
        let mut st = self.state.lock();
        let v = st.version_mut(session, &key(path), tag, content);
        for (a, b) in lines {
            v.seen.insert(a, b);
        }
        st.enforce_budget();
    }

    /// Record a version the session produced itself (`write`, `edit`,
    /// `lsp rename`): lines carried over from `old` keep their seen state,
    /// and `extra` lines (what the result showed, what the model wrote) are
    /// seen too.
    pub fn wrote(
        &self,
        session: &str,
        path: &Path,
        old: Option<&str>,
        new: &str,
        extra: impl IntoIterator<Item = usize>,
    ) {
        let key = key(path);
        let tag = super::files::file_tag(new);
        let mut st = self.state.lock();
        let mut seen = old
            .and_then(|o| {
                let old_tag = super::files::file_tag(o);
                st.find(session, &key, &old_tag).map(|v| v.seen.clone())
            })
            .map(|s| s.mapped(&LineMap::new(old.unwrap_or(""), new)))
            .unwrap_or_default();
        seen.insert_lines(extra);
        let v = st.version_mut(session, &key, &tag, Some(new));
        for &(a, b) in seen.ranges() {
            v.seen.insert(a, b);
        }
        st.enforce_budget();
    }

    /// The recorded version with this tag, if any.
    pub fn version(&self, session: &str, path: &Path, tag: &str) -> Option<Version> {
        let mut st = self.state.lock();
        let key = key(path);
        let tick = st.bump();
        let v = st.find_mut(session, &key, tag)?;
        v.used = tick;
        Some(v.clone())
    }

    /// The version of `path` this session saw or wrote last, if any.
    pub fn latest(&self, session: &str, path: &Path) -> Option<Version> {
        let st = self.state.lock();
        st.sessions
            .get(session)?
            .paths
            .get(&key(path))?
            .versions
            .iter()
            .max_by_key(|v| v.used)
            .cloned()
    }

    pub fn forget_session(&self, session: &str) {
        let mut st = self.state.lock();
        if let Some(s) = st.sessions.remove(session) {
            let freed: usize = s
                .paths
                .values()
                .flat_map(|p| p.versions.iter())
                .filter_map(|v| v.content.as_ref().map(|c| c.len()))
                .sum();
            st.bytes = st.bytes.saturating_sub(freed);
        }
    }

    #[cfg(test)]
    fn bytes(&self) -> usize {
        self.state.lock().bytes
    }

    /// Sessions with recorded files, and the bytes of file text kept.
    pub fn stats(&self) -> (usize, usize) {
        let st = self.state.lock();
        (st.sessions.len(), st.bytes)
    }
}

impl State {
    fn bump(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn find(&self, session: &str, key: &Path, tag: &str) -> Option<&Version> {
        self.sessions
            .get(session)?
            .paths
            .get(key)?
            .versions
            .iter()
            .find(|v| v.tag == tag)
    }

    fn find_mut(&mut self, session: &str, key: &Path, tag: &str) -> Option<&mut Version> {
        self.sessions
            .get_mut(session)?
            .paths
            .get_mut(key)?
            .versions
            .iter_mut()
            .find(|v| v.tag == tag)
    }

    fn version_mut(
        &mut self,
        session: &str,
        key: &Path,
        tag: &str,
        content: Option<&str>,
    ) -> &mut Version {
        let tick = self.bump();
        if !self.sessions.contains_key(session) && self.sessions.len() >= MAX_SESSIONS {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                let freed: usize = self.sessions[&k]
                    .paths
                    .values()
                    .flat_map(|p| p.versions.iter())
                    .filter_map(|v| v.content.as_ref().map(|c| c.len()))
                    .sum();
                self.sessions.remove(&k);
                self.bytes = self.bytes.saturating_sub(freed);
            }
        }
        let mut freed = 0usize;
        let s = self.sessions.entry(session.to_string()).or_default();
        s.used = tick;
        if !s.paths.contains_key(key) && s.paths.len() >= MAX_PATHS {
            let oldest = s
                .paths
                .iter()
                .min_by_key(|(_, p)| p.used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest
                && let Some(p) = s.paths.remove(&k)
            {
                freed += p
                    .versions
                    .iter()
                    .filter_map(|v| v.content.as_ref().map(|c| c.len()))
                    .sum::<usize>();
            }
        }
        let p = s.paths.entry(key.to_path_buf()).or_default();
        p.used = tick;
        let mut added = 0usize;
        if let Some(i) = p.versions.iter().position(|v| v.tag == tag) {
            // Most recent last.
            let mut v = p.versions.remove(i).expect("index is valid");
            if v.content.is_none()
                && let Some(c) = content.filter(|c| c.len() <= MAX_FILE)
            {
                added = c.len();
                v.content = Some(Arc::from(c));
            }
            v.used = tick;
            p.versions.push_back(v);
        } else {
            if p.versions.len() >= MAX_VERSIONS
                && let Some(old) = p.versions.pop_front()
            {
                freed += old.content.map(|c| c.len()).unwrap_or(0);
            }
            let content = content.filter(|c| c.len() <= MAX_FILE).map(Arc::from);
            added = content.as_ref().map(|c: &Arc<str>| c.len()).unwrap_or(0);
            p.versions.push_back(Version {
                tag: tag.to_string(),
                content,
                seen: RangeSet::default(),
                used: tick,
            });
        }
        self.bytes = (self.bytes + added).saturating_sub(freed);
        self.sessions
            .get_mut(session)
            .and_then(|s| s.paths.get_mut(key))
            .and_then(|p| p.versions.back_mut())
            .expect("just inserted")
    }

    /// Drop the least recently used contents until under budget.
    fn enforce_budget(&mut self) {
        while self.bytes > max_bytes() {
            let mut oldest: Option<(u64, String, PathBuf, String)> = None;
            for (sid, s) in &self.sessions {
                for (path, p) in &s.paths {
                    for v in &p.versions {
                        if v.content.is_some() && oldest.as_ref().is_none_or(|o| v.used < o.0) {
                            oldest = Some((v.used, sid.clone(), path.clone(), v.tag.clone()));
                        }
                    }
                }
            }
            let Some((_, sid, path, tag)) = oldest else {
                self.bytes = 0;
                return;
            };
            if let Some(v) = self.find_mut(&sid, &path, &tag)
                && let Some(c) = v.content.take()
            {
                self.bytes = self.bytes.saturating_sub(c.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_merge_and_report_gaps() {
        let mut r = RangeSet::default();
        r.insert(10, 20);
        r.insert(5, 6);
        r.insert(21, 25);
        r.insert(7, 7);
        assert_eq!(r.ranges(), &[(5, 7), (10, 25)]);
        assert_eq!(r.missing(1, 30), vec![(1, 4), (8, 9), (26, 30)]);
        assert!(r.missing(10, 25).is_empty());
        assert!(r.contains(12) && !r.contains(9));
        r.insert(8, 9);
        assert_eq!(r.ranges(), &[(5, 25)]);
    }

    #[test]
    fn seen_lines_follow_edits_and_budgets_hold() {
        let c = FileCache::default();
        let p = Path::new("/nonexistent/tessera-test/a.txt");
        let v1 = "a\nb\nc\nd\n";
        let t1 = crate::tools::files::file_tag(v1);
        c.saw("s", p, &t1, Some(v1), [(3, 4)]);
        // An insert at the top shifts what was seen; the new line was written.
        let v2 = "new\na\nb\nc\nd\n";
        c.wrote("s", p, Some(v1), v2, [1]);
        let t2 = crate::tools::files::file_tag(v2);
        let got = c.version("s", p, &t2).unwrap();
        assert_eq!(got.seen.ranges(), &[(1, 1), (4, 5)]);
        assert_eq!(got.content.as_deref(), Some(v2));
        // Other sessions see nothing.
        assert!(c.version("other", p, &t2).is_none());
        // Versions per path are capped.
        for i in 0..10 {
            let v = format!("{i}\n");
            c.wrote("s", p, None, &v, []);
        }
        assert!(c.version("s", p, &t1).is_none());
        c.forget_session("s");
        assert_eq!(c.bytes(), 0);
    }
}
