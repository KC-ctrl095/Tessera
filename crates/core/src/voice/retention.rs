//! Retained audio commits only after bounded writes preserve the data-disk reserve.
use std::{
    io::Write,
    path::{Path, PathBuf},
};

struct Pending {
    path: PathBuf,
    file: Option<std::fs::File>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        // Close before removing, including on write/sync/admission failure on Windows.
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn write_checked(
    path: &Path,
    audio: &[u8],
    admit: &dyn Fn(u64) -> std::io::Result<()>,
) -> std::io::Result<()> {
    admit(audio.len() as u64)?;
    let tmp = path.with_extension(format!("pending-{:016x}", rand::random::<u64>()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Own the guard only after create_new succeeds: never remove someone else's file.
    let file = options.open(&tmp)?;
    let mut pending = Pending {
        path: tmp,
        file: Some(file),
    };
    let file = pending.file.as_mut().expect("pending writer");
    let mut remaining = audio.len() as u64;
    for chunk in audio.chunks(64 * 1024) {
        admit(remaining)?;
        file.write_all(chunk)?;
        remaining -= chunk.len() as u64;
    }
    admit(0)?;
    file.sync_all()?;
    drop(pending.file.take()); // Windows rename needs the writer closed.
    admit(0)?;
    std::fs::rename(&pending.path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn low_space_at_every_retention_stage_leaves_no_audio_or_temp() {
        let audio = vec![42; 128 * 1024];
        // Initial admission, both chunk writes, pre-sync and pre-commit.
        for fail_at in 0..5 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("recording.wav");
            let calls = Cell::new(0);
            let result = write_checked(&path, &audio, &|work| {
                let n = calls.get();
                calls.set(n + 1);
                let free = if n == fail_at {
                    super::super::space::RESERVE
                } else {
                    u64::MAX
                };
                // A zero-work pre-commit still needs the whole reserve.
                let free = if n == fail_at && work == 0 {
                    free - 1
                } else {
                    free
                };
                super::super::space::admit(free, work).map_err(std::io::Error::other)
            });
            assert!(result.is_err(), "failure {fail_at}");
            assert_eq!(
                std::fs::read_dir(dir.path()).unwrap().count(),
                0,
                "failure {fail_at}"
            );
        }
    }

    #[test]
    fn retention_commit_failure_cleans_temp_and_success_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.wav");
        std::fs::create_dir(&path).unwrap();
        assert!(write_checked(&path, b"audio", &|_| Ok(())).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        std::fs::remove_dir(&path).unwrap();
        write_checked(&path, b"audio", &|_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"audio");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
