//! Revision-bound authority for a file commit or process start.
//!
//! Lock order: session state -> ceiling gates. Actions under a gate may do
//! only filesystem work or spawn; they must never call the host, lock session
//! state, or await. Publication waits for a commit already in progress, while
//! queued work loses its old revision even if permissions are later restored.
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use parking_lot::Mutex;
use tessera_proto::PermissionMode;
use tokio_util::sync::CancellationToken;

pub(crate) struct PermissionCeiling {
    mode: AtomicU8,
    revision: Mutex<Revision>,
}

#[derive(Default)]
struct Revision {
    number: u64,
    changed: CancellationToken,
}

impl PermissionCeiling {
    pub(crate) fn new(mode: PermissionMode) -> Arc<Self> {
        Arc::new(Self {
            mode: AtomicU8::new(Self::encode(mode)),
            revision: Mutex::new(Revision::default()),
        })
    }

    fn encode(mode: PermissionMode) -> u8 {
        match mode {
            PermissionMode::ReadOnly => 0,
            PermissionMode::Ask => 1,
            PermissionMode::Default => 2,
            PermissionMode::Auto => 3,
        }
    }

    fn publish(&self, mode: u8) {
        let mut revision = self.revision.lock();
        let old = self.mode.load(Ordering::Acquire);
        if old == 4 || old == mode {
            return;
        }
        // The gate ensures no commit using the old authority can finish
        // after this publication. Cancellation also wakes pending setup.
        self.mode.store(mode, Ordering::Release);
        revision.changed.cancel();
        revision.number = revision.number.wrapping_add(1);
        revision.changed = CancellationToken::new();
    }

    pub(crate) fn store(&self, mode: PermissionMode) {
        self.publish(Self::encode(mode));
    }
    pub(crate) fn revoke(&self) {
        self.publish(4);
    }
    pub(crate) fn load(&self) -> PermissionMode {
        match self.mode.load(Ordering::Acquire) {
            1 => PermissionMode::Ask,
            2 => PermissionMode::Default,
            3 => PermissionMode::Auto,
            _ => PermissionMode::ReadOnly,
        }
    }
}

/// Captured before work can wait. An empty admission is reserved for
/// host-owned cleanup and standalone tests, never engine-dispatched actions.
#[derive(Clone, Default)]
pub struct ActionAdmission {
    ceilings: Vec<(Arc<PermissionCeiling>, u64, CancellationToken)>,
    denied: bool,
    cancel: Option<CancellationToken>,
}

impl ActionAdmission {
    pub(crate) fn denied() -> Self {
        Self {
            denied: true,
            ..Self::default()
        }
    }

    pub(crate) fn capture(
        mut ceilings: Vec<Arc<PermissionCeiling>>,
        cancel: Option<CancellationToken>,
    ) -> Self {
        // Both parent and child publication take just one gate. Multiple
        // action gates always follow the same order, including across rooms.
        ceilings.sort_by_key(|c| Arc::as_ptr(c) as usize);
        ceilings.dedup_by(|a, b| Arc::ptr_eq(a, b));
        Self {
            ceilings: ceilings
                .into_iter()
                .map(|c| {
                    let r = c.revision.lock();
                    let (number, changed) = (r.number, r.changed.clone());
                    drop(r);
                    (c, number, changed)
                })
                .collect(),
            denied: false,
            cancel,
        }
    }

    pub(crate) fn run<T>(&self, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let guards: Vec<_> = self
            .ceilings
            .iter()
            .map(|(c, _, _)| c.revision.lock())
            .collect();
        if self.denied
            || self
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            || self
                .ceilings
                .iter()
                .zip(&guards)
                .any(|((c, number, _), r)| {
                    r.number != *number || c.load() == PermissionMode::ReadOnly
                })
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The chat's permissions changed or it stopped; this action was not committed. Submit new work under the current mode.",
            ));
        }
        action()
    }

    /// [`run`](Self::run) for an action that changes nothing (a read-only
    /// helper's command, confined or limited to a list): every mode allows
    /// it, read-only included, but not once the chat stopped or was removed.
    pub(crate) fn run_reading<T>(&self, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let _guards: Vec<_> = self
            .ceilings
            .iter()
            .map(|(c, _, _)| c.revision.lock())
            .collect();
        if self.denied {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Not started: nothing may run for this actor here (it has no live parent chat, or acts read-only through Claude Code).",
            ));
        }
        if self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
            || self
                .ceilings
                .iter()
                .any(|(c, _, _)| c.mode.load(Ordering::Acquire) == 4)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The chat stopped; this command was not started.",
            ));
        }
        action()
    }

    pub(crate) fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        self.run(|| Ok(())).map_err(|e| e.to_string())
    }

    pub(crate) async fn changed(&self) {
        let mut waits: Vec<_> = self
            .ceilings
            .iter()
            .map(|(_, _, c)| c.cancelled())
            .collect();
        if let Some(c) = &self.cancel {
            waits.push(c.cancelled());
        }
        if self.denied {
            return;
        }
        if waits.is_empty() {
            std::future::pending::<()>().await;
        } else {
            futures::future::select_all(waits.into_iter().map(Box::pin)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_actions_run_in_read_only_but_not_after_a_stop() {
        let ceiling = PermissionCeiling::new(PermissionMode::ReadOnly);
        let admission = ActionAdmission::capture(vec![ceiling.clone()], None);
        assert!(admission.run(|| Ok(())).is_err());
        assert_eq!(admission.run_reading(|| Ok(7)).unwrap(), 7);
        ceiling.store(PermissionMode::Auto);
        assert!(admission.run_reading(|| Ok(())).is_ok());
        ceiling.revoke();
        assert!(
            admission
                .run_reading::<()>(|| panic!("ran after a stop"))
                .is_err()
        );
        let cancel = CancellationToken::new();
        let stopped = ActionAdmission::capture(vec![], Some(cancel.clone()));
        cancel.cancel();
        assert!(stopped.run_reading(|| Ok(())).is_err());
        assert!(ActionAdmission::denied().run_reading(|| Ok(())).is_err());
    }

    #[test]
    fn downgrade_and_upgrade_do_not_revive_a_queued_action() {
        let ceiling = PermissionCeiling::new(PermissionMode::Auto);
        let old = ActionAdmission::capture(vec![ceiling.clone()], None);
        ceiling.store(PermissionMode::ReadOnly);
        ceiling.store(PermissionMode::Auto);
        assert_eq!(
            old.run::<()>(|| panic!("stale action ran"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(
            ActionAdmission::capture(vec![ceiling], None)
                .check()
                .is_ok()
        );
    }

    #[test]
    fn publication_waits_for_a_commit_but_invalidates_later_commits() {
        let ceiling = PermissionCeiling::new(PermissionMode::Auto);
        let admission = ActionAdmission::capture(vec![ceiling.clone()], None);
        let (entered, reached) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let a = admission.clone();
        let writer = std::thread::spawn(move || {
            a.run(|| {
                entered.send(()).unwrap();
                released
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                Ok(())
            })
            .unwrap()
        });
        reached
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (published, done) = std::sync::mpsc::channel();
        let publisher = std::thread::spawn(move || {
            ceiling.store(PermissionMode::ReadOnly);
            published.send(()).unwrap();
        });
        assert!(
            done.recv_timeout(std::time::Duration::from_millis(30))
                .is_err()
        );
        release.send(()).unwrap();
        writer.join().unwrap();
        done.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        publisher.join().unwrap();
        assert!(
            admission
                .run::<()>(|| panic!("commit after publication"))
                .is_err()
        );
    }

    #[test]
    fn queued_process_start_is_refused_without_creating_its_marker() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ceiling = PermissionCeiling::new(PermissionMode::Auto);
        let admission = ActionAdmission::capture(vec![ceiling.clone()], None);
        let mut cmd = crate::tools::bash::shell_command("echo fixture > marker", None);
        cmd.current_dir(dir.path());
        ceiling.store(PermissionMode::ReadOnly);
        runtime.block_on(async {
            assert!(admission.run(|| cmd.spawn()).is_err());
        });
        assert!(!dir.path().join("marker").exists());
    }
}
