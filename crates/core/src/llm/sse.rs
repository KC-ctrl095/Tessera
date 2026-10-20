//! Minimal Server-Sent Events reader over a reqwest response.

use futures::StreamExt;

use super::{ErrorKind, LlmError};

pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

pub struct SseReader {
    stream: futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    buf: Vec<u8>,
    idle: std::time::Duration,
}

impl SseReader {
    pub fn new(resp: reqwest::Response) -> Self {
        Self {
            stream: resp.bytes_stream().boxed(),
            buf: Vec::new(),
            idle: std::time::Duration::from_secs(300),
        }
    }

    /// Next event, `Ok(None)` at end of stream.
    pub async fn next(&mut self) -> Result<Option<SseEvent>, LlmError> {
        loop {
            if let Some(ev) = self.take_event() {
                return Ok(Some(ev));
            }
            let chunk = match tokio::time::timeout(self.idle, self.stream.next()).await {
                Err(_) => {
                    return Err(LlmError::new(
                        ErrorKind::Transient,
                        "stream stalled: no data for 5 minutes",
                    ));
                }
                Ok(None) => {
                    // Flush a trailing event without a blank line.
                    if !self.buf.iter().all(u8::is_ascii_whitespace) {
                        self.buf.extend_from_slice(b"\n\n");
                        return Ok(self.take_event());
                    }
                    return Ok(None);
                }
                Ok(Some(Err(e))) => return Err(LlmError::network(e)),
                Ok(Some(Ok(c))) => c,
            };
            self.buf.extend_from_slice(&chunk);
        }
    }

    fn take_event(&mut self) -> Option<SseEvent> {
        loop {
            let (end, sep) = find_blank_line(&self.buf)?;
            let block: Vec<u8> = self.buf.drain(..end + sep).collect();
            let text = String::from_utf8_lossy(&block[..end]);
            let mut event = None;
            let mut data = String::new();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(v.strip_prefix(' ').unwrap_or(v));
                } else if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_string());
                }
            }
            if !data.is_empty() || event.is_some() {
                return Some(SseEvent { event, data });
            }
        }
    }
}

fn find_blank_line(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if buf[i] == b'\r' && i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}
