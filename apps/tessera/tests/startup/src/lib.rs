//! Tauri-free production startup paths. Login issuance and connection
//! handoff are controlled futures; config merging, private persistence,
//! sequencing, ownership and Stop/retry use the app's actual modules.

#[cfg(test)]
#[path = "../../../src-tauri/src/cli_run.rs"]
pub mod cli_run;
#[cfg(test)]
#[path = "../../../src-tauri/src/credential_write.rs"]
mod credential_write;
#[cfg(all(test, unix))]
#[path = "../../../../../crates/cli/src/data_lock.rs"]
pub mod data_lock;
#[cfg(test)]
#[path = "../../../src-tauri/src/local_login.rs"]
pub mod local_login;
#[cfg(test)]
#[path = "../../../src-tauri/src/private_file.rs"]
pub mod private_file;
#[cfg(test)]
#[path = "../../../src-tauri/src/profile_owner.rs"]
mod profile_owner;
#[cfg(test)]
#[path = "../../../src-tauri/src/server_config.rs"]
mod server_config;
#[cfg(test)]
#[path = "../../../src-tauri/src/start_job.rs"]
pub mod start_job;

#[cfg(test)]
#[path = "../../../src-tauri/src/update_checks.rs"]
mod update_checks;

#[cfg(test)]
#[path = "../../../src-tauri/src/capture/content.rs"]
mod capture_content;

#[cfg(test)]
#[path = "../../../src-tauri/src/notify/toast_content.rs"]
mod toast_content;

#[cfg(test)]
#[allow(dead_code)] // App/OS entry points are compile-checked separately.
mod notifications;

#[cfg(test)]
use private_file::write as write_private;
#[cfg(test)]
use server_config::ClientConfig;

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use server_config::{ClientConfig, SavedServer};
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;

    fn login(id: &str, token: &str) -> SavedServer {
        SavedServer {
            id: id.into(),
            name: "Desktop app".into(),
            endpoints: vec!["http://127.0.0.1:7878".into()],
            token: token.into(),
            server_key: "same-server-key".into(),
            ..Default::default()
        }
    }

    fn read(file: &std::path::Path) -> ClientConfig {
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn stop_and_retry_during_retirement_join_one_saved_login_job() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("servers.json");
        let cfg = Arc::new(Mutex::new(ClientConfig::default()));
        server_config::save_local(&mut cfg.lock(), login("old-id", "old-login"), &file).unwrap();
        let stale = server_config::redacted(&cfg.lock());
        let credentials = Arc::new(credential_write::CredentialWrites::default());
        let job = start_job::OneJob::new();
        let starts = Arc::new(AtomicUsize::new(0));
        let count = starts.clone();
        let (saved, saved_rx) = tokio::sync::oneshot::channel();
        let (retire, retire_rx) = tokio::sync::oneshot::channel();
        let path = file.clone();
        let current = cfg.clone();
        let (mut first, joined) = job.join_or_begin(move || async move {
            credentials
                .run(|| async {
                    count.fetch_add(1, Ordering::SeqCst);
                    let (id, cleanup) = local_login::save_then_retire(
                        || {
                            server_config::save_local(
                                &mut current.lock(),
                                login("new-id", "new-login"),
                                &path,
                            )
                        },
                        || async {
                            assert_eq!(read(&path).servers[0].token, "new-login");
                            saved.send(()).unwrap();
                            retire_rx.await.unwrap();
                            Ok(())
                        },
                    )
                    .await
                    .unwrap();
                    cleanup.unwrap();
                    // Actual saved entry is available to the connection handoff.
                    assert!(
                        current
                            .lock()
                            .servers
                            .iter()
                            .any(|s| s.id == id && s.token == "new-login")
                    );
                    id
                })
                .await
        });
        assert!(!joined);
        saved_rx.await.unwrap();
        assert!(server_config::save_ui(&mut cfg.lock(), stale, &file).is_err());
        assert_eq!(read(&file).servers[0].token, "new-login");
        assert!(matches!(
            start_job::wait(&mut first, std::future::ready(())).await,
            start_job::Waited::Stopped
        ));
        let count = starts.clone();
        let (mut retry, joined) = job.join_or_begin(move || async move {
            count.fetch_add(1, Ordering::SeqCst);
            "unexpected-retry".to_string()
        });
        assert!(joined);
        retire.send(()).unwrap();
        assert!(matches!(
            start_job::wait(&mut retry, std::future::pending()).await,
            start_job::Waited::Done(id) if id == "new-id"
        ));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(read(&file).active.as_deref(), Some("new-id"));
    }

    #[tokio::test]
    async fn native_pair_and_local_start_serialize_before_issuance_through_handoff_in_both_orders()
    {
        for local_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("servers.json");
            let cfg = Arc::new(Mutex::new(ClientConfig::default()));
            server_config::save_local(&mut cfg.lock(), login("old-id", "old-login"), &file)
                .unwrap();
            let credentials = Arc::new(credential_write::CredentialWrites::default());
            let first_credentials = credentials.clone();
            let first_cfg = cfg.clone();
            let first_file = file.clone();
            let (saved, saved_rx) = tokio::sync::oneshot::channel();
            let (release, release_rx) = tokio::sync::oneshot::channel();
            let first = tokio::spawn(async move {
                first_credentials
                    .run(|| async {
                        if local_first {
                            let (_, retired) = local_login::save_then_retire(
                                || {
                                    server_config::save_local(
                                        &mut first_cfg.lock(),
                                        login("first-id", "first-login"),
                                        &first_file,
                                    )
                                },
                                || async {
                                    saved.send(()).unwrap();
                                    release_rx.await.unwrap();
                                    assert_eq!(read(&first_file).servers[0].token, "first-login");
                                    Ok(())
                                },
                            )
                            .await
                            .unwrap();
                            retired.unwrap();
                        } else {
                            let (_, old) = server_config::save_paired(
                                &mut first_cfg.lock(),
                                login("first-id", "first-login"),
                                &first_file,
                            )
                            .unwrap();
                            assert_eq!(old.as_deref(), Some("old-login"));
                            saved.send(()).unwrap();
                            release_rx.await.unwrap(); // pairing's connection handoff
                            assert_eq!(read(&first_file).servers[0].token, "first-login");
                        }
                    })
                    .await
            });
            saved_rx.await.unwrap();
            let issued = AtomicUsize::new(0);
            let mut second = Box::pin(credentials.run(|| async {
                // This is the issuance boundary, not just the save boundary.
                issued.fetch_add(1, Ordering::SeqCst);
                if local_first {
                    let (_, retire) = server_config::save_paired(
                        &mut cfg.lock(),
                        login("second-id", "second-login"),
                        &file,
                    )
                    .unwrap();
                    assert_eq!(retire.as_deref(), Some("first-login"));
                } else {
                    let (_, retired) = local_login::save_then_retire(
                        || {
                            server_config::save_local(
                                &mut cfg.lock(),
                                login("second-id", "second-login"),
                                &file,
                            )
                        },
                        || async { Ok(()) },
                    )
                    .await
                    .unwrap();
                    retired.unwrap();
                }
            }));
            // Deterministic contention: no timing/sleep assumptions.
            std::future::poll_fn(|cx| {
                assert!(matches!(second.as_mut().poll(cx), Poll::Pending));
                Poll::Ready(())
            })
            .await;
            assert_eq!(issued.load(Ordering::SeqCst), 0);
            assert_eq!(read(&file).servers[0].token, "first-login");
            release.send(()).unwrap();
            first.await.unwrap();
            second.await;
            assert_eq!(issued.load(Ordering::SeqCst), 1);
            assert_eq!(read(&file).servers[0].token, "second-login");
            assert_eq!(read(&file).servers.len(), 1);
        }
    }
}
