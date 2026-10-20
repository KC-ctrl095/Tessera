//! "The agent sees what you see": a picture of one whiteboard tile, for an
//! agent's `board_snapshot_request` or the user's "Ask about this".
//!
//! Only the UI's own main frame can call this (board frames get no IPC
//! bridge, see vendor/README.md), and it only asks while the tile is on
//! screen. It passes the tile's content rect in CSS pixels; here the rect is
//! clamped to the web view and only the pixels inside it are copied, scaled
//! to at most `MAX_SIDE` on the long side. The picture goes straight back to
//! the UI: nothing is written to disk or kept.
//!
//! - Linux: WebKitGTK's snapshot of the visible part of the web view,
//!   cropped here.
//! - Windows: WebView2's native screenshot of only the visible rectangle.
//! - Android: our plugin copies just that rect of the window (PixelCopy).
//! - Elsewhere: `None` (not supported yet).

use serde::{Deserialize, Serialize};
#[cfg(any(windows, test))]
mod content;
#[cfg(windows)]
mod windows;

/// Long side of the picture, at most (vision models scale down anyway).
#[cfg_attr(not(any(target_os = "linux", windows, mobile)), allow(dead_code))]
pub const MAX_SIDE: u32 = 1600;
/// Above this, a PNG is swapped for a JPEG (photos, video frames).
#[cfg(target_os = "linux")]
const PNG_BUDGET: usize = 1_200_000;
/// The server's limit for a picture is 4 MB.
#[cfg(target_os = "linux")]
const MAX_BYTES: usize = 3_800_000;

/// What the UI asks for: a rect in CSS pixels and the viewport it is in.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// `window.innerWidth` and `innerHeight`: maps CSS pixels to the
    /// picture's pixels whatever the zoom and device scale.
    pub viewport_width: f64,
    pub viewport_height: f64,
}

/// A picture of the tile, as `SnapshotImage` in the protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shot {
    pub mime: String,
    /// Base64.
    pub data: String,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    fn check(&self) -> Result<(), String> {
        let all = [
            self.x,
            self.y,
            self.width,
            self.height,
            self.viewport_width,
            self.viewport_height,
        ];
        if all.iter().any(|v| !v.is_finite()) {
            return Err("invalid rect".into());
        }
        if self.viewport_width < 1.0 || self.viewport_height < 1.0 {
            return Err("invalid viewport".into());
        }
        if self.width < 1.0 || self.height < 1.0 {
            return Err("the tile is too small".into());
        }
        Ok(())
    }

    /// The rect in the pixels of a `px_w` × `px_h` picture of the whole
    /// viewport, clamped to it: `(x, y, w, h)`, or `None` when (almost)
    /// nothing of it is inside. Edges that fall inside a pixel round
    /// inwards, so no pixel from outside the tile is taken.
    // Desktop capture crops with it; phones send the whole picture.
    #[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
    pub fn in_pixels(self, px_w: u32, px_h: u32) -> Option<(u32, u32, u32, u32)> {
        let sx = f64::from(px_w) / self.viewport_width;
        let sy = f64::from(px_h) / self.viewport_height;
        // A little slack for float noise (100 × 2.0 = 200.0000001).
        let inner_start = |v: f64, max: u32| (v - 0.02).ceil().clamp(0.0, f64::from(max));
        let inner_end = |v: f64, max: u32| (v + 0.02).floor().clamp(0.0, f64::from(max));
        let x0 = inner_start(self.x * sx, px_w);
        let y0 = inner_start(self.y * sy, px_h);
        let x1 = inner_end((self.x + self.width) * sx, px_w);
        let y1 = inner_end((self.y + self.height) * sy, px_h);
        let (w, h) = (x1 - x0, y1 - y0);
        (w >= 8.0 && h >= 8.0).then_some((x0 as u32, y0 as u32, w as u32, h as u32))
    }
}

/// `w` × `h` scaled down (never up) so the long side is at most `max`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max || long == 0 {
        return (w, h);
    }
    let k = f64::from(max) / f64::from(long);
    let s = |v: u32| ((f64::from(v) * k).round() as u32).max(1);
    (s(w), s(h))
}

/// A picture of `rect` in the calling window's web view; `None` where this
/// platform can't take one.
#[tauri::command]
pub async fn board_capture(
    window: tauri::WebviewWindow,
    rect: Rect,
) -> Result<Option<Shot>, String> {
    rect.check()?;
    // A window that can't be seen shows nothing (the UI checks the tile).
    #[cfg(desktop)]
    if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
        return Err("Tessera isn't on screen".into());
    }
    capture(&window, rect).await
}

#[cfg(target_os = "linux")]
async fn capture(window: &tauri::WebviewWindow, rect: Rect) -> Result<Option<Shot>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |pw| {
            use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt};
            pw.inner().snapshot(
                SnapshotRegion::Visible,
                SnapshotOptions::NONE,
                None::<&webkit2gtk::gio::Cancellable>,
                move |res| {
                    let _ = tx.send(
                        res.map_err(|e| e.to_string())
                            .and_then(|s| linux::crop(s, rect)),
                    );
                },
            );
        })
        .map_err(|e| e.to_string())?;
    let raw = tokio::time::timeout(std::time::Duration::from_secs(8), rx)
        .await
        .map_err(|_| "the picture took too long".to_string())?
        .map_err(|_| "the picture was dropped".to_string())??;
    tauri::async_runtime::spawn_blocking(move || linux::encode(raw))
        .await
        .map_err(|e| e.to_string())?
        .map(Some)
}

#[cfg(mobile)]
async fn capture(window: &tauri::WebviewWindow, rect: Rect) -> Result<Option<Shot>, String> {
    use tauri::Manager;
    use tessera_background::BackgroundExt;
    let s = window
        .app_handle()
        .background()
        .capture(tessera_background::CaptureRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            viewport_width: rect.viewport_width,
            viewport_height: rect.viewport_height,
            max_side: MAX_SIDE,
        })
        .await?;
    Ok(Some(Shot {
        mime: s.mime,
        data: s.data,
        width: s.width,
        height: s.height,
    }))
}

#[cfg(windows)]
async fn capture(window: &tauri::WebviewWindow, rect: Rect) -> Result<Option<Shot>, String> {
    windows::capture(window, rect).await.map(Some)
}

#[cfg(not(any(target_os = "linux", windows, mobile)))]
async fn capture(_window: &tauri::WebviewWindow, _rect: Rect) -> Result<Option<Shot>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{MAX_BYTES, MAX_SIDE, PNG_BUDGET, Rect, Shot, fit};
    use base64::Engine as _;
    use gdk::cairo;
    use gdk::gdk_pixbuf::{Colorspace, Pixbuf};

    /// Pixels of the cropped, scaled picture (RGB, 3 bytes each).
    pub struct Raw {
        width: u32,
        height: u32,
        rgb: Vec<u8>,
    }

    /// Runs on the main thread, in WebKit's callback: copy out just the rect.
    pub fn crop(surface: cairo::Surface, rect: Rect) -> Result<Raw, String> {
        let shot = cairo::ImageSurface::try_from(surface)
            .map_err(|_| "unexpected snapshot surface".to_string())?;
        // Work in device pixels; the viewport size gives the scale.
        shot.set_device_scale(1.0, 1.0);
        let (pw, ph) = (shot.width().max(0) as u32, shot.height().max(0) as u32);
        let (x, y, w, h) = rect
            .in_pixels(pw, ph)
            .ok_or_else(|| "the tile isn't on screen".to_string())?;
        let (ow, oh) = fit(w, h, MAX_SIDE);
        let out = cairo::ImageSurface::create(cairo::Format::Rgb24, ow as i32, oh as i32)
            .map_err(|e| e.to_string())?;
        {
            // A sub-surface, so even the scaling filter never samples a
            // pixel outside the rect (its edges are padded instead).
            let tile = shot
                .create_for_rectangle(cairo::Rectangle::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(w),
                    f64::from(h),
                ))
                .map_err(|e| e.to_string())?;
            let cr = cairo::Context::new(&out).map_err(|e| e.to_string())?;
            cr.scale(f64::from(ow) / f64::from(w), f64::from(oh) / f64::from(h));
            cr.set_source_surface(&tile, 0.0, 0.0)
                .map_err(|e| e.to_string())?;
            cr.source().set_extend(cairo::Extend::Pad);
            cr.source().set_filter(cairo::Filter::Good);
            cr.paint().map_err(|e| e.to_string())?;
        }
        drop(shot);
        out.flush();
        let stride = out.stride().max(0) as usize;
        let mut out = out;
        let data = out.data().map_err(|e| e.to_string())?;
        let mut rgb = Vec::with_capacity(ow as usize * oh as usize * 3);
        for row in 0..oh as usize {
            let line = &data[row * stride..row * stride + ow as usize * 4];
            for px in line.as_chunks::<4>().0 {
                // RGB24 is a native-endian 0x00RRGGBB word.
                let v = u32::from_ne_bytes(*px);
                rgb.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
            }
        }
        Ok(Raw {
            width: ow,
            height: oh,
            rgb,
        })
    }

    /// PNG (crisp text), or JPEG when the PNG would be large.
    pub fn encode(raw: Raw) -> Result<Shot, String> {
        let (w, h) = (raw.width, raw.height);
        let pixbuf = Pixbuf::from_mut_slice(
            raw.rgb,
            Colorspace::Rgb,
            false,
            8,
            w as i32,
            h as i32,
            w as i32 * 3,
        );
        let png = pixbuf
            .save_to_bufferv("png", &[])
            .map_err(|e| e.to_string())?;
        let (mime, bytes) = if png.len() > PNG_BUDGET {
            let jpeg = pixbuf
                .save_to_bufferv("jpeg", &[("quality", "85")])
                .map_err(|e| e.to_string())?;
            if jpeg.len() < png.len() {
                ("image/jpeg", jpeg)
            } else {
                ("image/png", png)
            }
        } else {
            ("image/png", png)
        };
        // The server takes up to 4 MB; a busy page at full size can be more.
        let (mime, bytes) = if bytes.len() > MAX_BYTES {
            let small = pixbuf
                .save_to_bufferv("jpeg", &[("quality", "60")])
                .map_err(|e| e.to_string())?;
            if small.len() > MAX_BYTES {
                return Err("the picture is too big".into());
            }
            ("image/jpeg", small)
        } else {
            (mime, bytes)
        };
        Ok(Shot {
            mime: mime.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            width: w,
            height: h,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
            viewport_width: 1000.0,
            viewport_height: 800.0,
        }
    }

    #[test]
    fn the_rect_scales_with_the_device() {
        // A 2× screen: the picture of the viewport is 2000 × 1600.
        assert_eq!(
            rect(100.0, 50.0, 400.0, 300.0).in_pixels(2000, 1600),
            Some((200, 100, 800, 600))
        );
        // Page zoom or fractional scale: edges inside a pixel round inwards.
        assert_eq!(
            rect(100.0, 50.0, 400.0, 300.0).in_pixels(1250, 1000),
            Some((125, 63, 500, 374))
        );
        assert_eq!(
            rect(423.9, 10.2, 200.0, 100.0).in_pixels(2625, 2100),
            Some((1113, 27, 524, 262))
        );
    }

    #[test]
    fn nothing_outside_the_viewport_is_taken() {
        // Partly off screen (a tile mid-swipe): only the visible part.
        assert_eq!(
            rect(-200.0, 700.0, 500.0, 300.0).in_pixels(1000, 800),
            Some((0, 700, 300, 100))
        );
        // Entirely off screen, or a sliver: nothing.
        assert_eq!(rect(1200.0, 0.0, 300.0, 300.0).in_pixels(1000, 800), None);
        assert_eq!(rect(995.0, 0.0, 300.0, 300.0).in_pixels(1000, 800), None);
    }

    #[test]
    fn bad_rects_are_refused() {
        assert!(rect(0.0, 0.0, 10.0, 10.0).check().is_ok());
        assert!(rect(f64::NAN, 0.0, 10.0, 10.0).check().is_err());
        assert!(rect(0.0, 0.0, 0.0, 10.0).check().is_err());
        assert!(
            Rect {
                viewport_width: 0.0,
                ..rect(0.0, 0.0, 10.0, 10.0)
            }
            .check()
            .is_err()
        );
    }

    #[test]
    fn pictures_fit_in_the_long_side() {
        assert_eq!(fit(800, 600, MAX_SIDE), (800, 600));
        assert_eq!(fit(3200, 1800, MAX_SIDE), (1600, 900));
        assert_eq!(fit(1000, 4000, MAX_SIDE), (400, 1600));
        assert_eq!(fit(5000, 1, MAX_SIDE), (1600, 1));
    }
}
