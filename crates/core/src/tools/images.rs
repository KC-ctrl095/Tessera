//! Pictures sent to models: what every provider accepts.
//!
//! An image stays in the conversation and goes with every later request, so
//! one a provider refuses breaks every turn after it, and a needlessly big
//! one slows each of them. Models look at no more than ~1,600–2,600 pixels
//! a side; larger pictures are only shrunk on their side.

use base64::Engine;

/// The largest picture sent: under 5 MB once base64-encoded, the limit of
/// Claude on Amazon Bedrock and Google Cloud (Anthropic's own API takes 10).
pub const MAX_BYTES: usize = 3_600_000;
/// Anthropic refuses pictures more than 8000 pixels on a side.
pub const MAX_SIDE: u32 = 8000;

/// What to do about a picture that is too big.
pub const SHRINK_HINT: &str = "Make a smaller copy and look at that instead, e.g. \
`magick in.png -resize '2000x2000>' -quality 85 small.jpg` (ImageMagick) or \
`ffmpeg -i in.png -vf \"scale='min(2000,iw)':-2\" small.jpg`.";

/// Why the picture in `bytes` can't be sent to a model as it is.
pub fn unfit(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_BYTES {
        return Some(format!(
            "it is {} and models take at most {} per picture",
            crate::util::human_bytes(bytes.len() as u64),
            crate::util::human_bytes(MAX_BYTES as u64)
        ));
    }
    match size(bytes) {
        Some((w, h)) if w.max(h) > MAX_SIDE => Some(format!(
            "it is {w}×{h} pixels and models take at most {MAX_SIDE} on a side"
        )),
        _ => None,
    }
}

/// [`unfit`] for base64 data, decoding only the header.
pub fn unfit_base64(data: &str) -> Option<String> {
    let data = data.trim();
    let padding = data.bytes().rev().take_while(|b| *b == b'=').count();
    let len = (data.len() / 4 * 3).saturating_sub(padding);
    if len > MAX_BYTES {
        return Some(format!(
            "it is {} and models take at most {} per picture",
            crate::util::human_bytes(len as u64),
            crate::util::human_bytes(MAX_BYTES as u64)
        ));
    }
    // JPEG headers can sit behind a large EXIF block.
    let head = &data.as_bytes()[..data.len().min(128 * 1024) / 4 * 4];
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(head)
        .ok()?;
    match size(&bytes) {
        Some((w, h)) if w.max(h) > MAX_SIDE => Some(format!(
            "it is {w}×{h} pixels and models take at most {MAX_SIDE} on a side"
        )),
        _ => None,
    }
}

/// Width and height of a PNG, JPEG, GIF or WebP picture, from its header.
pub fn size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le24 = |i: usize| {
        let s = b.get(i..i + 3)?;
        Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16) == Some(b"IHDR") {
        let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
        return Some((w, h));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let s = b.get(21..25)?;
                let w = 1 + (s[0] as u32 | ((s[1] as u32 & 0x3f) << 8));
                let h = 1 + ((s[1] as u32 >> 6) | (s[2] as u32) << 2 | ((s[3] as u32 & 0xf) << 10));
                Some((w, h))
            }
            b"VP8X" => Some((1 + le24(24)?, 1 + le24(27)?)),
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xff {
                return None;
            }
            let marker = b[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            // Start of frame (not DHT, JPG or DAC): height, then width.
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        // SOI, an APP0 segment, then SOF0.
        let mut b = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x06, b'J', b'F', b'I', b'F'];
        b.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b
    }

    #[test]
    fn sizes_come_from_the_header() {
        assert_eq!(size(&png(1920, 1080)), Some((1920, 1080)));
        assert_eq!(size(&jpeg(4032, 3024)), Some((4032, 3024)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xf0, 0x00]);
        assert_eq!(size(&gif), Some((320, 240)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend_from_slice(&[0x7f, 0x07, 0x00, 0x37, 0x04, 0x00]);
        assert_eq!(size(&webp), Some((1920, 1080)));
        assert_eq!(size(b"not a picture"), None);
    }

    #[test]
    fn too_big_or_too_wide_pictures_are_named() {
        assert!(unfit(&png(1920, 1080)).is_none());
        assert!(unfit(&png(9000, 400)).unwrap().contains("9000×400 pixels"));
        let big = vec![0u8; MAX_BYTES + 1];
        assert!(unfit(&big).unwrap().contains("at most"));
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        assert!(unfit_base64(&b64(&jpeg(1600, 1200))).is_none());
        assert!(
            unfit_base64(&b64(&jpeg(12000, 800)))
                .unwrap()
                .contains("12000×800")
        );
        assert!(unfit_base64(&b64(&big)).is_some());
    }
}
