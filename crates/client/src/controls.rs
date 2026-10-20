//! Coalesced control work, serviced between complete data frames.
use super::{WriteProgress, send_frame, tokio_util_lite};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Bytes, Message};

#[derive(Default)]
struct Pending {
    ping: bool,
    probe: bool,
    pong: Option<Bytes>,
}

pub(crate) struct Controls {
    pending: Mutex<Pending>,
    pub(crate) ready: Notify,
    flushed: watch::Sender<Option<(Instant, u64)>>,
    reads: AtomicU64,
}

impl Controls {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::default(),
            ready: Notify::new(),
            flushed: watch::channel(None).0,
            reads: AtomicU64::new(0),
        }
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<(Instant, u64)>> {
        self.flushed.subscribe()
    }
    pub(crate) fn observed_input(&self) {
        self.reads.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn read_generation(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
    pub(crate) fn ping(&self, probe: bool) {
        let mut pending = self.pending.lock();
        pending.ping = true;
        pending.probe |= probe;
        drop(pending);
        self.ready.notify_one();
    }
    pub(crate) fn pong(&self, bytes: Bytes) {
        // RFC 6455 permits answering only the most recent pending Ping.
        self.pending.lock().pong = Some(bytes);
        self.ready.notify_one();
    }
    pub(crate) async fn flush<S>(
        &self,
        sink: &mut S,
        stop: &tokio_util_lite::Stop,
        progress: &WriteProgress,
    ) -> bool
    where
        S: futures::Sink<Message> + Unpin,
    {
        let pending = std::mem::take(&mut *self.pending.lock());
        if let Some(bytes) = pending.pong
            && !send_frame(sink, Message::Pong(bytes), stop, progress).await
        {
            return false;
        }
        if pending.ping {
            // A response may arrive while poll_flush is still pending.
            let generation = self.read_generation();
            if !send_frame(sink, Message::Ping(Vec::new().into()), stop, progress).await {
                return false;
            }
            if pending.probe {
                // SinkExt::send includes the flush, not merely admission to its buffer.
                self.flushed
                    .send_replace(Some((Instant::now(), generation)));
            }
        }
        true
    }
}
