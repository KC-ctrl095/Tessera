//! Just enough of WAV for voice: how long a recording is (to size the
//! speech model's work and its time limit), and plain 16-bit files for tests
//! and checks.

/// The PCM format and length of a WAV file, if it is one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WavInfo {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub data_bytes: u32,
}

impl WavInfo {
    pub fn seconds(&self) -> f64 {
        let frame = u64::from(self.channels) * u64::from(self.bits / 8);
        if self.rate == 0 || frame == 0 {
            return 0.0;
        }
        f64::from(self.data_bytes) / (frame as f64 * f64::from(self.rate))
    }
}

/// Reads the header of a RIFF/WAVE file (PCM or float samples).
pub fn info(bytes: &[u8]) -> Option<WavInfo> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?);
        let body = at + 8;
        if id == b"fmt " {
            if body + 16 > bytes.len() {
                return None;
            }
            let b = &bytes[body..body + 16];
            let format = u16::from_le_bytes([b[0], b[1]]);
            let channels = u16::from_le_bytes([b[2], b[3]]);
            let rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
            let bits = u16::from_le_bytes([b[14], b[15]]);
            fmt = Some((format, channels, rate, bits));
        } else if id == b"data" {
            let (format, channels, rate, bits) = fmt?;
            // 1 = PCM, 3 = float, 0xFFFE = extensible (PCM or float inside).
            if !matches!(format, 1 | 3 | 0xFFFE) || channels == 0 || bits < 8 {
                return None;
            }
            // Recorders that stream a WAV write 0 or u32::MAX as the length.
            let avail = (bytes.len() - body) as u32;
            let data_bytes = if len == 0 || len > avail { avail } else { len };
            return Some(WavInfo {
                rate,
                channels,
                bits,
                data_bytes,
            });
        }
        // Chunks are padded to an even length.
        at = body.checked_add(len as usize + (len as usize & 1))?;
    }
    None
}

/// A mono 16-bit PCM WAV file.
pub fn encode_pcm16(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_says_how_long_it_is() {
        let w = encode_pcm16(16_000, &vec![0i16; 24_000]);
        let i = info(&w).unwrap();
        assert_eq!((i.rate, i.channels, i.bits), (16_000, 1, 16));
        assert!((i.seconds() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn streamed_lengths_and_extra_chunks_are_handled() {
        let mut w = encode_pcm16(8_000, &vec![1i16; 8_000]);
        // A recorder that didn't know the length yet.
        w[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!((info(&w).unwrap().seconds() - 1.0).abs() < 1e-9);
        // A LIST chunk (odd length, padded) before the format.
        let mut x = b"RIFF\0\0\0\0WAVELIST\x03\0\0\0abc\0".to_vec();
        x.extend_from_slice(&encode_pcm16(8_000, &[0; 80])[12..]);
        assert!((info(&x).unwrap().seconds() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn other_files_are_not_wav() {
        assert_eq!(info(b"OggS\0\0\0\0\0\0\0\0\0\0"), None);
        assert_eq!(info(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(info(&[0x1a, 0x45, 0xdf, 0xa3]), None);
        // A data chunk without a format before it.
        assert_eq!(info(b"RIFF\0\0\0\0WAVEdata\x02\0\0\0\0\0"), None);
    }
}
