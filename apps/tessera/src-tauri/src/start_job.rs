//! "Start Tessera here" runs as one job that carries on by itself. The
//! setup screen's "Stop waiting" only stops watching it, and pressing Start
//! again while it runs waits for that same job: never a second install, a
//! second launcher or two keys issued at once. A job ends by itself within
//! its own time limits.

use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::watch;

/// At most one job at a time; its result is there for whoever waits.
pub struct OneJob<T> {
    running: Mutex<Option<watch::Receiver<Option<T>>>>,
}

impl<T: Clone + Send + Sync + 'static> OneJob<T> {
    pub const fn new() -> Self {
        Self {
            running: Mutex::new(None),
        }
    }

    /// The job in progress (true), or the one `begin` makes, started now
    /// (false). A job that ended, or died without a result, isn't joined.
    pub fn join_or_begin<F>(&self, begin: impl FnOnce() -> F) -> (watch::Receiver<Option<T>>, bool)
    where
        F: Future<Output = T> + Send + 'static,
    {
        let mut slot = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rx) = slot
            .as_ref()
            .filter(|rx| rx.borrow().is_none() && rx.has_changed().is_ok())
        {
            return (rx.clone(), true);
        }
        let (tx, rx) = watch::channel(None);
        let job = begin();
        tokio::spawn(async move {
            let out = job.await;
            let _ = tx.send(Some(out));
        });
        *slot = Some(rx.clone());
        (rx, false)
    }
}

impl<T: Clone + Send + Sync + 'static> Default for OneJob<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub enum Waited<T> {
    Done(T),
    /// "Stop waiting": the job carries on.
    Stopped,
    /// The job died without a result.
    Lost,
}

/// The job's result, unless `stop` comes first.
pub async fn wait<T: Clone>(
    rx: &mut watch::Receiver<Option<T>>,
    stop: impl Future<Output = ()>,
) -> Waited<T> {
    tokio::select! {
        r = rx.wait_for(Option::is_some) => match r.as_deref() {
            Ok(Some(t)) => Waited::Done(t.clone()),
            _ => Waited::Lost,
        },
        () = stop => Waited::Stopped,
    }
}

/// "Stop waiting" for the attempts begun until then. An attempt is the time
/// it began (milliseconds since 1970, as the setup screen counts), so a stop
/// pressed before its start even arrived here still counts.
pub struct Stops(AtomicU64);

impl Stops {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn stop(&self, attempt: u64) {
        self.0.fetch_max(attempt, Ordering::SeqCst);
    }

    pub fn stopped(&self, attempt: u64) -> bool {
        self.0.load(Ordering::SeqCst) >= attempt
    }

    /// Ready once `attempt` is stopped.
    pub async fn until_stopped(&self, attempt: u64) {
        while !self.stopped(attempt) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

impl Default for Stops {
    fn default() -> Self {
        Self::new()
    }
}

/// Now, as an attempt.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// A job that takes a while (a server slow to answer), counting how
    /// often one was begun.
    fn slow(
        begun: &Arc<AtomicUsize>,
        took: Duration,
        out: u32,
    ) -> impl FnOnce() -> std::pin::Pin<Box<dyn Future<Output = u32> + Send>> {
        let begun = begun.clone();
        move || {
            begun.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(took).await;
                out
            })
        }
    }

    #[tokio::test]
    async fn stop_then_start_again_at_once_waits_for_the_same_job() {
        let job = OneJob::new();
        let stops = Stops::new();
        let begun = Arc::new(AtomicUsize::new(0));

        let first = 1_000;
        let (mut rx, joined) = job.join_or_begin(slow(&begun, Duration::from_millis(800), 7));
        assert!(!joined);
        let stop_soon = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stops.stop(first);
        };
        let (waited, ()) = tokio::join!(wait(&mut rx, stops.until_stopped(first)), stop_soon);
        assert!(matches!(waited, Waited::Stopped));

        // Start again right away: the same job, not a second one.
        let second = 2_000;
        let (mut rx, joined) = job.join_or_begin(slow(&begun, Duration::ZERO, 8));
        assert!(joined);
        let waited = wait(&mut rx, stops.until_stopped(second)).await;
        assert!(matches!(waited, Waited::Done(7)));
        assert_eq!(begun.load(Ordering::SeqCst), 1);

        // Once it ended, the next start begins anew.
        let (mut rx, joined) = job.join_or_begin(slow(&begun, Duration::ZERO, 9));
        assert!(!joined);
        assert!(matches!(
            wait(&mut rx, std::future::pending()).await,
            Waited::Done(9)
        ));
        assert_eq!(begun.load(Ordering::SeqCst), 2);
    }

    fn fails() -> u32 {
        panic!("the job failed (on purpose)")
    }

    #[tokio::test]
    async fn a_job_that_dies_isnt_waited_for_or_joined() {
        let job = OneJob::<u32>::new();
        let (mut rx, _) = job.join_or_begin(|| async { fails() });
        assert!(matches!(
            wait(&mut rx, std::future::pending()).await,
            Waited::Lost
        ));
        let (mut rx, joined) = job.join_or_begin(|| async { 3 });
        assert!(!joined);
        assert!(matches!(
            wait(&mut rx, std::future::pending()).await,
            Waited::Done(3)
        ));
    }

    #[test]
    fn a_stop_covers_the_attempts_begun_until_then() {
        let stops = Stops::new();
        assert!(!stops.stopped(1));
        stops.stop(100);
        assert!(stops.stopped(99));
        assert!(stops.stopped(100));
        assert!(!stops.stopped(101));
        // An older stop arriving late changes nothing.
        stops.stop(50);
        assert!(!stops.stopped(101));
    }
}
