//! Only bytes the user selected reach a file input. Browser commands never
//! get to name a file from the host's filesystem.
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine as _;
use tessera_proto::Attachment;

pub(crate) const MAX_BYTES: usize = 4 << 20;
const MAX_KEPT: usize = 256 << 20;
const MAX_FILES: usize = 8;

pub(crate) struct Uploads {
    dir: PathBuf,
    kept: usize,
    streams: HashMap<String, Stream>,
    free: fn(&std::path::Path) -> Result<u64, String>,
}
/// Rolls back staged files if an asynchronous hand-off fails or is cancelled.
pub(crate) struct Rollback<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> Rollback<F> {
    pub(crate) fn new(f: F) -> Self {
        Self(Some(f))
    }
    pub(crate) fn commit(mut self) {
        self.0.take();
    }
}
impl<F: FnOnce()> Drop for Rollback<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

impl Uploads {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            kept: 0,
            streams: HashMap::new(),
            free: crate::disk::available,
        }
    }

    #[cfg(test)]
    pub(crate) fn free_for_test(&mut self, free: fn(&std::path::Path) -> Result<u64, String>) {
        self.free = free;
    }
    pub fn reset(&mut self) -> Result<(), String> {
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir)
                .map_err(|_| "can't clear the previous upload copies")?;
        }
        super::private_dir(&self.dir).map_err(|_| "can't create the upload folder")?;
        self.kept = 0;
        self.streams.clear();
        Ok(())
    }

    pub fn stage(&mut self, files: &[Attachment]) -> Result<Vec<String>, String> {
        self.stage_limit(files, MAX_BYTES)
    }

    pub(crate) fn stage_agent(&mut self, files: &[Attachment]) -> Result<Vec<String>, String> {
        self.stage_limit(files, super::agent_upload::MAX_AGENT_BYTES)
    }

    fn stage_limit(&mut self, files: &[Attachment], limit: usize) -> Result<Vec<String>, String> {
        if files.len() > MAX_FILES {
            return Err("choose at most 8 files at a time".into());
        }
        let mut bytes = Vec::new();
        let mut total = 0usize;
        for f in files {
            if f.name.is_empty() || super::safe_name(&f.name) != f.name {
                return Err("that file name can't be uploaded; rename it first".into());
            }
            if f.data.len() > limit.div_ceil(3) * 4 {
                return Err(format!("choose files totaling at most {} MiB", limit >> 20));
            }
            let b = base64::engine::general_purpose::STANDARD
                .decode(&f.data)
                .map_err(|_| "the selected file isn't readable")?;
            total = total
                .checked_add(b.len())
                .ok_or("the selected files are too large")?;
            if total > limit {
                return Err(format!("choose files totaling at most {} MiB", limit >> 20));
            }
            bytes.push(b);
        }
        if self.kept + total > MAX_KEPT {
            return Err("the upload copies have reached 256 MB; close unused web views and restart their browser before choosing more files".into());
        }
        crate::disk::admit((self.free)(&self.dir)?, total as u64)?;
        let batch = self.dir.join(crate::util::random_token(16));
        super::private_dir(&batch).map_err(|_| "can't prepare the selected files")?;
        let mut paths = Vec::new();
        let result = (|| {
            for (i, (f, b)) in files.iter().zip(bytes).enumerate() {
                // Separate folders also preserve two selected files whose names match.
                let parent = batch.join(i.to_string());
                super::private_dir(&parent).map_err(|_| "can't prepare the selected files")?;
                let path = parent.join(&f.name);
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options
                    .open(&path)
                    .and_then(|mut file| file.write_all(&b))
                    .map_err(|_| "can't copy the selected file for the page")?;
                paths.push(path.display().to_string());
            }
            Ok(paths)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(batch);
        } else {
            self.kept += total;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(name: &str) -> Attachment {
        Attachment {
            name: name.into(),
            mime: "text/plain".into(),
            data: "aG9tZXdvcms=".into(),
        }
    }
    #[test]
    fn only_selected_bytes_are_staged_and_copies_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let mut uploads = Uploads::new(dir.path().join("uploads"));
        uploads.free_for_test(|_| Ok(u64::MAX));
        uploads.reset().unwrap();
        let paths = uploads
            .stage(&[file("notes.txt"), file("notes.txt")])
            .unwrap();
        assert_ne!(paths[0], paths[1]);
        for p in paths {
            assert_eq!(std::fs::read(&p).unwrap(), b"homework");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert!(uploads.stage(&[file("../notes.txt")]).is_err());
        assert!(uploads.stage(&[file("/etc/notes.txt")]).is_err());
        assert!(uploads.stage(&[]).is_ok());
        let mut bad = file("bad.txt");
        bad.data = "not base64".into();
        assert!(uploads.stage(&[bad]).is_err());
        assert!(uploads.stage(&vec![file("notes.txt"); 9]).is_err());
        uploads.kept = MAX_KEPT;
        assert!(uploads.stage(&[file("notes.txt")]).is_err());
        uploads.free_for_test(|_| Ok(u64::MAX));
        uploads.reset().unwrap();
        assert_eq!(uploads.kept, 0);
    }
}

// Chunking keeps a 20 MB phone upload from creating several full base64/JSON
// copies in memory at once. All paths below are generated here, never on wire.
pub(crate) const MAX_STREAM: usize = 200 << 20;
pub(crate) const CHUNK: usize = 256 << 10;
const STREAM_LIFE: Duration = Duration::from_secs(3600);
struct Stream {
    owner: String,
    batch: PathBuf,
    paths: Vec<String>,
    sizes: Vec<u32>,
    received: Vec<u32>,
    total: usize,
    at: Instant,
}

impl Uploads {
    pub(crate) fn expire(&mut self) {
        let old: Vec<String> = self
            .streams
            .iter()
            .filter(|(_, s)| s.at.elapsed() > STREAM_LIFE)
            .map(|(k, _)| k.clone())
            .collect();
        for key in old {
            if let Some(s) = self.streams.remove(&key) {
                self.kept = self.kept.saturating_sub(s.total);
                let _ = std::fs::remove_dir_all(s.batch);
            }
        }
    }
    pub(crate) fn start(
        &mut self,
        owner: &str,
        files: &[tessera_proto::BrowserUploadFile],
    ) -> Result<String, String> {
        self.expire();
        if files.is_empty() || files.len() > MAX_FILES {
            return Err("choose 1 to 8 files".into());
        }
        let total = files
            .iter()
            .try_fold(0usize, |n, f| n.checked_add(f.size as usize))
            .ok_or("the selected files are too large")?;
        if total > MAX_STREAM {
            return Err("choose files totaling at most 200 MB".into());
        }
        if self.kept + total > MAX_KEPT || self.streams.len() >= 8 {
            return Err(
                "upload copies are full; cancel a pending upload or restart the web-view browser"
                    .into(),
            );
        }
        for f in files {
            if f.name.is_empty() || super::safe_name(&f.name) != f.name {
                return Err("rename that file before uploading it".into());
            }
        }
        // Only one pending batch for a viewer/chooser; a replaced picker cancels it.
        let old: Vec<String> = self
            .streams
            .iter()
            .filter(|(_, s)| s.owner == owner)
            .map(|(k, _)| k.clone())
            .collect();
        for id in old {
            let _ = self.cancel(owner, &id);
        }
        let outstanding: usize = self
            .streams
            .values()
            .map(|s| s.total - s.received.iter().map(|n| *n as usize).sum::<usize>())
            .sum();
        crate::disk::admit((self.free)(&self.dir)?, (outstanding + total) as u64)?;
        let token = crate::util::random_token(24);
        let batch = self.dir.join(crate::util::random_token(16));
        super::private_dir(&batch).map_err(|_| "can't prepare the selected files")?;
        let result = (|| {
            let mut paths = Vec::new();
            for (i, f) in files.iter().enumerate() {
                let dir = batch.join(i.to_string());
                super::private_dir(&dir).map_err(|_| "can't prepare the selected files")?;
                let path = dir.join(&f.name);
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options
                    .open(&path)
                    .map_err(|_| "can't copy this selected file")?;
                paths.push(path.display().to_string());
            }
            Ok::<_, String>(paths)
        })();
        let paths = match result {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&batch);
                return Err(e);
            }
        };
        self.streams.insert(
            token.clone(),
            Stream {
                owner: owner.into(),
                batch,
                paths,
                sizes: files.iter().map(|f| f.size).collect(),
                received: vec![0; files.len()],
                total,
                at: Instant::now(),
            },
        );
        self.kept += total;
        Ok(token)
    }
    pub(crate) fn chunk(
        &mut self,
        owner: &str,
        token: &str,
        index: u32,
        data: &str,
    ) -> Result<u32, String> {
        self.expire();
        if data.len() > CHUNK.div_ceil(3) * 4 {
            return Err("this upload chunk is too large".into());
        }
        let s = self
            .streams
            .get_mut(token)
            .filter(|s| s.owner == owner)
            .ok_or("that upload expired or belongs to another picker")?;
        let i = index as usize;
        let expected = *s.sizes.get(i).ok_or("that file wasn't selected")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "the selected file isn't readable")?;
        if bytes.len() > CHUNK || s.received[i] as usize + bytes.len() > expected as usize {
            return Err("more bytes than the selected file declared".into());
        }
        crate::disk::admit((self.free)(&self.dir)?, bytes.len() as u64)?;
        // No model can name this path. It was created exclusively above.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&s.paths[i])
            .and_then(|mut f| f.write_all(&bytes))
            .map_err(|_| "can't copy this selected file")?;
        s.received[i] += bytes.len() as u32;
        Ok(s.received.iter().sum())
    }
    pub(crate) fn finish(&mut self, owner: &str, token: &str) -> Result<Vec<String>, String> {
        self.expire();
        let s = self
            .streams
            .get(token)
            .filter(|s| s.owner == owner)
            .ok_or("that upload expired or belongs to another picker")?;
        if s.sizes != s.received {
            return Err("the selected files haven't finished sending".into());
        }
        // Retain cleanup capability until Chrome accepts the selected files.
        Ok(s.paths.clone())
    }
    pub(crate) fn commit(&mut self, owner: &str, token: &str) -> Result<(), String> {
        self.finish(owner, token)?;
        self.streams.remove(token);
        Ok(())
    }
    pub(crate) fn cancel(&mut self, owner: &str, token: &str) -> Result<(), String> {
        self.expire();
        if self.streams.get(token).is_none_or(|s| s.owner != owner) {
            return Err("that upload expired or belongs to another picker".into());
        }
        let s = self.streams.remove(token).unwrap();
        self.kept = self.kept.saturating_sub(s.total);
        std::fs::remove_dir_all(s.batch).map_err(|_| "can't clear the canceled upload".to_string())
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    #[test]
    fn reserve_and_unknown_disk_reject_before_staging_or_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut u = Uploads::new(dir.path().join("upload"));
        let files = [tessera_proto::BrowserUploadFile {
            name: "file.txt".into(),
            size: 1,
        }];
        for free in [
            |_: &std::path::Path| Ok(crate::disk::RESERVE),
            |_: &std::path::Path| Err("unknown".into()),
        ] {
            u.free_for_test(free);
            assert!(u.start("viewer:old-chooser", &files).is_err());
            assert!(!u.dir.exists());
        }
        u.free_for_test(|_| Ok(u64::MAX));
        let t = u.start("viewer:old-chooser", &files).unwrap();
        u.free_for_test(|_| Ok(crate::disk::RESERVE));
        assert!(u.chunk("viewer:old-chooser", &t, 0, "YQ==").is_err());
        assert_eq!(std::fs::metadata(&u.streams[&t].paths[0]).unwrap().len(), 0);
        u.cancel("viewer:old-chooser", &t).unwrap();
        assert_eq!(u.kept, 0);
    }

    #[test]
    fn pending_finish_retains_cancel_and_commit_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let mut u = Uploads::new(dir.path().join("upload"));
        u.free_for_test(|_| Ok(u64::MAX));
        let files = [tessera_proto::BrowserUploadFile {
            name: "file.txt".into(),
            size: 1,
        }];
        let t = u.start("viewer:old-chooser", &files).unwrap();
        u.chunk("viewer:old-chooser", &t, 0, "YQ==").unwrap();
        let paths = u.finish("viewer:old-chooser", &t).unwrap();
        assert!(u.cancel("viewer:new-chooser", &t).is_err());
        u.cancel("viewer:old-chooser", &t).unwrap();
        assert!(!std::path::Path::new(&paths[0]).exists());
        assert_eq!(u.kept, 0);
        let t = u.start("viewer:old-chooser", &files).unwrap();
        let batch = u.streams[&t].batch.clone();
        {
            let _rollback = Rollback::new(|| {
                u.cancel("viewer:old-chooser", &t).unwrap();
            });
        }
        assert!(!batch.exists());
    }
    #[test]
    fn chunked_20mb_has_private_exact_bytes_and_bound_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let mut u = Uploads::new(dir.path().join("upload"));
        u.free_for_test(|_| Ok(u64::MAX));
        u.reset().unwrap();
        let token = u
            .start(
                "viewer:chooser",
                &[tessera_proto::BrowserUploadFile {
                    name: "lecture.pdf".into(),
                    size: 20 << 20,
                }],
            )
            .unwrap();
        assert!(u.chunk("other", &token, 0, "YQ==").is_err());
        assert!(u.finish("viewer:chooser", &token).is_err());
        let chunk = base64::engine::general_purpose::STANDARD.encode(vec![b'x'; CHUNK]);
        for _ in 0..80 {
            u.chunk("viewer:chooser", &token, 0, &chunk).unwrap();
        }
        assert!(u.chunk("viewer:chooser", &token, 0, "YQ==").is_err());
        let paths = u.finish("viewer:chooser", &token).unwrap();
        assert_eq!(std::fs::metadata(&paths[0]).unwrap().len(), 20 << 20);
        assert!(std::fs::read(&paths[0]).unwrap().iter().all(|b| *b == b'x'));
        u.commit("viewer:chooser", &token).unwrap();
        assert!(u.finish("viewer:chooser", &token).is_err());
        assert!(
            u.start(
                "v",
                &[tessera_proto::BrowserUploadFile {
                    name: "../secret".into(),
                    size: 1
                }]
            )
            .is_err()
        );
        assert!(
            u.start(
                "v",
                &[tessera_proto::BrowserUploadFile {
                    name: "big".into(),
                    size: 201 << 20
                }]
            )
            .is_err()
        );
        let t = u
            .start(
                "v",
                &[tessera_proto::BrowserUploadFile {
                    name: "small".into(),
                    size: 1,
                }],
            )
            .unwrap();
        let batch = u.streams[&t].batch.clone();
        u.cancel("v", &t).unwrap();
        assert!(!batch.exists());
        let t = u
            .start(
                "v",
                &[tessera_proto::BrowserUploadFile {
                    name: "expired".into(),
                    size: 1,
                }],
            )
            .unwrap();
        let batch = u.streams[&t].batch.clone();
        u.streams.get_mut(&t).unwrap().at = Instant::now() - Duration::from_secs(3601);
        assert!(u.chunk("v", &t, 0, "YQ==").is_err());
        assert!(!batch.exists());
    }
}
