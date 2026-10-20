//! Saved servers and their credential/list revision. UI snapshots cannot
//! replace a list read before a native login swap. Callers hold the config
//! mutex through checking, persistence and publication.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedServer {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Device token. Never sent to the web UI.
    #[serde(default)]
    pub token: String,
    /// Server public key pinned at pairing (base64). Kept on the Rust side.
    #[serde(default)]
    pub server_key: String,
    /// Human-comparable digest of `server_key`, for display only.
    #[serde(default, skip_deserializing)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub device_name: String,
    /// Which endpoints and headers came from the server. Rust-side only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<tessera_client::KnownRoute>,
    /// An older UI copy must not restore addresses changed by the server.
    #[serde(default)]
    pub route_rev: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_endpoint: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientConfig {
    pub servers: Vec<SavedServer>,
    pub active: Option<String>,
    /// Credentials, list or selection changed since this snapshot was read.
    /// Old profiles start at zero; a revision never wraps and becomes reusable.
    #[serde(default)]
    pub revision: u32,
}

pub(crate) fn redacted(cfg: &ClientConfig) -> ClientConfig {
    let mut c = cfg.clone();
    for s in &mut c.servers {
        s.token = String::new();
        s.fingerprint = tessera_client::key_fingerprint(&s.server_key);
        s.server_key = String::new();
        s.route = None;
    }
    c
}

/// The exact next snapshot is verified on disk before becoming current.
pub(crate) fn commit(
    current: &mut ClientConfig,
    mut next: ClientConfig,
    file: &Path,
) -> Result<(), String> {
    next.revision = current.revision.checked_add(1).ok_or(
        "This app's saved-server revision is exhausted. Its current settings have been kept.",
    )?;
    let data = serde_json::to_vec_pretty(&next)
        .map_err(|_| "Couldn't prepare this app's saved servers.".to_string())?;
    super::private_file::write_verified(file, &data)
        .map_err(|e| format!("Couldn't save and verify this app's saved servers: {e}"))?;
    *current = next;
    Ok(())
}

pub(crate) fn save_ui(
    current: &mut ClientConfig,
    next: ClientConfig,
    file: &Path,
) -> Result<(), String> {
    if next.revision != current.revision {
        return Err("Saved servers changed while this action was waiting. Read the current list and try your change again.".into());
    }
    let next = with_stored_secrets(current, next);
    commit(current, next, file)
}

/// UI tokens and pinned keys always come from the stored copy.
pub(crate) fn with_stored_secrets(cur: &ClientConfig, mut next: ClientConfig) -> ClientConfig {
    for s in &mut next.servers {
        let Some(old) = cur.servers.iter().find(|o| o.id == s.id) else {
            s.token = String::new();
            s.server_key = String::new();
            s.fingerprint = None;
            s.route = None;
            s.route_rev = 0;
            continue;
        };
        if s.route_rev != old.route_rev {
            s.endpoints = old.endpoints.clone();
            s.headers = old.headers.clone();
            s.route_rev = old.route_rev;
        }
        s.route = old.route.clone();
        s.last_endpoint = old.last_endpoint.clone();
        // An unpinned login must not follow an address changed by the UI.
        s.token = if old.server_key.is_empty() && old.endpoints != s.endpoints {
            String::new()
        } else {
            old.token.clone()
        };
        s.server_key = old.server_key.clone();
        s.fingerprint = None;
    }
    next
}

pub(crate) fn save_paired(
    current: &mut ClientConfig,
    mut server: SavedServer,
    file: &Path,
) -> Result<(SavedServer, Option<String>), String> {
    let mut next = current.clone();
    let same = next
        .servers
        .iter()
        .position(|s| !s.server_key.is_empty() && s.server_key == server.server_key);
    let retire = match same {
        Some(i) => {
            let old = next.servers.remove(i);
            server.id = old.id;
            server.route_rev = old.route_rev.wrapping_add(1);
            next.servers.insert(i, server.clone());
            Some(old.token).filter(|t| !t.is_empty() && *t != server.token)
        }
        None => {
            next.servers.push(server.clone());
            None
        }
    };
    next.active = Some(server.id.clone());
    commit(current, next, file)?;
    Ok((server, retire))
}

#[cfg(any(desktop, test))]
pub(crate) fn save_local(
    current: &mut ClientConfig,
    server: SavedServer,
    file: &Path,
) -> Result<String, String> {
    let mut next = current.clone();
    next.servers.retain(|s| {
        !s.endpoints
            .iter()
            .any(|e| tessera_client::is_loopback_endpoint(e))
    });
    let id = server.id.clone();
    next.servers.push(server);
    next.active = Some(id.clone());
    commit(current, next, file)?;
    Ok(id)
}

pub(crate) fn save_active(current: &mut ClientConfig, id: &str, file: &Path) -> Result<(), String> {
    if !current.servers.iter().any(|s| s.id == id) {
        return Err("unknown server".into());
    }
    if current.active.as_deref() != Some(id) {
        let mut next = current.clone();
        next.active = Some(id.into());
        commit(current, next, file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(id: &str, token: &str) -> SavedServer {
        SavedServer {
            id: id.into(),
            name: "This computer".into(),
            endpoints: vec!["http://127.0.0.1:7878".into()],
            token: token.into(),
            server_key: "same-key".into(),
            ..Default::default()
        }
    }

    fn read(file: &Path) -> ClientConfig {
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    #[test]
    fn stale_old_id_and_missing_new_entry_cannot_erase_a_verified_local_login() {
        for old_local in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("servers.json");
            let mut cfg = ClientConfig::default();
            let remote = SavedServer {
                id: "remote".into(),
                endpoints: vec!["https://remote.example".into()],
                token: "remote-login".into(),
                server_key: "remote-key".into(),
                ..Default::default()
            };
            save_paired(&mut cfg, remote, &file).unwrap();
            if old_local {
                save_local(&mut cfg, local("local-old-id", "old-login"), &file).unwrap();
            }
            let stale = redacted(&cfg);
            save_local(&mut cfg, local("local-new-id", "new-login"), &file).unwrap();
            let bytes = std::fs::read(&file).unwrap();
            let revision = cfg.revision;
            assert!(save_ui(&mut cfg, stale.clone(), &file).is_err());
            assert!(save_ui(&mut cfg, stale, &file).is_err(), "no stale retry");
            assert_eq!(cfg.revision, revision);
            assert_eq!(std::fs::read(&file).unwrap(), bytes);
            let persisted = read(&file);
            assert_eq!(persisted.active.as_deref(), Some("local-new-id"));
            assert_eq!(persisted.servers.len(), 2);
            assert!(
                persisted
                    .servers
                    .iter()
                    .any(|s| s.id == "local-new-id" && s.token == "new-login")
            );
            assert!(!persisted.servers.iter().any(|s| s.id == "local-old-id"));
        }
    }

    #[test]
    fn same_key_pair_replacement_and_native_selection_invalidate_ui_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        let mut cfg = ClientConfig::default();
        save_local(&mut cfg, local("local", "old-login"), &file).unwrap();
        let stale = redacted(&cfg);
        let (server, retire) =
            save_paired(&mut cfg, local("fresh-id", "paired-login"), &file).unwrap();
        assert_eq!(server.id, "local");
        assert_eq!(retire.as_deref(), Some("old-login"));
        assert!(save_ui(&mut cfg, stale, &file).is_err());
        assert_eq!(read(&file).servers[0].token, "paired-login");
        let remote = SavedServer {
            id: "remote".into(),
            ..Default::default()
        };
        save_paired(&mut cfg, remote, &file).unwrap();
        let stale = redacted(&cfg);
        save_active(&mut cfg, "local", &file).unwrap();
        assert!(save_ui(&mut cfg, stale, &file).is_err());
        assert_eq!(read(&file).active.as_deref(), Some("local"));
    }

    #[test]
    fn current_ui_change_preserves_secrets_and_bumps_the_persisted_revision() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        let mut cfg = ClientConfig::default();
        save_local(&mut cfg, local("local", "login"), &file).unwrap();
        let mut next = redacted(&cfg);
        let revision = next.revision;
        next.servers[0].name = "Renamed".into();
        next.servers[0].token = "not-from-the-ui".into();
        save_ui(&mut cfg, next, &file).unwrap();
        let saved = read(&file);
        assert_eq!(saved.revision, revision + 1);
        assert_eq!(saved.servers[0].name, "Renamed");
        assert_eq!(saved.servers[0].token, "login");
        assert_eq!(saved.servers[0].server_key, "same-key");
        assert_eq!(redacted(&saved).revision, saved.revision);
    }

    #[test]
    fn old_profiles_load_at_revision_zero_without_resetting_their_login() {
        let cfg: ClientConfig = serde_json::from_slice(br#"{"servers":[{"id":"local","name":"Saved","endpoints":["http://127.0.0.1:7878"],"token":"old-login","serverKey":"same-key"}],"active":"local"}"#).unwrap();
        assert_eq!(cfg.revision, 0);
        assert_eq!(cfg.active.as_deref(), Some("local"));
        assert_eq!(cfg.servers[0].token, "old-login");
    }

    #[test]
    fn failed_native_save_never_publishes_a_replacement_or_new_revision() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cannot-replace");
        std::fs::create_dir(&file).unwrap();
        let mut cfg = ClientConfig {
            servers: vec![local("local", "old-login")],
            active: Some("local".into()),
            revision: 4,
        };
        for paired in [false, true] {
            let result = if paired {
                save_paired(&mut cfg, local("new-id", "new-login"), &file).map(|_| ())
            } else {
                save_local(&mut cfg, local("new-id", "new-login"), &file).map(|_| ())
            };
            assert!(result.is_err());
            assert_eq!(cfg.revision, 4);
            assert_eq!(cfg.servers[0].token, "old-login");
            assert_eq!(cfg.active.as_deref(), Some("local"));
        }
    }

    #[test]
    fn revision_exhaustion_refuses_instead_of_reusing_a_stale_generation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        let mut cfg = ClientConfig {
            revision: u32::MAX,
            ..Default::default()
        };
        assert!(save_local(&mut cfg, local("local", "login"), &file).is_err());
        assert_eq!(cfg.revision, u32::MAX);
        assert!(cfg.servers.is_empty());
        assert!(!file.exists());
    }
}
