//! The local app login is issued without retiring anything, saved and
//! read back first, then its older logins can be retired. No token is ever
//! used as an argument or included in an error.

use std::future::Future;
#[cfg(test)]
use std::path::Path;

use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Issued {
    pub device_id: String,
    pub token: String,
    pub server_key: String,
    pub url: String,
}

pub fn parse(bytes: &[u8]) -> Result<Issued, String> {
    let issued: Issued = serde_json::from_slice(bytes).map_err(|_| {
        "Tessera didn't return a complete local app key. Update Tessera and try again.".to_string()
    })?;
    // A kept id is public; a malformed reply must not put a token in argv.
    if issued.device_id.len() != 22
        || !issued
            .device_id
            .bytes()
            .all(|c| b"0123456789abcdefghjkmnpqrstvwxyz".contains(&c))
        || issued.token.is_empty()
        || issued.server_key.is_empty()
    {
        return Err(
            "Tessera didn't return a complete local app key. Update Tessera and try again.".into(),
        );
    }
    Ok(issued)
}

/// Only publish the new in-memory config once its exact bytes are durable.
/// The caller holds the config lock through this whole operation, including
/// read-back, so another app-settings write cannot slip between them.
#[cfg(test)]
pub fn save<C: serde::Serialize>(current: &mut C, next: C, file: &Path) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(&next)
        .map_err(|_| "Couldn't prepare this app's local login to save.".to_string())?;
    super::private_file::write_verified(file, &data)
        .map_err(|e| format!("Couldn't save and verify this app's local login: {e}"))?;
    *current = next;
    Ok(())
}

/// Cleanup failure is nonfatal: the new login is already durably saved.
/// A timeout may follow a commit, so some old logins may remain usable.
/// Cancellation after saving must not revoke the new login.
pub async fn save_then_retire<T, F: Future<Output = Result<(), String>>>(
    save: impl FnOnce() -> Result<T, String>,
    retire: impl FnOnce() -> F,
) -> Result<(T, Result<(), String>), String> {
    let saved = save()?;
    Ok((saved, retire().await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn malformed_reply_never_echoes_a_token_or_uses_it_as_the_kept_id() {
        for bytes in [
            br#"{"device_id":"tsr_private","token":"x","server_key":"k","url":"http://127.0.0.1"}"#
                .as_slice(),
            br#"{"device_id":false,"token":"tsr_private"}"#.as_slice(),
            br#"{"token":"tsr_private","server_key":3}"#.as_slice(),
        ] {
            let e = parse(bytes).err().unwrap();
            assert!(!e.contains("tsr_private"));
        }
        let issued = parse(br#"{"device_id":"0000000000000000000001","token":"tsr_private","server_key":"k","url":"http://127.0.0.1"}"#).unwrap();
        assert_eq!(issued.device_id, "0000000000000000000001");
    }

    #[tokio::test]
    async fn failed_save_never_retires_or_changes_in_memory_config() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        std::fs::create_dir(&file).unwrap();
        let mut cfg = "old login".to_string();
        let mut retires = 0;
        let r = save_then_retire(
            || save(&mut cfg, "new login".into(), &file),
            || {
                retires += 1;
                std::future::ready(Ok(()))
            },
        )
        .await;
        assert!(r.is_err());
        assert_eq!(cfg, "old login");
        assert_eq!(retires, 0);
    }

    #[tokio::test]
    async fn retirement_only_sees_a_saved_login_and_failure_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        let mut cfg = "old login".to_string();
        let (_, retired) = save_then_retire(
            || save(&mut cfg, "new login".into(), &file),
            || async {
                assert_eq!(std::fs::read(&file).unwrap(), b"\"new login\"");
                Err("cleanup didn't finish".into())
            },
        )
        .await
        .unwrap();
        assert!(retired.is_err());
        assert_eq!(cfg, "new login");
        assert_eq!(std::fs::read(&file).unwrap(), b"\"new login\"");
    }

    #[tokio::test]
    async fn closing_before_save_does_not_retire_and_closing_after_save_keeps_login() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        super::super::private_file::write_verified(&file, b"old login").unwrap();
        let mut cfg = "old login".to_string();
        let retires = Arc::new(AtomicUsize::new(0));
        // An unpolled startup (the app closes before saving).
        let future = save_then_retire(
            || save(&mut cfg, "new login".into(), &file),
            || async {
                retires.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        drop(future);
        assert_eq!(std::fs::read(&file).unwrap(), b"old login");
        assert_eq!(retires.load(Ordering::SeqCst), 0);
        // Poll through save, then drop the actual future while retirement is pending.
        {
            let (entered, mut rx) = tokio::sync::oneshot::channel();
            let future = save_then_retire(
                || save(&mut cfg, "new login".into(), &file),
                || async {
                    entered.send(()).unwrap();
                    std::future::pending::<()>().await;
                    retires.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            );
            tokio::pin!(future);
            tokio::select! {
                biased;
                _ = &mut future => panic!("retirement should still be pending"),
                _ = &mut rx => {}
            }
        }
        assert_eq!(cfg, "new login");
        assert_eq!(std::fs::read(&file).unwrap(), b"\"new login\"");
        assert_eq!(retires.load(Ordering::SeqCst), 0);
    }
}
