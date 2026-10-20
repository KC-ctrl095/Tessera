//! Decisions shared by the native updater and Linux-runnable tests.
use base64::Engine;
use tessera_proto::{AppUpdate, FileChunk};

pub const PLATFORM: &str = "windows-x86_64-desktop";
pub const MAX_SIZE: u64 = 1 << 30;
pub const CHUNK: u32 = 1 << 20;

pub fn offer(build: &AppUpdate, id: &str, hash: &str) -> Result<(), String> {
    if build.platform != PLATFORM
        || build.id != id
        || build.sha256 != hash
        || build.sha256.len() != 64
        || !build.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || build.size == 0
        || build.size > MAX_SIZE
    {
        return Err("The offered update changed or isn't a Windows app; check again.".into());
    }
    Ok(())
}

/// Only forward semantic versions. Same-version offers require an explicit
/// `+rebuild.<id>` marker and a different locally recorded setup hash. When
/// the installed setup's hash is unknown we fail closed, never use timestamps.
pub fn version(
    build: &AppUpdate,
    installed: &str,
    installed_setup_hash: Option<&str>,
) -> Result<(), String> {
    let offered = semver::Version::parse(&build.version)
        .map_err(|_| "The update version is invalid; nothing was run.")?;
    let current = semver::Version::parse(installed)
        .map_err(|_| "The installed version is invalid; nothing was run.")?;
    match offered.cmp_precedence(&current) {
        std::cmp::Ordering::Greater => Ok(()),
        std::cmp::Ordering::Less => Err("Downgrades are refused; nothing was run.".into()),
        std::cmp::Ordering::Equal => {
            if offered
                .build
                .as_str()
                .strip_prefix("rebuild.")
                .is_some_and(|id| !id.is_empty())
                && installed_setup_hash.is_some_and(|hash| {
                    hash.len() == 64
                        && hash.bytes().all(|b| b.is_ascii_hexdigit())
                        && !hash.eq_ignore_ascii_case(&build.sha256)
                })
            {
                Ok(())
            } else {
                Err("Same-version updates need an explicit +rebuild.<id> and a different known setup hash; nothing was run.".into())
            }
        }
    }
}

pub fn chunk(c: FileChunk, offset: u64, size: u64) -> Result<Vec<u8>, String> {
    if c.offset != offset || c.size != size || c.data.len() > (CHUNK as usize * 4 / 3 + 8) {
        return Err("The update download didn't match its size or position.".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(c.data)
        .map_err(|_| "The update download wasn't valid base64.")?;
    let end = offset
        .checked_add(bytes.len() as u64)
        .ok_or("The update download was too large.")?;
    if bytes.is_empty() || bytes.len() > CHUNK as usize || end > size || c.eof != (end == size) {
        return Err("The update download was incomplete or too large.".into());
    }
    Ok(bytes)
}

pub fn hash(actual: &str, expected: &str) -> Result<(), String> {
    if actual != expected {
        Err("The update's SHA-256 didn't match; nothing was run.".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn build() -> AppUpdate {
        AppUpdate {
            id: "updtest".into(),
            platform: PLATFORM.into(),
            version: "0.2.0".into(),
            size: 4,
            sha256: "aa".repeat(32),
            added_at: 1,
            notes: None,
        }
    }
    #[test]
    fn only_the_confirmed_windows_offer_is_accepted() {
        let b = build();
        assert!(offer(&b, &b.id, &b.sha256).is_ok());
        for field in ["platform", "id", "hash", "zero", "huge"] {
            let mut wrong = b.clone();
            match field {
                "platform" => wrong.platform = "linux-x86_64-desktop".into(),
                "id" => wrong.id = "new".into(),
                "hash" => wrong.sha256 = "broken".into(),
                "zero" => wrong.size = 0,
                _ => wrong.size = MAX_SIZE + 1,
            }
            assert!(offer(&wrong, &b.id, &b.sha256).is_err(), "{field}");
        }
    }
    fn c(data: &[u8], offset: u64, size: u64, eof: bool) -> FileChunk {
        FileChunk {
            data: base64::engine::general_purpose::STANDARD.encode(data),
            offset,
            size,
            eof,
        }
    }
    #[test]
    fn rollback_invalid_and_unmarked_rebuilds_are_refused_locally() {
        let mut b = build();
        for v in ["0.0.9", "0.1.0-beta", "bad", "1", "01.1.0", "0.1.0"] {
            b.version = v.into();
            assert!(version(&b, "0.1.0", Some(&"bb".repeat(32))).is_err(), "{v}");
        }
        b.version = "0.1.0+rebuild.second".into();
        assert!(version(&b, "0.1.0", None).is_err());
        assert!(version(&b, "0.1.0", Some(&b.sha256)).is_err());
        assert!(version(&b, "0.1.0", Some(&b.sha256.to_uppercase())).is_err());
        assert!(version(&b, "0.1.0", Some(&"z".repeat(64))).is_err());
        assert!(version(&b, "0.1.0", Some(&"bb".repeat(32))).is_ok());
        b.version = "0.2.0".into();
        assert!(version(&b, "0.1.0", None).is_ok());
        // A refreshed malicious offer cannot roll back after consent either.
        b.version = "0.0.8".into();
        assert!(version(&b, "0.1.0", None).is_err());
    }

    #[test]
    fn truncated_changed_oversize_and_wrong_offset_chunks_are_refused() {
        assert_eq!(chunk(c(b"test", 0, 4, true), 0, 4).unwrap(), b"test");
        for c in [
            c(b"", 0, 4, true),
            c(b"abc", 0, 4, true),
            c(b"test!", 0, 4, true),
            c(b"test", 1, 4, true),
            c(b"test", 0, 5, true),
            c(b"test", 0, 4, false),
        ] {
            assert!(chunk(c, 0, 4).is_err());
        }
        assert!(hash("wrong", "expected").is_err());
        assert!(hash("expected", "expected").is_ok());
    }
}
