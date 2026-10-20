//! LSP base protocol: `Content-Length`-framed JSON-RPC messages.

use serde_json::Value;

/// Largest message accepted from a server (workspace symbol dumps of huge
/// monorepos stay well below this).
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

pub fn encode(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

#[derive(Debug, PartialEq)]
pub enum FrameError {
    /// A header block without `Content-Length` (skipped; framing resumes).
    MissingLength,
    /// Larger than [`MAX_MESSAGE`]; the stream can't be trusted any more.
    TooLarge(usize),
    /// The body wasn't JSON (skipped).
    BadJson(String),
}

/// Incremental decoder: feed it whatever the pipe returned, then pull
/// complete messages out. Tolerates stray output before a header (servers
/// that log to stdout) by resynchronising on the next header block.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next complete message, `Ok(None)` if more bytes are needed.
    pub fn next_message(&mut self) -> Result<Option<Value>, FrameError> {
        loop {
            let Some(end) = find(&self.buf, b"\r\n\r\n") else {
                // Guard against a peer that never sends a header terminator.
                if self.buf.len() > 64 * 1024 {
                    let keep = self.buf.split_off(self.buf.len() - 3);
                    self.buf = keep;
                }
                return Ok(None);
            };
            let header = String::from_utf8_lossy(&self.buf[..end]).to_string();
            let mut length = None;
            for line in header.split('\n') {
                if let Some((k, v)) = line.trim_end_matches('\r').split_once(':')
                    && k.trim().eq_ignore_ascii_case("content-length")
                {
                    length = v.trim().parse::<usize>().ok();
                }
            }
            let Some(len) = length else {
                // Garbage (or a header without length): drop it and resync.
                self.buf.drain(..end + 4);
                if self.buf.is_empty() {
                    return Err(FrameError::MissingLength);
                }
                continue;
            };
            if len > MAX_MESSAGE {
                return Err(FrameError::TooLarge(len));
            }
            let start = end + 4;
            if self.buf.len() < start + len {
                return Ok(None);
            }
            let body: Vec<u8> = self.buf.drain(..start + len).skip(start).collect();
            return match serde_json::from_slice(&body) {
                Ok(v) => Ok(Some(v)),
                Err(e) => Err(FrameError::BadJson(e.to_string())),
            };
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_split_across_reads_and_batched_in_one() {
        let a = encode(&json!({"jsonrpc": "2.0", "id": 1, "result": "héllo"}));
        let b = encode(&json!({"jsonrpc": "2.0", "method": "x", "params": {}}));
        let mut f = Framer::default();
        // One byte at a time: nothing until the message is complete.
        for (i, byte) in a.iter().enumerate() {
            f.push(&[*byte]);
            let got = f.next_message().unwrap();
            assert_eq!(got.is_some(), i == a.len() - 1, "byte {i}");
        }
        // Two messages (and part of a third) in one read.
        let c = encode(&json!({"id": 3}));
        let mut both = [a.clone(), b.clone()].concat();
        both.extend_from_slice(&c[..5]);
        f.push(&both);
        assert_eq!(f.next_message().unwrap().unwrap()["result"], "héllo");
        assert_eq!(f.next_message().unwrap().unwrap()["method"], "x");
        assert_eq!(f.next_message().unwrap(), None);
        f.push(&c[5..]);
        assert_eq!(f.next_message().unwrap().unwrap()["id"], 3);
    }

    #[test]
    fn extra_headers_junk_and_limits() {
        let mut f = Framer::default();
        let body = br#"{"id":7}"#;
        f.push(b"some log line\r\n\r\n");
        f.push(
            format!(
                "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        );
        f.push(body);
        assert_eq!(f.next_message().unwrap().unwrap()["id"], 7);
        f.push(b"Content-Length: 999999999999\r\n\r\n");
        assert!(matches!(f.next_message(), Err(FrameError::TooLarge(_))));
        let mut g = Framer::default();
        g.push(b"Content-Length: 3\r\n\r\nnot");
        assert!(matches!(g.next_message(), Err(FrameError::BadJson(_))));
    }
}
