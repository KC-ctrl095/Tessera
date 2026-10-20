//! Redaction at text and structured output boundaries.

use serde_json::Value;

/// JSON has no opaque binary fields. Object keys are text too.
pub fn value(value: &mut Value, secrets: &[String], patterns: bool) {
    let clean = |text: &str| {
        if patterns {
            super::redact(text, secrets)
        } else {
            super::redact_known(text, secrets)
        }
    };
    match value {
        Value::String(text) => *text = clean(text),
        Value::Array(items) => {
            for item in items {
                self::value(item, secrets, patterns);
            }
        }
        Value::Object(items) => {
            for (key, mut item) in std::mem::take(items) {
                self::value(&mut item, secrets, patterns);
                items.insert(clean(&key), item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// One ordered byte stream. Nothing is published or truncated until a possible
/// secret suffix and the UTF-8 decoder's remainder have been held back. Short
/// secrets also need their following character and the preceding token context.
pub struct Stream {
    secrets: Vec<String>,
    pending: String,
    utf8: Vec<u8>,
    hold: usize,
    previous: Option<char>,
    terminal: Option<u8>,
}

impl Stream {
    pub fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|s| !s.is_empty());
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        let hold = secrets.first().map_or(0, |s| s.len().saturating_add(4));
        Self {
            secrets,
            pending: String::new(),
            utf8: Vec::new(),
            hold,
            previous: None,
            terminal: None,
        }
    }

    /// Strip terminal escape sequences statefully before comparing values.
    pub fn terminal(secrets: Vec<String>) -> Self {
        let mut stream = Self::new(secrets);
        stream.terminal = Some(0);
        stream
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        if let Some(state) = self.terminal.as_mut() {
            for &byte in bytes {
                match *state {
                    0 if byte == 0x1b => *state = 1,
                    0 => self.utf8.push(byte),
                    1 => {
                        *state = match byte {
                            b'[' => 2,
                            b']' => 3,
                            b'(' | b')' => 5,
                            _ => 0,
                        }
                    }
                    2 if (0x40..=0x7e).contains(&byte) => *state = 0,
                    3 if byte == 7 => *state = 0,
                    3 if byte == 0x1b => *state = 4,
                    4 if byte == b'\\' || byte == 7 => *state = 0,
                    4 => *state = 3,
                    5 => *state = 0,
                    _ => {}
                }
            }
        } else {
            self.utf8.extend_from_slice(bytes);
        }
        decode_lossy(&mut self.utf8, &mut self.pending, false);
        self.drain(false)
    }

    pub fn finish(&mut self) -> String {
        decode_lossy(&mut self.utf8, &mut self.pending, true);
        self.drain(true)
    }

    fn drain(&mut self, end: bool) -> String {
        let text = self.pending.as_str();
        let limit = if end {
            text.len()
        } else {
            text.len().saturating_sub(self.hold)
        };
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let mut out = String::new();
        let mut at = 0;
        while at < limit {
            let matched = self.secrets.iter().find(|s| {
                if !text[at..].starts_with(s.as_str()) {
                    return false;
                }
                if s.chars().count() >= super::WHOLE_TOKEN_BELOW {
                    return true;
                }
                !(s.chars().next().is_some_and(word) && self.previous.is_some_and(word)
                    || s.chars().last().is_some_and(word)
                        && text[at + s.len()..].chars().next().is_some_and(word))
            });
            if let Some(secret) = matched {
                out.push_str("[REDACTED]");
                self.previous = secret.chars().last();
                at += secret.len();
            } else {
                let c = text[at..].chars().next().unwrap();
                out.push(c);
                self.previous = Some(c);
                at += c.len_utf8();
            }
        }
        self.pending.drain(..at);
        out
    }
}

fn decode_lossy(bytes: &mut Vec<u8>, text: &mut String, end: bool) {
    loop {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                text.push_str(valid);
                bytes.clear();
                return;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                text.push_str(std::str::from_utf8(&bytes[..valid]).unwrap());
                bytes.drain(..valid);
                if let Some(invalid) = error.error_len() {
                    text.push('\u{fffd}');
                    bytes.drain(..invalid);
                } else {
                    if end {
                        text.push_str(&String::from_utf8_lossy(bytes));
                        bytes.clear();
                    }
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streams_without_secrets_publish_short_progress_without_waiting_for_exit() {
        for secrets in [vec![], vec![String::new()]] {
            let mut stream = Stream::terminal(secrets);
            assert_eq!(stream.push(b"ready\n"), "ready\n");
            assert_eq!(stream.push(b"started\n"), "started\n");
            assert_eq!(stream.push(b"\x1b[31mnext\x1b[0m\n"), "next\n");
            assert!(stream.push(&[0xc3]).is_empty());
            assert_eq!(stream.push(&[0xa9]), "\u{e9}");
            assert!(stream.finish().is_empty());
        }
    }
    #[test]
    fn terminal_sequences_are_removed_before_redaction_at_every_split() {
        let input = b"before fixture-\x1b[31mvalue\x1b[0m-123456 after\x1b]0;title\x07";
        for split in 0..=input.len() {
            let mut stream = Stream::terminal(vec!["fixture-value-123456".into()]);
            let mut out = stream.push(&input[..split]);
            out.push_str(&stream.push(&input[split..]));
            out.push_str(&stream.finish());
            assert_eq!(out, "before [REDACTED] after");
        }
    }
    #[test]
    fn invalid_bytes_do_not_discard_a_following_split_codepoint() {
        let secret = "caf\u{e9}-\u{1f33b}-value";
        let input = [
            b"\xff before ".as_slice(),
            secret.as_bytes(),
            b" after\xf0\x9f",
        ]
        .concat();
        for split in 0..=input.len() {
            let mut stream = Stream::new(vec![secret.into()]);
            let mut out = stream.push(&input[..split]);
            out.push_str(&stream.push(&input[split..]));
            out.push_str(&stream.finish());
            assert_eq!(
                out, "\u{fffd} before [REDACTED] after\u{fffd}",
                "split {split}"
            );
        }
    }

    #[test]
    fn every_byte_split_is_redacted_including_utf8_and_short_tokens() {
        for secret in ["fixture-value-123456", "caf\u{e9}-\u{1f33b}-value", "4821"] {
            let input = format!("before {secret} after \u{1f33b}");
            for split in 0..=input.len() {
                let mut stream = Stream::new(vec![secret.into()]);
                let mut out = stream.push(&input.as_bytes()[..split]);
                out.push_str(&stream.push(&input.as_bytes()[split..]));
                out.push_str(&stream.finish());
                assert_eq!(out, "before [REDACTED] after \u{1f33b}", "split {split}");
                assert!(stream.finish().is_empty());
            }
        }
    }

    #[test]
    fn streaming_short_token_context_and_overlapping_values_match_whole_redaction() {
        let secrets = vec![
            "4821".into(),
            "fixture-long-value".into(),
            "long-value".into(),
        ];
        let text = "48210 14821 x4821 4821_ (4821) fixture-long-value fixture-long-value";
        for size in 1..=text.len() {
            let mut stream = Stream::new(secrets.clone());
            let mut out = String::new();
            for chunk in text.as_bytes().chunks(size) {
                out.push_str(&stream.push(chunk));
            }
            out.push_str(&stream.finish());
            assert_eq!(out, super::super::redact_known(text, &secrets));
        }
    }

    #[test]
    fn structured_text_including_keys_is_redacted_without_changing_scalars() {
        let secret = "fixture-value-123456";
        let mut details = serde_json::json!({secret: [{"diff": format!(" context {secret}\n-old\n+new"), "n": 7, "ok": true, "empty": null}]});
        value(&mut details, &[secret.into()], false);
        assert!(!details.to_string().contains(secret));
        assert_eq!(details["[REDACTED]"][0]["n"], 7);
        assert_eq!(details["[REDACTED]"][0]["ok"], true);
    }
}
