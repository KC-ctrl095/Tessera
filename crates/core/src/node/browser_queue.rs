//! Bounded CDP memory including messages already being written/sent.
//! Fail closed rather than waiting inside a node/Stop lock. Byte leases
//! follow each message until the blocked pipe/link operation finishes.
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

pub(super) struct Item<T> {
    pub value: T,
    pub lease: OwnedSemaphorePermit,
}
pub(super) struct Sender<T> {
    tx: mpsc::Sender<Item<T>>,
    bytes: Arc<Semaphore>,
    stop: CancellationToken,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            bytes: self.bytes.clone(),
            stop: self.stop.clone(),
        }
    }
}
pub(super) type Receiver<T> = mpsc::Receiver<Item<T>>;
pub(super) fn channel<T>(budget: usize, stop: CancellationToken) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel(128);
    (
        Sender {
            tx,
            bytes: Arc::new(Semaphore::new(budget)),
            stop,
        },
        rx,
    )
}
pub(super) trait Size {
    fn bytes(&self) -> usize;
}
impl Size for Vec<u8> {
    fn bytes(&self) -> usize {
        self.len()
    }
}
impl Size for tessera_proto::NodeBrowserUp {
    fn bytes(&self) -> usize {
        match self {
            Self::Cdp { messages, .. } => messages.iter().map(|m| m.len() + 32).sum(),
            _ => serde_json::to_vec(self).map_or(4096, |v| v.len()),
        }
    }
}
impl<T: Size> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), ()> {
        let bytes = value.bytes();
        if self.stop.is_cancelled() {
            return Err(());
        }
        let fail = || {
            self.stop.cancel();
        };
        let Some(bytes) = bytes
            .max(1)
            .checked_add(128)
            .and_then(|n| u32::try_from(n).ok())
        else {
            fail();
            return Err(());
        };
        let Ok(lease) = self.bytes.clone().try_acquire_many_owned(bytes) else {
            fail();
            return Err(());
        };
        if self.tx.try_send(Item { value, lease }).is_err() {
            fail();
            return Err(());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn flood_on_a_blocked_link_is_bounded_and_stop_is_immediate() {
        let stop = CancellationToken::new();
        let (tx, mut rx) = channel(4096, stop.clone());
        tx.send(vec![0; 1024]).unwrap();
        let in_flight = rx.recv().await.unwrap();
        tx.send(vec![0; 1024]).unwrap();
        tx.send(vec![0; 1024]).unwrap();
        assert!(tx.send(vec![0; 1024]).is_err());
        tokio::time::timeout(std::time::Duration::from_millis(50), stop.cancelled())
            .await
            .unwrap();
        assert!(tx.bytes.available_permits() < 1024);
        drop(in_flight);
        drop(rx);
        assert_eq!(tx.bytes.available_permits(), 4096);
    }
    #[test]
    fn item_count_and_downlink_bytes_also_fail_closed() {
        let stop = CancellationToken::new();
        let (tx, _rx) = channel(1 << 20, stop.clone());
        for _ in 0..128 {
            tx.send(vec![0]).unwrap();
        }
        assert!(tx.send(vec![0]).is_err());
        assert!(stop.is_cancelled());
        let stop = CancellationToken::new();
        let (tx, _rx) = channel(1024, stop.clone());
        assert!(tx.send(vec![0; 2048]).is_err());
        assert!(stop.is_cancelled());
    }
}
