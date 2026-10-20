use base64::Engine;

/// Reject a full-view fallback or oversize native image. No page code is
/// involved: the native screenshot response supplies these bytes.
pub fn decode(
    json: &str,
    expected: (u32, u32),
    max_side: u32,
) -> Result<(String, u32, u32), String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let data = value
        .get("data")
        .and_then(|v| v.as_str())
        .ok_or("The snapshot had no picture.")?;
    if data.len() > 5_000_000 {
        return Err("The snapshot is too large.".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())?;
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16) != Some(b"IHDR") {
        return Err("The snapshot wasn't a PNG.".into());
    }
    let dimension = |at: usize| -> Result<u32, String> {
        let b = bytes
            .get(at..at + 4)
            .ok_or("The snapshot was incomplete.")?;
        Ok(u32::from_be_bytes(
            b.try_into().map_err(|_| "The snapshot was incomplete.")?,
        ))
    };
    let width = dimension(16)?;
    let height = dimension(20)?;
    if width == 0 || height == 0 || width > max_side || height > max_side {
        return Err("The snapshot's size wasn't safe.".into());
    }
    if (width, height) != expected {
        return Err("The snapshot didn't match the tile; nothing was shared.".into());
    }
    Ok((data.to_string(), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(w: u32, h: u32) -> String {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        serde_json::json!({"data": base64::engine::general_purpose::STANDARD.encode(b)}).to_string()
    }
    #[test]
    fn png_must_match_the_tile_not_the_whole_view() {
        assert!(decode(&response(320, 240), (320, 240), 1600).is_ok());
        for (w, h) in [
            (1280, 720),
            (0, 240),
            (1601, 240),
            (320, 241),
            (321, 240),
            (320, 242),
        ] {
            assert!(decode(&response(w, h), (320, 240), 1600).is_err());
        }
    }
    #[test]
    fn malformed_and_large_responses_are_refused() {
        for text in ["no json", "{}", r#"{"data":"%%%"}"#, r#"{"data":"YWJj"}"#] {
            assert!(decode(text, (1, 1), 1600).is_err());
        }
        let text = serde_json::json!({"data": "A".repeat(5_000_001)}).to_string();
        assert!(decode(&text, (1, 1), 1600).is_err());
    }
}
