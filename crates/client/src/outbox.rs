//! Bounded client-owned admission, including disconnected and active work.
use super::{ApiError, ErrorCode, Request, Waiters, tokio_util_lite};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

// The same phone-safe defaults on every shell; not user-tunable transport knobs.
pub(crate) const MAX_REQUESTS: usize = 128;
pub(crate) const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = tessera_proto::MAX_SERIALIZED_REQUEST_BYTES;

pub(crate) struct Lease {
    _count: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
    pub(crate) cancelled: tokio_util_lite::Stop,
}

pub(crate) struct Payload {
    pub(crate) id: u64,
    pub(crate) text: String,
    pub(crate) expires: Option<Instant>,
    pub(crate) epoch: Option<u64>,
    pub(crate) lease: Arc<Lease>,
}

impl Payload {
    pub(crate) fn live_in_epoch(&self, waiters: &Waiters, epoch: u64) -> bool {
        self.epoch.is_none_or(|bound| bound == epoch) && self.live(waiters)
    }

    #[cfg(test)]
    pub(crate) fn into_message_in_epoch(
        self,
        waiters: &Waiters,
        epoch: u64,
    ) -> Option<super::ClientMessage> {
        self.epoch
            .is_none_or(|bound| bound == epoch)
            .then(|| self.into_message(waiters))
            .flatten()
    }
    pub(crate) fn live(&self, waiters: &Waiters) -> bool {
        !self.lease.cancelled.stopped()
            && self.expires.is_none_or(|deadline| {
                Instant::now() < deadline
                    && waiters
                        .lock()
                        .get(&self.id)
                        .is_some_and(|tx| !tx.is_closed())
            })
    }

    #[cfg(test)]
    pub(crate) fn into_message(self, waiters: &Waiters) -> Option<super::ClientMessage> {
        self.live(waiters)
            .then(|| serde_json::from_str(&self.text).unwrap())
    }
}

#[derive(Serialize)]
struct Message<'a> {
    id: u64,
    #[serde(flatten)]
    request: &'a Request,
}

struct Counter(usize);
impl io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(buf.len())
            .filter(|n| *n <= MAX_REQUEST_BYTES)
            .ok_or_else(|| io::Error::other("request exceeds the serialized server limit"))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn size(id: u64, request: &Request) -> Result<usize, ApiError> {
    if let Request::Send { attachments, .. } = request {
        let mut total = 0usize;
        for attachment in attachments {
            let text = attachment.data.trim();
            // Valid standard base64 has 4 symbols per 3 raw bytes. The server
            // remains responsible for validating malformed encodings.
            let padding = text
                .bytes()
                .rev()
                .take_while(|c| *c == b'=')
                .take(2)
                .count();
            let raw = (text.len() / 4 * 3).saturating_sub(padding);
            total = total.saturating_add(raw);
            if raw > tessera_proto::MAX_FILE_ATTACHMENT_BYTES
                || total > tessera_proto::MAX_MESSAGE_ATTACHMENT_BYTES
            {
                return Err(ApiError::new(
                    ErrorCode::BadRequest,
                    "attachments exceed the server limit (25 MiB per file, 50 MiB per message)",
                ));
            }
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, &Message { id, request }).map_err(|_| {
        ApiError::new(
            ErrorCode::BadRequest,
            "request exceeds the serialized server limit (50 MiB attachments plus base64/JSON)",
        )
    })?;
    Ok(counter.0)
}

pub(crate) struct Outbox {
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    large: Arc<Semaphore>,
    byte_budget: usize,
    queue: Mutex<VecDeque<Payload>>,
    ready: Notify,
    closed: AtomicBool,
    #[cfg(test)]
    limits: (usize, usize),
}

impl Default for Outbox {
    fn default() -> Self {
        Self::with_limits(MAX_REQUESTS, MAX_BYTES)
    }
}

impl Outbox {
    pub(crate) fn with_limits(count: usize, bytes: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(count)),
            bytes: Arc::new(Semaphore::new(bytes)),
            large: Arc::new(Semaphore::new(1)),
            byte_budget: bytes,
            queue: Mutex::default(),
            ready: Notify::new(),
            closed: AtomicBool::new(false),
            #[cfg(test)]
            limits: (count, bytes),
        }
    }

    pub(crate) async fn admit(
        &self,
        id: u64,
        req: Request,
        expires: Instant,
    ) -> Result<Payload, ApiError> {
        // Measure without retaining a second payload while waiting for space.
        let n = size(id, &req)?;
        // Always reserve the byte/large lease before a shared count slot.
        // Waiting large producers must not consume ordinary admission slots.
        let (pool, permits) = self.byte_pool(n);
        let bytes = pool
            .acquire_many_owned(permits)
            .await
            .map_err(|_| stopped())?;
        let count = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| stopped())?;
        self.encode(id, req, Some(expires), n, count, bytes)
    }

    pub(crate) fn post(&self, id: u64, req: Request) -> Result<(), ApiError> {
        self.post_in_epoch(id, req, None)
    }

    pub(crate) fn post_in_epoch(
        &self,
        id: u64,
        req: Request,
        epoch: Option<u64>,
    ) -> Result<(), ApiError> {
        let n = size(id, &req)?;
        let bytes = self.try_bytes(n)?;
        let count = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| self.overflow())?;
        let mut payload = self.encode(id, req, None, n, count, bytes)?;
        payload.epoch = epoch;
        self.push(payload)
    }

    fn byte_pool(&self, n: usize) -> (Arc<Semaphore>, u32) {
        if n > self.byte_budget {
            (self.large.clone(), 1)
        } else {
            (self.bytes.clone(), n as u32)
        }
    }

    fn try_bytes(&self, n: usize) -> Result<OwnedSemaphorePermit, ApiError> {
        let (pool, permits) = self.byte_pool(n);
        pool.try_acquire_many_owned(permits)
            .map_err(|_| self.overflow())
    }
    fn encode(
        &self,
        id: u64,
        req: Request,
        expires: Option<Instant>,
        n: usize,
        count: OwnedSemaphorePermit,
        bytes: OwnedSemaphorePermit,
    ) -> Result<Payload, ApiError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(stopped());
        }
        let mut data = Vec::with_capacity(n);
        serde_json::to_writer(&mut data, &Message { id, request: &req })
            .map_err(|_| ApiError::new(ErrorCode::BadRequest, "couldn't encode the request"))?;
        debug_assert_eq!(data.len(), n);
        Ok(Payload {
            id,
            text: String::from_utf8(data).expect("JSON is UTF-8"),
            expires,
            epoch: None,
            lease: Arc::new(Lease {
                _count: count,
                _bytes: bytes,
                cancelled: tokio_util_lite::Stop::default(),
            }),
        })
    }

    pub(crate) fn push(&self, payload: Payload) -> Result<(), ApiError> {
        let mut queue = self.queue.lock();
        if self.closed.load(Ordering::Acquire) {
            return Err(stopped());
        }
        queue.push_back(payload);
        drop(queue);
        self.ready.notify_one();
        Ok(())
    }

    pub(crate) async fn recv(&self) -> Option<Payload> {
        loop {
            let ready = self.ready.notified();
            if let Some(payload) = self.queue.lock().pop_front() {
                return Some(payload);
            }
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            ready.await;
        }
    }

    // Called by the RPC drop guard, not by the writer: cancellation frees
    // queued JSON and permits even while another request's write is blocked.
    pub(crate) fn cancel(&self, id: u64) {
        self.queue.lock().retain(|item| item.id != id);
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.queue.lock().clear();
        self.slots.close();
        self.bytes.close();
        self.large.close();
        self.ready.notify_waiters();
    }

    fn overflow(&self) -> ApiError {
        if self.closed.load(Ordering::Acquire) {
            stopped()
        } else {
            ApiError::new(
                ErrorCode::Busy,
                "client request queue is full (128 requests, 16 MiB, or one large request); the request was not queued",
            )
        }
    }

    #[cfg(test)]
    pub(crate) fn send(&self, queued: super::QueuedRequest) -> Result<(), ApiError> {
        let n = size(queued.id, &queued.request)?;
        let bytes = self.try_bytes(n)?;
        let count = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| self.overflow())?;
        let mut payload =
            self.encode(queued.id, queued.request, queued.expires, n, count, bytes)?;
        payload.epoch = queued.epoch;
        self.push(payload)
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> (usize, usize, usize) {
        (
            self.limits.0 - self.slots.available_permits(),
            self.limits.1 - self.bytes.available_permits(),
            self.queue.lock().len(),
        )
    }
    #[cfg(test)]
    pub(crate) fn large_in_use(&self) -> bool {
        self.large.available_permits() == 0
    }
}

fn stopped() -> ApiError {
    ApiError::new(ErrorCode::Internal, "client stopped")
}
