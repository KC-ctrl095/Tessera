//! Capture only the already displayed app rectangle, through WebView2's
//! native DevTools API. No page script, screen grabbing, network listener,
//! permission change or browser profile access.

use ::windows::core::{HSTRING, PCWSTR};
use tauri::WebviewWindow;
use webview2_com::CallDevToolsProtocolMethodCompletedHandler;

pub(super) async fn capture(
    window: &WebviewWindow,
    rect: super::Rect,
) -> Result<super::Shot, String> {
    let view = window.inner_size().map_err(|e| e.to_string())?;
    let dpr = view.width as f64 / rect.viewport_width;
    let dy = view.height as f64 / rect.viewport_height;
    if (dpr - dy).abs() > 0.03 {
        return Err("The window changed size; try again.".into());
    }
    let (px, py, pw, ph) = rect
        .in_pixels(view.width, view.height)
        .ok_or("The tile is outside the window.")?;
    let (x, y, width, height) = (
        px as f64 / dpr,
        py as f64 / dpr,
        pw as f64 / dpr,
        ph as f64 / dpr,
    );
    let scale = (super::MAX_SIDE as f64 / (pw.max(ph) as f64)).min(1.0);
    let params = serde_json::json!({
        "format": "png", "fromSurface": true, "captureBeyondViewport": false,
        "clip": { "x": x, "y": y, "width": width, "height": height, "scale": scale }
    })
    .to_string();
    let expected = (
        (pw as f64 * scale).round() as u32,
        (ph as f64 * scale).round() as u32,
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |webview| {
            let tx = std::sync::Arc::new(parking_lot::Mutex::new(Some(tx)));
            let tx2 = tx.clone();
            let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                move |status, json| {
                    let result = status
                        .map_err(|e| e.to_string())
                        .and_then(|()| decode(&json, expected));
                    if let Some(tx) = tx2.lock().take() {
                        let _ = tx.send(result);
                    }
                    Ok(())
                },
            ));
            // SAFETY: called on the WebView2 owner thread; arguments stay alive
            // through the synchronous COM call, and it owns the completion handler.
            let result = unsafe {
                webview.controller().CoreWebView2().and_then(|view| {
                    view.CallDevToolsProtocolMethod(
                        PCWSTR(HSTRING::from("Page.captureScreenshot").as_ptr()),
                        PCWSTR(HSTRING::from(params).as_ptr()),
                        &done,
                    )
                })
            };
            if let Err(e) = result
                && let Some(tx) = tx.lock().take()
            {
                let _ = tx.send(Err(e.to_string()));
            }
        })
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(10), rx)
        .await
        .map_err(|_| "The snapshot took too long.".to_string())?
        .map_err(|_| "The snapshot was cancelled.".to_string())?
}

fn decode(json: &str, expected: (u32, u32)) -> Result<super::Shot, String> {
    let (data, width, height) = super::content::decode(json, expected, super::MAX_SIDE)?;
    Ok(super::Shot {
        mime: "image/png".into(),
        data,
        width,
        height,
    })
}
