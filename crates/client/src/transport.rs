//! Track bytes accepted by the socket, below TLS and WebSocket buffering.
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Instant;

const WRITE_IDLE: Duration = Duration::from_secs(40);

#[derive(Clone)]
pub(crate) struct WriteProgress(Arc<Mutex<Instant>>);

impl WriteProgress {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    pub(crate) fn begin(&self) {
        *self.0.lock() = Instant::now();
    }

    pub(crate) async fn stalled(&self) {
        loop {
            let deadline = *self.0.lock() + WRITE_IDLE;
            tokio::time::sleep_until(deadline).await;
            // Writes may have advanced while this timer was asleep. Recheck
            // the actual socket stamp, not how often the send was polled.
            if Instant::now() >= *self.0.lock() + WRITE_IDLE {
                return;
            }
        }
    }

    fn wrote(&self, result: &Poll<io::Result<usize>>) {
        if matches!(result, Poll::Ready(Ok(n)) if *n > 0) {
            self.begin();
        }
    }
}

pub(crate) struct ProgressIo<S> {
    inner: S,
    progress: WriteProgress,
}

impl<S> ProgressIo<S> {
    pub(crate) fn new(inner: S, progress: WriteProgress) -> Self {
        Self { inner, progress }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ProgressIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ProgressIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let result = Pin::new(&mut me.inner).poll_write(cx, buf);
        me.progress.wrote(&result);
        result
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let result = Pin::new(&mut me.inner).poll_write_vectored(cx, bufs);
        me.progress.wrote(&result);
        result
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
