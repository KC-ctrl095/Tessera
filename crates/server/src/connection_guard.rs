//! One connection's authority and cancellation, independent of socket backpressure.
use std::future::Future;

use tessera_core::{Engine, store::Login};
use tessera_proto::{ApiError, ErrorCode};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub(crate) struct Authority {
    pub login: Login,
    // Never logged; needed to notice a replaced machine login too.
    pub token: String,
}

impl Authority {
    pub fn valid(&self, engine: &Engine) -> bool {
        engine
            .store()
            .login(&self.token)
            .is_ok_and(|l| l.as_ref() == Some(&self.login))
    }

    pub fn check(&self, engine: &Engine, cancel: &CancellationToken) -> Result<(), ApiError> {
        if cancel.is_cancelled() || !self.valid(engine) {
            cancel.cancel();
            Err(ApiError::new(
                ErrorCode::Unauthorized,
                "this device's login was revoked; pair again",
            ))
        } else {
            Ok(())
        }
    }
}

/// Biased so cancellation wins even when a queued request/permit is ready.
pub(crate) async fn while_live<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = work => Some(result),
    }
}

/// Required machine controls use the same cancellation path as revocation,
/// including tasks waiting behind a full reply queue or a held permit.
pub(crate) async fn watch_cut(cancel: CancellationToken, cut: impl Future<Output = ()>) {
    if while_live(&cancel, cut).await.is_some() {
        cancel.cancel();
    }
}
pub(crate) async fn watch_revocations(
    engine: Engine,
    authority: std::sync::Arc<Authority>,
    mut revocations: broadcast::Receiver<String>,
    cancel: CancellationToken,
) {
    if !authority.valid(&engine) {
        cancel.cancel();
        return;
    }
    while let Some(id) = while_live(&cancel, revocations.recv()).await {
        match id {
            Ok(id) if id == authority.login.device => break,
            Err(broadcast::error::RecvError::Lagged(_)) if !authority.valid(&engine) => break,
            Err(broadcast::error::RecvError::Closed) => break,
            _ => {}
        }
    }
    cancel.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::sync::{Semaphore, mpsc};
    use tokio::task::JoinSet;

    #[tokio::test]
    async fn revocation_cancels_full_queue_permit_and_request_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(
            tessera_core::config::Paths::at(dir.path()),
            Default::default(),
        )
        .await
        .unwrap();
        let (id, token) = engine.store().add_device("test").unwrap();
        let authority = Arc::new(Authority {
            login: engine.store().login(&token).unwrap().unwrap(),
            token,
        });
        let cancel = CancellationToken::new();
        let monitor = tokio::spawn(watch_revocations(
            engine.clone(),
            authority,
            engine.revocations(),
            cancel.clone(),
        ));
        let (tx, _rx) = mpsc::channel(1);
        tx.send(()).await.unwrap();
        let semaphore = Semaphore::new(0);
        let effects = Arc::new(AtomicUsize::new(0));
        let mut tasks = JoinSet::new();
        let c = cancel.clone();
        let e = effects.clone();
        tasks.spawn(async move {
            while_live(&c, async {
                std::future::pending::<()>().await;
                e.fetch_add(1, Ordering::Relaxed);
            })
            .await;
        });
        engine
            .handle(tessera_proto::Request::RevokeDevice { id }, None)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(while_live(&cancel, tx.send(())).await.is_none());
            assert!(while_live(&cancel, semaphore.acquire()).await.is_none());
            monitor.await.unwrap();
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        })
        .await
        .expect("revocation never waits for backpressure");
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn a_machine_cut_cancels_full_queue_and_permit_waits() {
        let cancel = CancellationToken::new();
        let cut = CancellationToken::new();
        let signal = cut.clone();
        let monitor = tokio::spawn(watch_cut(
            cancel.clone(),
            async move { cut.cancelled().await },
        ));
        let (tx, _rx) = mpsc::channel(1);
        tx.send(()).await.unwrap();
        let permits = Semaphore::new(0);
        signal.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(while_live(&cancel, tx.send(())).await.is_none());
            assert!(while_live(&cancel, permits.acquire()).await.is_none());
            monitor.await.unwrap();
        })
        .await
        .expect("a machine cut never waits for backpressure");
        let mut changed = false;
        assert!(
            while_live(&cancel, async { changed = true })
                .await
                .is_none()
        );
        assert!(!changed, "cancellation wins even over ready work");
    }

    #[tokio::test]
    async fn dispatch_rechecks_the_exact_login_without_a_revocation_event() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(
            tessera_core::config::Paths::at(dir.path()),
            Default::default(),
        )
        .await
        .unwrap();
        let (id, _) = engine.store().add_device("test").unwrap();
        let token = engine.store().machine_login(&id).unwrap().unwrap();
        let authority = Authority {
            login: engine.store().login(&token).unwrap().unwrap(),
            token,
        };
        let cancel = CancellationToken::new();
        authority.check(&engine, &cancel).unwrap();
        engine.store().machine_login(&id).unwrap();
        assert!(authority.check(&engine, &cancel).is_err());
        assert!(cancel.is_cancelled());
    }
}
