//! Startup files changed by agents (desktop control), kept so they can be
//! put back: `tessera node restore`, Settings → Devices → This computer.
//!
//! The machine keeps a copy of every startup file's last-known content
//! (`node-backups/` in Tessera's data directory: owner-only and out of reach
//! of the actions it records). Before a changing action it brings that copy
//! up to date with what is on disk (a look at each file's size and time;
//! only changed files are read), so a change you made yourself is never
//! mistaken for an agent's. Afterwards it looks again, and every startup
//! file that changed, appeared or disappeared gets an entry with its
//! content before and after. Putting one back is itself an entry, so it can
//! be undone the same way.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest;
use tessera_proto::StartupChange;

use crate::config::Paths;

/// Largest startup file kept (larger ones are noted, not kept).
const MAX_FILE: u64 = 2 << 20;
/// Files looked at per scan.
const MAX_FILES: usize = 20_000;
/// Entries kept (the oldest go first, with content nothing else needs).
const MAX_CHANGES: usize = 500;

pub fn dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("node-backups")
}

/// What a file looked like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ino: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Known {
    meta: Meta,
    /// Content hash; `None`: too large to keep.
    hash: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    files: BTreeMap<String, Known>,
    #[serde(default)]
    next_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: u64,
    ts: u64,
    path: String,
    /// Content before (`None`: it didn't exist).
    before: Option<String>,
    after: Option<String>,
    existed: bool,
    exists: bool,
    action: String,
    #[serde(default)]
    by: String,
    /// It existed but was too large to keep.
    #[serde(default)]
    lost: bool,
}

/// The startup files as they were before an action.
pub struct Snapshot(BTreeMap<PathBuf, Meta>);

pub struct Backups {
    dir: PathBuf,
}

impl Backups {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn private_dir(p: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Exclusive use of the backups (the node and `tessera node restore`
    /// may both write); released when dropped.
    fn lock(&self) -> Option<std::fs::File> {
        Self::private_dir(&self.dir.join("objects")).ok()?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("lock"))
            .ok()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return None;
            }
        }
        Some(f)
    }

    fn index(&self) -> Index {
        std::fs::read_to_string(self.dir.join("index.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_index(&self, index: &Index) {
        if let Ok(text) = serde_json::to_string(index) {
            let _ =
                crate::config::write_atomic(&self.dir.join("index.json"), text.as_bytes(), true);
        }
    }

    fn entries(&self) -> Vec<Entry> {
        std::fs::read_to_string(self.dir.join("changes.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn object(&self, hash: &str) -> PathBuf {
        self.dir.join("objects").join(hash)
    }

    /// Keep `data`; its hash.
    fn store(&self, data: &[u8]) -> Option<String> {
        let hash = hex(&sha2::Sha256::digest(data));
        let p = self.object(&hash);
        if !p.exists() {
            crate::config::write_atomic(&p, data, true).ok()?;
        }
        Some(hash)
    }

    /// The file's content kept now (`None`: too large or unreadable).
    fn keep_file(&self, path: &Path, meta: &Meta) -> Option<String> {
        if meta.size > MAX_FILE {
            return None;
        }
        let data = std::fs::read(path).ok()?;
        self.store(&data)
    }

    /// Before a changing action: bring the kept copies up to date.
    pub fn before(&self, roots: &[PathBuf]) -> Snapshot {
        let _lock = self.lock();
        let now = scan(roots);
        let mut index = self.index();
        index.files.retain(|p, _| now.contains_key(Path::new(p)));
        for (p, meta) in &now {
            let key = p.display().to_string();
            if index.files.get(&key).is_some_and(|k| k.meta == *meta) {
                continue;
            }
            let hash = self.keep_file(p, meta);
            index.files.insert(
                key,
                Known {
                    meta: meta.clone(),
                    hash,
                },
            );
        }
        self.save_index(&index);
        Snapshot(now)
    }

    /// After it: an entry for every startup file that changed. Returns the
    /// changed paths.
    pub fn after(&self, roots: &[PathBuf], snap: Snapshot, action: &str, by: &str) -> Vec<String> {
        let _lock = self.lock();
        let now = scan(roots);
        let mut index = self.index();
        let mut changed = Vec::new();
        let mut keys: Vec<&PathBuf> = snap.0.keys().chain(now.keys()).collect();
        keys.sort();
        keys.dedup();
        let mut new_entries = Vec::new();
        for p in keys {
            let (was, is) = (snap.0.get(p), now.get(p));
            if was == is {
                continue;
            }
            let key = p.display().to_string();
            let before = was.and_then(|_| index.files.get(&key).and_then(|k| k.hash.clone()));
            let after = is.and_then(|m| self.keep_file(p, m));
            match is {
                Some(m) => {
                    index.files.insert(
                        key.clone(),
                        Known {
                            meta: m.clone(),
                            hash: after.clone(),
                        },
                    );
                }
                None => {
                    index.files.remove(&key);
                }
            }
            index.next_id += 1;
            new_entries.push(Entry {
                id: index.next_id,
                ts: crate::util::now_ms(),
                path: key.clone(),
                lost: was.is_some() && before.is_none(),
                before,
                after,
                existed: was.is_some(),
                exists: is.is_some(),
                action: action.into(),
                by: crate::util::one_line(by, 120),
            });
            changed.push(key);
        }
        self.save_index(&index);
        self.append(&new_entries);
        changed
    }

    fn append(&self, entries: &[Entry]) {
        if entries.is_empty() {
            return;
        }
        let path = self.dir.join("changes.jsonl");
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        if let Ok(mut f) = opts.open(&path) {
            for e in entries {
                if let Ok(mut line) = serde_json::to_string(e) {
                    line.push('\n');
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
        self.prune();
    }

    /// Keep the newest entries, and only the content they need.
    fn prune(&self) {
        let all = self.entries();
        if all.len() <= MAX_CHANGES {
            return;
        }
        let keep = &all[all.len() - MAX_CHANGES..];
        let text: String = keep
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .map(|l| l + "\n")
            .collect();
        let _ = crate::config::write_atomic(&self.dir.join("changes.jsonl"), text.as_bytes(), true);
        let index = self.index();
        let mut needed: std::collections::HashSet<String> = keep
            .iter()
            .flat_map(|e| [e.before.clone(), e.after.clone()])
            .flatten()
            .collect();
        needed.extend(index.files.values().filter_map(|k| k.hash.clone()));
        if let Ok(rd) = std::fs::read_dir(self.dir.join("objects")) {
            for o in rd.flatten() {
                if !needed.contains(&o.file_name().to_string_lossy().to_string()) {
                    let _ = std::fs::remove_file(o.path());
                }
            }
        }
    }

    /// The changes kept, newest first.
    pub fn list(&self, limit: usize) -> Vec<StartupChange> {
        let mut all = self.entries();
        all.reverse();
        all.truncate(limit);
        all.into_iter().map(view).collect()
    }

    pub fn count(&self) -> u32 {
        self.entries().len() as u32
    }

    /// Put a startup file back as it was before change `id`. `roots`: the
    /// startup files and folders (only those can be put back).
    pub fn restore(&self, id: u64, roots: &[PathBuf]) -> Result<StartupChange, String> {
        let e = self
            .entries()
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("there is no change {id}"))?;
        let path = PathBuf::from(&e.path);
        if !roots.iter().any(|r| path.starts_with(r)) {
            return Err(format!("{} isn't a startup file", e.path));
        }
        if e.lost {
            return Err(format!(
                "{} was too large to keep, so it can't be put back",
                e.path
            ));
        }
        let snap = self.before(roots);
        match &e.before {
            Some(hash) => {
                let data = std::fs::read(self.object(hash))
                    .map_err(|_| format!("the kept copy of {} is gone", e.path))?;
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
                }
                // In place, so a link (a dotfiles manager's) stays a link.
                std::fs::write(&path, data).map_err(|err| format!("{}: {err}", e.path))?;
            }
            None => match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(format!("{}: {err}", e.path)),
            },
        }
        self.after(roots, snap, "restore", &format!("putting back change {id}"));
        Ok(view(e))
    }
}

fn view(e: Entry) -> StartupChange {
    StartupChange {
        id: e.id,
        ts: e.ts,
        path: e.path,
        existed: e.existed,
        exists: e.exists,
        action: e.action,
        by: e.by,
        lost: e.lost,
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn meta_of(m: &std::fs::Metadata) -> Meta {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Meta {
            size: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ino: m.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        let t = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        Meta {
            size: m.len(),
            mtime: t.as_secs() as i64,
            mtime_ns: t.subsec_nanos() as i64,
            ino: 0,
        }
    }
}

/// Every regular file under `roots` (following a link at a root, as a
/// dotfiles manager makes them; not below it), with what it looks like.
pub fn scan(roots: &[PathBuf]) -> BTreeMap<PathBuf, Meta> {
    let mut out = BTreeMap::new();
    for r in roots {
        let Ok(m) = std::fs::metadata(r) else {
            continue;
        };
        if m.is_file() {
            out.insert(r.clone(), meta_of(&m));
        } else if m.is_dir() {
            walk(r, &mut out, 0);
        }
    }
    out
}

fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Meta>, depth: u32) {
    if depth > 8 || out.len() >= MAX_FILES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() {
            walk(&p, out, depth + 1);
        } else if t.is_file() {
            if let Ok(m) = e.metadata() {
                out.insert(p, meta_of(&m));
            }
        } else if t.is_symlink()
            && let Ok(m) = std::fs::metadata(&p)
            && m.is_file()
        {
            // A linked file: what it leads to is what runs.
            out.insert(p, meta_of(&m));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_kept_and_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let hypr = home.join(".config/hypr");
        std::fs::create_dir_all(&hypr).unwrap();
        std::fs::write(hypr.join("hyprland.conf"), "gaps_in = 5\n").unwrap();
        std::fs::write(home.join(".bashrc"), "alias ll='ls -l'\n").unwrap();
        let roots = vec![hypr.clone(), home.join(".bashrc"), home.join(".profile")];
        let b = Backups::new(dir.path().join("backups"));

        // You change a file yourself: not an agent's change.
        let snap = b.before(&roots);
        assert!(b.after(&roots, snap, "run", "chat").is_empty());
        std::fs::write(hypr.join("hyprland.conf"), "gaps_in = 6\n").unwrap();

        // An agent's command changes one, adds one, removes one.
        let snap = b.before(&roots);
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(hypr.join("hyprland.conf"), "gaps_in = 20\n").unwrap();
        std::fs::write(home.join(".profile"), "export X=1\n").unwrap();
        std::fs::remove_file(home.join(".bashrc")).unwrap();
        let changed = b.after(&roots, snap, "run", "chat \"Tidy my desktop\"");
        assert_eq!(changed.len(), 3, "{changed:?}");
        let list = b.list(10);
        assert_eq!(list.len(), 3);
        let conf = list
            .iter()
            .find(|c| c.path.ends_with("hyprland.conf"))
            .unwrap();
        assert!(conf.existed && conf.exists && conf.action == "run" && conf.by.contains("Tidy"));
        let profile = list.iter().find(|c| c.path.ends_with(".profile")).unwrap();
        assert!(!profile.existed && profile.exists);
        let bashrc = list.iter().find(|c| c.path.ends_with(".bashrc")).unwrap();
        assert!(bashrc.existed && !bashrc.exists);

        // Put each back: the content from before (yours, not the agent's).
        b.restore(conf.id, &roots).unwrap();
        assert_eq!(
            std::fs::read_to_string(hypr.join("hyprland.conf")).unwrap(),
            "gaps_in = 6\n"
        );
        b.restore(profile.id, &roots).unwrap();
        assert!(
            !home.join(".profile").exists(),
            "a file the agent added goes away"
        );
        b.restore(bashrc.id, &roots).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join(".bashrc")).unwrap(),
            "alias ll='ls -l'\n"
        );
        // Putting back is itself recorded (and could be undone).
        let list = b.list(10);
        assert_eq!(list.len(), 6);
        assert_eq!(list[0].action, "restore");
        assert!(b.restore(999, &roots).is_err());
        // Only startup files can be put back.
        assert!(b.restore(conf.id, &[home.join(".profile")]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_startup_file_stays_linked() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let dots = dir.path().join("dotfiles");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&dots).unwrap();
        std::fs::write(dots.join("bashrc"), "one\n").unwrap();
        std::os::unix::fs::symlink(dots.join("bashrc"), home.join(".bashrc")).unwrap();
        let roots = vec![home.join(".bashrc")];
        let b = Backups::new(dir.path().join("backups"));
        let snap = b.before(&roots);
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(home.join(".bashrc"), "two\n").unwrap();
        b.after(&roots, snap, "write", "chat");
        let id = b.list(1)[0].id;
        b.restore(id, &roots).unwrap();
        assert!(
            std::fs::symlink_metadata(home.join(".bashrc"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(dots.join("bashrc")).unwrap(),
            "one\n"
        );
    }
}
