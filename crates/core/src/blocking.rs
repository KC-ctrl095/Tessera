//! Synchronous store callers keep their API, but a multi-thread Tokio
//! worker hands its scheduler core to a replacement before waiting on
//! SQLite or its mutex. This covers writes from agent/Claude/scheduler
//! paths too, not just requests moved to spawn_blocking. On a current-thread
//! runtime callers must use spawn_blocking (there is no core to hand off).

use std::cell::Cell;
use tokio::runtime::{Handle, RuntimeFlavor};

thread_local! {
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn needed() -> bool {
    !INSIDE.with(Cell::get)
        && Handle::try_current().is_ok_and(|h| h.runtime_flavor() == RuntimeFlavor::MultiThread)
}

pub(crate) fn run<T>(f: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(|| {
        struct Reset(bool);
        impl Drop for Reset {
            fn drop(&mut self) {
                INSIDE.with(|c| c.set(self.0));
            }
        }
        let _reset = Reset(INSIDE.with(|c| c.replace(true)));
        f()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_calls_and_panics_restore_the_handoff_flag() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(needed());
            let failed = std::panic::catch_unwind(|| {
                run(|| {
                    assert!(!needed());
                    panic!("synthetic failure");
                })
            });
            assert!(failed.is_err());
            assert!(needed());
            run(|| assert!(!needed()));
        });
        assert!(!needed());
    }
    #[tokio::test]
    async fn a_current_thread_runtime_does_not_attempt_a_handoff() {
        assert!(!needed());
    }
}
