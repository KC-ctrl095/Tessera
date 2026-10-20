//! The machine's local log (`node-log.jsonl` in Tessera's data directory,
//! owner-only and out of reach of the actions it records): one JSON line
//! per action (started, done, refused…) and per state change.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use tessera_proto::{NodeAction, NodeLogEntry, NodeLogEvent};

/// The log is started afresh past this size (the previous one is kept).
const ROTATE_BYTES: u64 = 2 << 20;

pub struct NodeLog {
    path: PathBuf,
    lock: Mutex<()>,
}

/// An entry with just the event and detail.
pub fn entry(event: NodeLogEvent, detail: impl Into<String>) -> NodeLogEntry {
    NodeLogEntry {
        ts: crate::util::now_ms(),
        event,
        call_id: None,
        session_id: None,
        chat: None,
        action: None,
        target: None,
        detail: detail.into(),
        duration_ms: None,
        bot: None,
    }
}

/// What the log keeps of an action's target: one line, shortened, and
/// with anything credential-looking scrubbed.
pub fn target_of(action: &NodeAction) -> String {
    crate::util::truncate(
        &crate::tools::scrub_patterns(&crate::util::visible_controls(&action.target())),
        400,
    )
}

fn previous(path: &Path) -> PathBuf {
    path.with_extension("1.jsonl")
}

impl NodeLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry (best effort: a full disk must not stop the node).
    pub fn add(&self, e: &NodeLogEntry) {
        let _g = self.lock.lock();
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > ROTATE_BYTES) {
            let _ = std::fs::rename(&self.path, previous(&self.path));
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let Ok(mut f) = opts.open(&self.path) else {
            return;
        };
        if let Ok(mut line) = serde_json::to_string(e) {
            line.push('\n');
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// The last `limit` entries, newest first.
    pub fn read(path: &Path, limit: usize) -> Vec<NodeLogEntry> {
        let mut all: Vec<NodeLogEntry> = Vec::new();
        for p in [previous(path), path.to_path_buf()] {
            if let Ok(text) = std::fs::read_to_string(&p) {
                all.extend(
                    text.lines()
                        .filter_map(|l| serde_json::from_str::<NodeLogEntry>(l).ok()),
                );
            }
        }
        all.reverse();
        all.truncate(limit);
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_newest_first_and_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let log = NodeLog::new(dir.path().join("node-log.jsonl"));
        log.add(&entry(NodeLogEvent::Enabled, "on"));
        let mut e = entry(NodeLogEvent::Started, "");
        e.target = Some(target_of(&NodeAction::Run {
            command: "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789 gh pr list".into(),
            cwd: "/w".into(),
            timeout_secs: None,
            admin: false,
        }));
        log.add(&e);
        let got = NodeLog::read(log.path(), 10);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event, NodeLogEvent::Started);
        assert!(
            !got[0].target.as_ref().unwrap().contains("ghp_abc"),
            "{got:?}"
        );
        assert_eq!(NodeLog::read(log.path(), 1).len(), 1);
    }
}
