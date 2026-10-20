//! All native login writers use the same sequence, from before requesting
//! a login through saving, retirement and connection handoff. The factory
//! starts only after ownership is acquired; no config mutex spans an await.

use std::future::Future;

#[derive(Default)]
pub(crate) struct CredentialWrites(tokio::sync::Mutex<()>);

impl CredentialWrites {
    pub(crate) async fn run<T, F: Future<Output = T>>(&self, begin: impl FnOnce() -> F) -> T {
        let _guard = self.0.lock().await;
        begin().await
    }
}
